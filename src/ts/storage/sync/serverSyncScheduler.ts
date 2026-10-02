export interface ServerSyncSchedulerDependencies {
    push(): Promise<void>
    pull(completeAvailable?: boolean): Promise<void>
    connect(): Promise<void>
    disconnect(): Promise<void>
    retryClock?(): Promise<void>
    failed(error: unknown): void
}

const integrityCodes = ['clock-skew', 'incoming-clock-skew', 'accepted-clock-correction-required', 'writer-collision', 'equal-stamp-integrity', 'server-epoch-changed', 'unauthorized', 'invalid-device-token']
export function serverSyncErrorCode(error: unknown): string {
    if (typeof error !== 'object' || !error) return ''
    if ('message' in error && typeof error.message === 'string' && integrityCodes.includes(error.message)) return error.message
    return 'code' in error ? String(error.code) : ''
}

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
    const disconnect = () => {
        if (!disconnecting) disconnecting = dependencies.disconnect().finally(() => { disconnecting = undefined })
        return disconnecting
    }
    const code = serverSyncErrorCode
    const fail = (error: unknown) => {
        if (blocked && blockedCode !== 'clock-skew') return
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
        sending = dependencies.push().then(() => { sendRetryAttempt = 0 }).catch(error => {
            fail(error)
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
        receiving = dependencies.pull(completeAvailable).catch(error => { fail(error); throw error }).finally(() => {
            receiving = undefined
            if (pullAgain && foreground) { pullAgain = false; void pull().catch(() => {}) }
            else schedulePull()
        })
        return receiving
    }
    const connect = () => {
        if (!foreground || blocked) return
        void dependencies.connect().catch(error => { fail(error); socket(false) })
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
    const recheckClock = (): Promise<boolean> => {
        if (!foreground || !blocked || blockedCode !== 'clock-skew' || !dependencies.retryClock) return Promise.resolve(false)
        if (checkingClock) return checkingClock
        checkingClock = (async () => {
            await Promise.allSettled([sending, receiving, disconnecting])
            if (!foreground || blockedCode !== 'clock-skew') return false
            await dependencies.retryClock!()
            if (!foreground || blockedCode !== 'clock-skew') return false
            blocked = false; blockedCode = ''; sendRetryAttempt = 0
            connect(); void pull().catch(() => {}); void push().catch(() => {})
            return true
        })().catch(error => { fail(error); return false }).finally(() => { checkingClock = undefined })
        return checkingClock
    }
    return {
        async foreground(value: boolean) {
            if (foreground === value) { if (value) await recheckClock(); return }
            foreground = value
            clear()
            if (value) { reconnectAttempt = 0; if (!await recheckClock()) { connect(); void pull().catch(() => {}); void push().catch(() => {}) } }
            else { connected = false; await disconnect() }
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
        reportFailure: fail,
        isBlocked: () => blocked,
        isRunning: () => !!sending || !!receiving || !!checkingClock,
        async fence() { foreground = false; pushAgain = false; pullAgain = false; clear(); await disconnect(); await Promise.allSettled([sending, receiving, checkingClock]) },
        dispose() { foreground = false; clear(); void disconnect().catch(fail) },
    }
}
