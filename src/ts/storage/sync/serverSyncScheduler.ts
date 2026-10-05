export interface ServerSyncSchedulerDependencies {
    push(): Promise<void>
    pull(completeAvailable?: boolean): Promise<void>
    connect(): Promise<void>
    disconnect(): Promise<void>
    retryClock?(): Promise<void>
    publishHidden?(): Promise<void>
    failed(error: unknown): void
    recovered?(): void
}

export const integrityCodes = ['clock-skew', 'incoming-clock-skew', 'accepted-clock-correction-required', 'writer-collision', 'equal-stamp-integrity', 'server-epoch-changed', 'unauthorized', 'invalid-device-token']
// A new clock sample can clear these, so they are rechecked instead of waiting for a manual retry.
const clockCodes = ['clock-skew', 'incoming-clock-skew']
export function serverSyncErrorCode(error: unknown): string {
    if (typeof error !== 'object' || !error) return ''
    if ('message' in error && typeof error.message === 'string' && integrityCodes.includes(error.message)) return error.message
    return 'code' in error ? String(error.code) : ''
}
const isCancellation = (error: unknown) => serverSyncErrorCode(error) === 'cancelled' || (typeof error === 'object' && !!error && 'name' in error && error.name === 'AbortError')
type Lane = 'push' | 'pull' | 'connect'

export function createServerSyncScheduler(dependencies: ServerSyncSchedulerDependencies) {
    let foreground = false
    let connected = false
    let pushTimer: ReturnType<typeof setTimeout> | undefined
    let pullTimer: ReturnType<typeof setTimeout> | undefined
    let retryTimer: ReturnType<typeof setTimeout> | undefined
    let sendRetryTimer: ReturnType<typeof setTimeout> | undefined
    let sending: Promise<void> | undefined
    let receiving: Promise<void> | undefined
    let pushAgain = false
    let pullAgain = false
    let reconnectAttempt = 0
    let sendRetryAttempt = 0
    let blocked = false
    let blockedCode = ''
    let checkingClock: Promise<boolean> | undefined
    let disconnecting: Promise<void> | undefined
    let connecting: Promise<void> | undefined
    const failedLanes = new Set<Lane>()
    const disconnect = () => {
        if (!disconnecting) disconnecting = (async () => {
            await dependencies.disconnect()
            const started = connecting
            // A start that was already running may finish after the stop, so stop again once it settles.
            if (started) { await started; await dependencies.disconnect() }
        })().finally(() => { disconnecting = undefined })
        return disconnecting
    }
    const code = serverSyncErrorCode
    const succeeded = (lane: Lane) => { if (failedLanes.delete(lane) && failedLanes.size === 0 && !blocked) dependencies.recovered?.() }
    const fail = (error: unknown, lane?: Lane) => {
        if (isCancellation(error)) return
        if (blocked && !clockCodes.includes(blockedCode)) return
        if (lane) failedLanes.add(lane)
        if ((typeof error === 'object' && error && 'retryable' in error && error.retryable === false && code(error) !== 'server-unreachable') || integrityCodes.includes(code(error))) {
            blocked = true
            blockedCode = code(error)
            clear()
            void disconnect().catch(() => {})
        }
        dependencies.failed(error)
    }
    const clear = () => { clearTimeout(pushTimer); clearTimeout(pullTimer); clearTimeout(retryTimer); clearTimeout(sendRetryTimer) }
    const schedulePull = () => {
        clearTimeout(pullTimer)
        if (foreground && !blocked) pullTimer = setTimeout(() => { void pull().catch(() => {}) }, connected ? 60_000 : 5_000)
    }
    const push = (): Promise<void> => {
        clearTimeout(pushTimer)
        clearTimeout(sendRetryTimer)
        if (!foreground || blocked) return Promise.resolve()
        if (sending) { pushAgain = true; return sending }
        sending = dependencies.push().then(() => { sendRetryAttempt = 0; succeeded('push') }).catch(error => {
            fail(error, 'push')
            if (foreground && !blocked) sendRetryTimer = setTimeout(() => { void push().catch(() => {}) }, Math.min(30_000, 1000 * 2 ** sendRetryAttempt++))
            throw error
        }).finally(() => {
            sending = undefined
            if (pushAgain && foreground) { pushAgain = false; void push().catch(() => {}) }
        })
        return sending
    }
    const pull = (completeAvailable = false): Promise<void> => {
        clearTimeout(pullTimer)
        if ((!foreground && !completeAvailable) || blocked) return Promise.resolve()
        if (receiving) { pullAgain = true; return receiving }
        receiving = dependencies.pull(completeAvailable).then(() => succeeded('pull'), error => { fail(error, 'pull'); throw error }).finally(() => {
            receiving = undefined
            if (pullAgain && foreground) { pullAgain = false; void pull().catch(() => {}) }
            else schedulePull()
        })
        return receiving
    }
    const connect = () => {
        if (!foreground || blocked || connecting) return
        const attempt: Promise<void> = dependencies.connect().then(() => succeeded('connect'), error => { fail(error, 'connect'); socket(false) })
            .finally(() => { if (connecting === attempt) connecting = undefined })
        connecting = attempt
    }
    const socket = (value: boolean) => {
        connected = value
        clearTimeout(retryTimer)
        if (value) reconnectAttempt = 0
        else if (foreground && !blocked) {
            void pull().catch(() => {})
            retryTimer = setTimeout(connect, Math.min(30_000, 1000 * 2 ** reconnectAttempt++))
        }
        schedulePull()
    }
    // Pending work goes out before the app may be suspended. A foreground push started meanwhile waits for it.
    const publishHidden = async () => {
        await sending?.catch(() => {})
        if (foreground || blocked || sending) return
        pushAgain = false
        sending = dependencies.publishHidden!().then(() => { sendRetryAttempt = 0; succeeded('push') }, error => { fail(error, 'push') }).finally(() => {
            sending = undefined
            if (pushAgain && foreground) { pushAgain = false; void push().catch(() => {}) }
        })
        await sending
    }
    const recheckClock = (): Promise<boolean> => {
        if (!foreground || !blocked || !clockCodes.includes(blockedCode) || !dependencies.retryClock) return Promise.resolve(false)
        if (checkingClock) return checkingClock
        checkingClock = (async () => {
            await Promise.allSettled([sending, receiving, disconnecting])
            if (!foreground || !clockCodes.includes(blockedCode)) return false
            await dependencies.retryClock!()
            if (!foreground || !clockCodes.includes(blockedCode)) return false
            blocked = false; blockedCode = ''; sendRetryAttempt = 0
            connect(); void pull().catch(() => {}); void push().catch(() => {})
            return true
        })().catch(error => { fail(error); return false }).finally(() => { checkingClock = undefined })
        return checkingClock
    }
    return {
        async foreground(value: boolean, publishPending = false) {
            if (foreground === value) { if (value) await recheckClock(); return }
            foreground = value
            clear()
            if (value) { reconnectAttempt = 0; if (!await recheckClock()) { connect(); void pull().catch(() => {}); void push().catch(() => {}) } }
            else {
                connected = false
                if (publishPending && !blocked && dependencies.publishHidden) await publishHidden()
                if (!foreground) await disconnect()
            }
        },
        localChange(immediate = false) {
            clearTimeout(pushTimer)
            if (!foreground || blocked) return
            if (immediate) void push().catch(() => {})
            else pushTimer = setTimeout(() => { void push().catch(() => {}) }, 2000)
        },
        remoteHint: () => { void pull().catch(() => {}) },
        async conversationOpened() { if (!await recheckClock()) void pull().catch(() => {}) },
        socket,
        flush: push,
        async receiveAvailableChanges() {
            if (blocked) throw new Error('Sync is stopped')
            await receiving
            if (blocked) throw new Error('Sync is stopped')
            await pull(true)
        },
        retry() { blocked = false; blockedCode = ''; sendRetryAttempt = 0; connect(); void pull().catch(() => {}); return push() },
        reset() { blocked = false; blockedCode = ''; sendRetryAttempt = 0; reconnectAttempt = 0; failedLanes.clear() },
        reportFailure: fail,
        isBlocked: () => blocked,
        isRunning: () => !!sending || !!receiving || !!checkingClock,
        async fence() { foreground = false; pushAgain = false; pullAgain = false; clear(); await disconnect(); await Promise.allSettled([sending, receiving, checkingClock]) },
        dispose() { foreground = false; clear(); void disconnect().catch(fail) },
    }
}
