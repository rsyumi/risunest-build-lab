export type LwwOperation = 'publish' | 'receive' | 'maintenance'
export interface LwwCall { signal: AbortSignal; turnLimit?: number }
export interface LwwTurn { count: number; more: boolean }
export interface LwwSchedulerDependencies {
    available(): boolean
    publish(call: LwwCall): Promise<LwwTurn>
    receive(call: LwwCall): Promise<LwwTurn>
    maintain?(call: LwwCall): Promise<void>
    failed(error: unknown, operation: LwwOperation): void
    succeeded?(operation: LwwOperation): void
    now?(): number
    wallNow?(): number
    turnLimit?: number
    setTimer?(callback: () => void, delay: number): unknown
    clearTimer?(timer: unknown): void
}
type ExplicitKind = 'publish' | 'receive' | 'manual'
interface Waiting {
    kind: ExplicitKind
    promise: Promise<void>
    resolve(): void
    reject(error: unknown): void
    controller: AbortController
    participants: number
    unbounded: boolean
    publish?: (call: LwwCall) => Promise<LwwTurn>
}

export function createLwwScheduler(dependencies: LwwSchedulerDependencies) {
    const now = dependencies.now ?? (() => performance.now())
    const wallNow = dependencies.wallNow ?? Date.now
    const setTimer = dependencies.setTimer ?? ((callback, delay) => setTimeout(callback, delay))
    const clearTimer = dependencies.clearTimer ?? (timer => clearTimeout(timer as ReturnType<typeof setTimeout>))
    const limit = dependencies.turnLimit ?? 4
    let enabled = false
    let epoch = 0
    let lifetime = new AbortController()
    let dirtyVersion = 0
    let coveredVersion = 0
    let firstDirty: number | undefined
    let lastDirty = 0
    let immediate = false
    let receivePending = false
    let nextDirection: 'publish' | 'receive' = 'publish'
    let emptyListings = 0
    let wakeTimer: unknown
    let receiveTimer: unknown
    let maintenanceTimer: unknown
    let inFlight: Promise<void> | undefined
    let maintenance: Promise<void> | undefined
    let activeWaiting: Waiting | undefined
    let quiescence: { promise: Promise<void>; resolve(): void } | undefined
    const waiting = new Map<ExplicitKind, Waiting>()
    const retryAt: Record<LwwOperation, number> = { publish: 0, receive: 0, maintenance: 0 }
    const serviceRetryAt: Record<LwwOperation, number> = { publish: 0, receive: 0, maintenance: 0 }
    const blocked = new Set<LwwOperation>()
    const failureByOperation = new Map<LwwOperation, unknown>()
    const checkSettled = () => {
        if (!inFlight && !maintenance && !waiting.size) { quiescence?.resolve(); quiescence = undefined }
    }
    const aborted = () => new DOMException('Sync scheduling stopped', 'AbortError')
    const clearWake = () => { if (wakeTimer !== undefined) clearTimer(wakeTimer); wakeTimer = undefined }
    const clearReceive = () => { if (receiveTimer !== undefined) clearTimer(receiveTimer); receiveTimer = undefined }
    const clearMaintenance = () => { if (maintenanceTimer !== undefined) clearTimer(maintenanceTimer); maintenanceTimer = undefined }
    const hasPublish = () => dirtyVersion !== coveredVersion
    const sendDue = () => Math.max(retryAt.publish, immediate ? 0 : Math.min(lastDirty + 15_000, (firstDirty ?? now()) + 60_000))
    const rememberFailure = (error: unknown, operation: LwwOperation) => {
        const failure = error as { kind?: string; code?: string; retryAtMs?: string | number; retryable?: boolean; action?: string }
        const kind = failure?.kind ?? failure?.code
        const deadline = Number(failure?.retryAtMs)
        if (Number.isFinite(deadline)) serviceRetryAt[operation] = Math.max(serviceRetryAt[operation], now() + Math.max(0, deadline - wallNow()))
        retryAt[operation] = Math.max(retryAt[operation], serviceRetryAt[operation],
            now() + (operation === 'publish' ? 15_000 : operation === 'receive' ? 20_000 : 60_000))
        if (kind !== 'clockSkew' && kind !== 'cancelled' && (failure?.retryable === false || ['reauthenticate', 'unlock-key', 'free-space', 'check-endpoint'].includes(failure?.action ?? '')
            || ['unauthorized', 'reauthRequired', 'deviceVaultUnavailable', 'repositoryKeyUnavailable', 'localStorageFull', 'localPermissionDenied', 'storageFull', 'endpointRejected', 'corrupt', 'repositoryMismatch'].includes(kind ?? ''))) blocked.add(operation)
        failureByOperation.set(operation, error)
        dependencies.failed(error, operation)
    }
    const scheduleReceive = () => {
        clearReceive()
        if (!enabled) return
        receiveTimer = setTimer(() => { receiveTimer = undefined; receivePending = true; pump() }, emptyListings >= 3 ? 60_000 : 20_000)
    }
    const resetListing = () => {
        const backedOff = emptyListings >= 3
        emptyListings = 0
        if (backedOff && receiveTimer !== undefined) scheduleReceive()
    }
    const scheduleMaintenance = () => {
        if (!enabled || !dependencies.maintain || maintenanceTimer !== undefined || maintenance || blocked.has('maintenance')) return
        maintenanceTimer = setTimer(() => {
            maintenanceTimer = undefined
            if (!enabled) return
            if (!dependencies.available() || hasPublish()) { scheduleMaintenance(); return }
            const captured = epoch
            const signal = lifetime.signal
            maintenance = Promise.resolve().then(() => dependencies.maintain!({ signal })).then(() => {
                if (captured === epoch) dependencies.succeeded?.('maintenance')
            }, error => { if (captured === epoch && !signal.aborted) rememberFailure(error, 'maintenance') }).finally(() => {
                maintenance = undefined; scheduleMaintenance(); checkSettled()
            })
        }, Math.max(60_000, retryAt.maintenance - now()))
    }
    const execute = async (operation: 'publish' | 'receive', routine: boolean, signal: AbortSignal, captured: number, publish?: (call: LwwCall) => Promise<LwwTurn>) => {
        signal.throwIfAborted()
        const version = dirtyVersion
        if (operation === 'publish') { firstDirty = undefined; immediate = false }
        else { receivePending = false; clearReceive() }
        try {
            const result = await (operation === 'publish' && publish ? publish : dependencies[operation])({ signal, ...(routine ? { turnLimit: limit } : {}) })
            signal.throwIfAborted()
            if (captured !== epoch) throw aborted()
            failureByOperation.delete(operation)
            dependencies.succeeded?.(operation)
            if (operation === 'publish') {
                coveredVersion = version
                if (result.more) {
                    dirtyVersion++
                    firstDirty ??= now()
                    lastDirty = now()
                    // Generating-only outboxes cannot make progress until a later generation event.
                    immediate ||= result.count > 0
                } else if (!hasPublish()) firstDirty = undefined
            } else {
                receivePending ||= result.more
                emptyListings = result.more || result.count ? 0 : emptyListings + 1
                if (result.more && !result.count) retryAt.receive = Math.max(retryAt.receive, now() + 20_000)
                scheduleReceive()
            }
        } catch (error) {
            if (captured === epoch && !signal.aborted) {
                if (operation === 'publish') { firstDirty ??= now(); immediate = true }
                else receivePending = true
                rememberFailure(error, operation)
            }
            throw error
        }
    }
    const explicitDue = (item: Waiting) => item.kind === 'manual' ? Math.max(serviceRetryAt.publish, serviceRetryAt.receive) : serviceRetryAt[item.kind]
    const pump = () => {
        clearWake()
        if (inFlight) return
        const pendingExplicit = waiting.get('manual') ?? waiting.get('publish') ?? waiting.get('receive')
        const explicit = pendingExplicit && explicitDue(pendingExplicit) <= now() ? pendingExplicit : undefined
        const routine = enabled && dependencies.available()
        const publishDue = routine && hasPublish() && !blocked.has('publish') ? sendDue() : Infinity
        const receiveDue = routine && receivePending && !blocked.has('receive') ? retryAt.receive : Infinity
        let operation: 'publish' | 'receive' | undefined
        if (!explicit) {
            if (publishDue <= now() && receiveDue <= now()) operation = nextDirection
            else if (publishDue <= now()) operation = 'publish'
            else if (receiveDue <= now()) operation = 'receive'
        }
        if ((!explicit || explicitDue(explicit) > now()) && !operation) {
            const due = Math.min(pendingExplicit ? explicitDue(pendingExplicit) : Infinity, publishDue, receiveDue)
            if (Number.isFinite(due)) wakeTimer = setTimer(() => { wakeTimer = undefined; pump() }, Math.max(0, due - now()))
            return
        }
        const captured = epoch
        const signal = explicit?.controller.signal ?? lifetime.signal
        if (explicit) { waiting.delete(explicit.kind); activeWaiting = explicit }
        else nextDirection = operation === 'publish' ? 'receive' : 'publish'
        inFlight = Promise.resolve().then(async () => {
            if (captured !== epoch) throw aborted()
            signal.throwIfAborted()
            if (explicit?.kind === 'manual') {
                await execute('publish', false, signal, captured)
                await execute('receive', false, signal, captured)
            } else await execute(explicit?.kind ?? operation!, !explicit, signal, captured, explicit?.publish)
            explicit?.resolve()
        }).catch(error => { explicit?.reject(error) }).finally(() => {
            activeWaiting = undefined
            inFlight = undefined
            pump(); scheduleMaintenance(); checkSettled()
        })
    }
    const join = (item: Waiting, signal?: AbortSignal): Promise<void> => {
        if (!signal) { item.unbounded = true; return item.promise }
        item.participants++
        return new Promise<void>((resolve, reject) => {
            let finished = false
            const finish = (error?: unknown) => {
                if (finished) return
                finished = true
                signal.removeEventListener('abort', cancel)
                item.participants--
                if (error === undefined) resolve()
                else reject(error)
            }
            const cancel = () => {
                finish(signal.reason ?? aborted())
                if (!item.participants && !item.unbounded) {
                    item.controller.abort(signal.reason ?? aborted())
                    item.reject(item.controller.signal.reason)
                    if (waiting.get(item.kind) === item) { waiting.delete(item.kind); pump(); checkSettled() }
                }
            }
            signal.addEventListener('abort', cancel, { once: true })
            void item.promise.then(() => finish(), error => finish(error))
        })
    }
    const requestExplicit = (kind: ExplicitKind, signal?: AbortSignal, publish?: (call: LwwCall) => Promise<LwwTurn>): Promise<void> => {
        if (signal?.aborted) return Promise.reject(signal.reason ?? aborted())
        if (kind !== 'manual' && blocked.has(kind)) return Promise.reject(failureByOperation.get(kind))
        const existing = waiting.get(kind)
        if (existing) return join(existing, signal)
        const controller = new AbortController()
        let resolve!: () => void
        let reject!: (error: unknown) => void
        const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no })
        const item: Waiting = { kind, promise, resolve, reject, controller, publish, participants: 0, unbounded: false }
        const caller = join(item, signal)
        waiting.set(kind, item)
        if (kind === 'manual') { blocked.delete('publish'); blocked.delete('receive') }
        pump()
        return caller
    }
    const settled = (): Promise<void> => {
        if (!inFlight && !maintenance && !waiting.size) return Promise.resolve()
        if (!quiescence) {
            let resolve!: () => void
            const promise = new Promise<void>(yes => { resolve = yes })
            quiescence = { promise, resolve }
        }
        return quiescence.promise
    }
    const dirty = (generationComplete = false) => {
        if (!enabled) return
        dirtyVersion++
        firstDirty ??= now()
        lastDirty = now()
        immediate ||= generationComplete
        resetListing(); pump()
    }
    const receiveNow = (force = false, signal?: AbortSignal) => {
        if (force) return requestExplicit('receive', signal)
        if (!enabled || !dependencies.available()) return Promise.resolve()
        clearReceive(); receivePending = true; pump()
        return settled()
    }
    const start = (publish = true) => {
        if (enabled) return
        enabled = true; epoch++; lifetime = new AbortController(); emptyListings = 0; nextDirection = 'publish'
        receivePending = true
        if (publish) { dirtyVersion++; firstDirty = now(); lastDirty = now(); immediate = true }
        pump(); scheduleMaintenance()
    }
    const pauseAutomatic = () => {
        enabled = false
        clearWake(); clearReceive(); clearMaintenance()
        coveredVersion = dirtyVersion; firstDirty = undefined; immediate = false; receivePending = false
    }
    return {
        dirty,
        start,
        pauseAutomatic() { pauseAutomatic(); pump() },
        stop() {
            pauseAutomatic(); epoch++; lifetime.abort(aborted())
            for (const item of waiting.values()) { item.controller.abort(aborted()); item.reject(aborted()) }
            waiting.clear()
            activeWaiting?.controller.abort(aborted()); activeWaiting?.reject(aborted())
            checkSettled()
        },
        resumeForeground(publish = true) {
            if (!enabled) start(publish)
            else { resetListing(); if (publish) dirty(true); receivePending = true; pump() }
            return settled()
        },
        conversationOpened() { resetListing(); return receiveNow() },
        publishNow(force = false, signal?: AbortSignal, publish?: (call: LwwCall) => Promise<LwwTurn>) {
            if (force) return requestExplicit('publish', signal, publish)
            dirty(true); return settled()
        },
        receiveNow,
        manualNow: (signal?: AbortSignal) => requestExplicit('manual', signal),
        recovered() { blocked.clear(); pump(); scheduleMaintenance() },
        settled,
    }
}
