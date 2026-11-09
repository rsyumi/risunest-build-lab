import type { ContentChangeKey } from './persistentDataStore'

// Guards the conversation a native generation writes, from install until its
// committed state is adopted. Install, reinstall and the hold checks are
// synchronous so they can run inside the save queue; a held caller leaves the
// queue and awaits `settled` outside it. `settle`, `retryAdoption` and `close`
// await adoption, which the save queue serializes, so they run outside it.

/** The native ticket as the command DTOs carry it; both parts stay decimal text. */
export interface NativeGenerationGuardTicket {
    readonly epoch: string
    readonly sequence: string
}

export interface NativeGenerationGuardTarget {
    readonly characterId: string
    readonly conversationId: string
}

export type NativeGenerationGuardPhase =
    | 'reserved' | 'running' | 'completed' | 'cancelled' | 'stale' | 'interrupted' | 'failed'

/** The status fields the guard reads; the full command status is assignable to it. */
export interface NativeGenerationGuardStatus {
    readonly ticket: NativeGenerationGuardTicket
    readonly phase: NativeGenerationGuardPhase
    readonly pendingCommit: boolean
    readonly output: 'none' | 'committed' | 'unavailable'
}

export type NativeGenerationWriteSource = 'edit' | 'plugin'
export type NativeGenerationGuardCancelReason = NativeGenerationWriteSource | 'lifecycle'

/** The parts of a working-set commit or unit intent that can reach a guarded target. */
export interface NativeGenerationTargetWrite {
    readonly unitMutations?: readonly { readonly key: string }[]
    readonly root?: unknown
    readonly rootMutations?: readonly { readonly key: string }[]
    readonly character?: { readonly chaId: string }
    readonly characterDetails?: readonly { readonly chaId: string }[]
    readonly replaceCharacter?: { readonly chaId: string }
    readonly addCharacter?: { readonly chaId: string }
    readonly conversations?: readonly {
        readonly type: string
        readonly characterId: string
        readonly conversationId?: string
    }[]
    readonly deleteCharacterIds?: readonly string[]
}

export type NativeGenerationHoldOutcome = 'released' | 'fenced'

export interface NativeGenerationHold {
    readonly tickets: readonly NativeGenerationGuardTicket[]
    /** Resolves, never rejects, once every held ticket is released or fenced. */
    readonly settled: Promise<NativeGenerationHoldOutcome>
}

export type NativeGenerationSettlement = NativeGenerationHoldOutcome | 'pending' | 'unknown'

export type NativeGenerationFenceReason = 'adoption-failed' | 'output-unavailable' | 'close-timeout'

export interface NativeGenerationFenceEvent<S extends NativeGenerationGuardStatus> {
    readonly ticket: NativeGenerationGuardTicket
    readonly target: NativeGenerationGuardTarget
    /** The last status the guard saw, or null when none arrived before the fence. */
    readonly status: S | null
    readonly reason: NativeGenerationFenceReason
    /** The adoption failure for `adoption-failed`. */
    readonly error?: unknown
}

export interface NativeGenerationGuardClock {
    setTimeout(callback: () => void, delay: number): unknown
    clearTimeout(handle: unknown): void
}

export interface NativeGenerationGuardHooks<S extends NativeGenerationGuardStatus> {
    /** Asks native to cancel the ticket. The guard calls it at most once per ticket. */
    cancel(ticket: NativeGenerationGuardTicket, reason: NativeGenerationGuardCancelReason): void | PromiseLike<unknown>
    /** Adopts the ticket's committed state into the working set; rejects when it could not. */
    adopt(status: S): Promise<void>
    /** Raises the committed-refresh fence for a ticket whose committed state was not adopted. */
    fence(event: NativeGenerationFenceEvent<S>): void
    clock?: NativeGenerationGuardClock
    onError?(error: unknown): void
}

export interface NativeGenerationGuardReinstall<S extends NativeGenerationGuardStatus> extends NativeGenerationGuardTarget {
    readonly status: S
    readonly effectKeys?: readonly string[]
}

export interface NativeGenerationGuardEntry extends NativeGenerationGuardTarget {
    readonly ticket: NativeGenerationGuardTicket
    readonly state: 'holding' | 'adopting' | 'fenced'
    readonly cancelled: boolean
}

export interface NativeGenerationCloseResult {
    readonly released: NativeGenerationGuardTicket[]
    readonly fenced: NativeGenerationGuardTicket[]
}

const TERMINAL_PHASES: ReadonlySet<string> = new Set(['completed', 'cancelled', 'stale', 'interrupted', 'failed'])

interface Scope {
    readonly target: NativeGenerationGuardTarget
    readonly keys: ReadonlySet<string>
    /** Some guarded key lives in the root record, which a whole-root write replaces. */
    readonly rootKeys: boolean
    /** Some guarded key is a field of the target character, which a detail write replaces. */
    readonly characterKeys: boolean
}

interface Deferred<T> {
    readonly promise: Promise<T>
    resolve(value: T): void
}

interface Entry<S extends NativeGenerationGuardStatus> {
    readonly id: string
    readonly ticket: NativeGenerationGuardTicket
    scope: Scope
    state: 'holding' | 'adopting' | 'fenced'
    cancelled: boolean
    status: S | null
    adoption: Promise<NativeGenerationSettlement> | null
    waiter: Deferred<NativeGenerationHoldOutcome> | null
}

function deferred<T>(): Deferred<T> {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((settle) => { resolve = settle })
    return { promise, resolve }
}

function defaultClock(): NativeGenerationGuardClock {
    return {
        setTimeout: (callback, delay) => globalThis.setTimeout(callback, delay),
        clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
    }
}

function ticketId(ticket: NativeGenerationGuardTicket): string {
    if (typeof ticket?.epoch !== 'string' || !ticket.epoch || typeof ticket.sequence !== 'string' || !ticket.sequence) {
        throw new TypeError('Native generation ticket requires an epoch and a sequence')
    }
    return JSON.stringify([ticket.epoch, ticket.sequence])
}

function parseUnitKey(key: string): string[] | null {
    let parts: unknown
    try { parts = JSON.parse(key) } catch { return null }
    if (!Array.isArray(parts) || parts.length === 0 || !parts.every((part) => typeof part === 'string')) return null
    return parts
}

function createScope(target: NativeGenerationGuardTarget, effectKeys: Iterable<string>): Scope {
    if (typeof target?.characterId !== 'string' || !target.characterId ||
        typeof target.conversationId !== 'string' || !target.conversationId) {
        throw new TypeError('Native generation target requires stable IDs')
    }
    const keys = new Set<string>()
    let rootKeys = false
    let characterKeys = false
    for (const key of effectKeys) {
        const parts = parseUnitKey(key)
        if (!parts) throw new TypeError('Native generation effect key is not a unit key')
        keys.add(JSON.stringify(parts))
        const [kind, first] = parts
        if (kind === 'character') characterKeys ||= first === target.characterId
        else if (kind !== 'messages' && kind !== 'conversation' &&
            !(kind === 'exists' && (first === 'character' || first === 'conversation')) &&
            !(kind === 'order' && first === 'conversations')) rootKeys = true
    }
    return {
        target: { characterId: target.characterId, conversationId: target.conversationId },
        keys, rootKeys, characterKeys,
    }
}

function unitKeyTouches(key: string, scope: Scope): boolean {
    const parts = parseUnitKey(key)
    // An unreadable key could address the target.
    if (!parts) return true
    if (scope.keys.has(JSON.stringify(parts))) return true
    const [kind, first, second, third] = parts
    const { characterId, conversationId } = scope.target
    if (kind === 'messages' || kind === 'conversation') return first === characterId && second === conversationId
    if (kind === 'exists' && first === 'conversation') return second === characterId && third === conversationId
    if (kind === 'exists' && first === 'character') return second === characterId
    return false
}

function writeTouches(write: NativeGenerationTargetWrite, scope: Scope): boolean {
    const { characterId, conversationId } = scope.target
    if (write.unitMutations?.some((mutation) => unitKeyTouches(mutation.key, scope))) return true
    if (write.rootMutations?.some((mutation) => scope.keys.has(JSON.stringify(['root', mutation.key])))) return true
    if (write.root !== undefined && scope.rootKeys) return true
    if (scope.characterKeys && (write.character?.chaId === characterId ||
        write.characterDetails?.some((detail) => detail.chaId === characterId))) return true
    if (write.replaceCharacter?.chaId === characterId || write.addCharacter?.chaId === characterId) return true
    if (write.deleteCharacterIds?.includes(characterId)) return true
    return write.conversations?.some((mutation) => mutation.type !== 'reorder' &&
        mutation.characterId === characterId && mutation.conversationId === conversationId) ?? false
}

/**
 * Whether a write reaches the target conversation, its messages or existence, its
 * character's existence or whole record, or one of the given effect unit keys.
 */
export function touchesNativeGenerationTarget(
    write: NativeGenerationTargetWrite,
    target: NativeGenerationGuardTarget,
    effectKeys: Iterable<string> = [],
): boolean {
    return writeTouches(write, createScope(target, effectKeys))
}

function settledStatus(status: NativeGenerationGuardStatus): boolean {
    return TERMINAL_PHASES.has(status.phase) && !status.pendingCommit
}

function isThenable(value: unknown): value is PromiseLike<unknown> {
    return typeof (value as PromiseLike<unknown> | null)?.then === 'function'
}

export class NativeGenerationGuard<S extends NativeGenerationGuardStatus = NativeGenerationGuardStatus> {
    private readonly guarded = new Map<string, Entry<S>>()
    private readonly clock: NativeGenerationGuardClock

    constructor(private readonly hooks: NativeGenerationGuardHooks<S>) {
        this.clock = hooks.clock ?? defaultClock()
    }

    /** Guards the target for the ticket. Installing a known ticket again adds its effect keys. */
    install(ticket: NativeGenerationGuardTicket, target: NativeGenerationGuardTarget, effectKeys: readonly string[] = []): void {
        const id = ticketId(ticket)
        const scope = createScope(target, effectKeys)
        const existing = this.guarded.get(id)
        if (existing) {
            if (existing.scope.target.characterId !== scope.target.characterId ||
                existing.scope.target.conversationId !== scope.target.conversationId) {
                throw new TypeError('Native generation ticket already guards another target')
            }
            existing.scope = createScope(scope.target, [...existing.scope.keys, ...scope.keys])
            return
        }
        this.guarded.set(id, {
            id, ticket: { epoch: ticket.epoch, sequence: ticket.sequence }, scope,
            state: 'holding', cancelled: false, status: null, adoption: null, waiter: null,
        })
    }

    /** Replaces the effect unit keys guarded beside the target conversation. */
    setEffectKeys(ticket: NativeGenerationGuardTicket, effectKeys: readonly string[]): boolean {
        const entry = this.guarded.get(ticketId(ticket))
        if (!entry) return false
        entry.scope = createScope(entry.scope.target, effectKeys)
        return true
    }

    /** Guards again the tickets native still lists after the working set was loaded anew. */
    reinstall(active: readonly NativeGenerationGuardReinstall<S>[]): void {
        for (const value of active) {
            this.install(value.status.ticket, value, value.effectKeys)
            const entry = this.guarded.get(ticketId(value.status.ticket))!
            if (entry.state === 'holding') entry.status = value.status
        }
    }

    /**
     * Holds a write that reaches a guarded target, or returns null. The first held write
     * of a running ticket cancels it. The caller must not commit a held write and must
     * await `settled` outside the save queue before retrying it.
     */
    holdWrite(write: NativeGenerationTargetWrite, source: NativeGenerationWriteSource = 'edit'): NativeGenerationHold | null {
        const held = [...this.guarded.values()].filter((entry) => writeTouches(write, entry.scope))
        if (held.length === 0) return null
        for (const entry of held) if (entry.state === 'holding') this.cancelOnce(entry, source)
        return this.hold(held)
    }

    /**
     * Holds, without cancelling, a write that lost a revision race to a guarded target's
     * conversation, so it retries after that ticket's adoption. Returns null otherwise.
     */
    holdConflict(changed: readonly ContentChangeKey[]): NativeGenerationHold | null {
        const held = [...this.guarded.values()].filter((entry) => changed.some((key) => key.kind === 'conversation' &&
            key.key1 === entry.scope.target.characterId && key.key2 === entry.scope.target.conversationId))
        return held.length === 0 ? null : this.hold(held)
    }

    /**
     * Records a status and adopts committed state before release when required.
     * A status whose output cannot be verified fences the ticket.
     */
    settle(status: S): Promise<NativeGenerationSettlement> {
        const entry = this.guarded.get(ticketId(status.ticket))
        if (!entry) return Promise.resolve('unknown')
        if (entry.state === 'adopting') return entry.adoption!
        if (entry.state === 'fenced') {
            if (settledStatus(status)) entry.status = status
            return Promise.resolve('fenced')
        }
        entry.status = status
        if (!settledStatus(status)) return Promise.resolve('pending')
        if (status.output === 'unavailable') {
            this.fenceEntry(entry, 'output-unavailable')
            return Promise.resolve('fenced')
        }
        return this.adoptEntry(entry, status)
    }

    /** Adopts a fenced ticket's settled state again. */
    retryAdoption(ticket: NativeGenerationGuardTicket): Promise<NativeGenerationSettlement> {
        const entry = this.guarded.get(ticketId(ticket))
        if (!entry) return Promise.resolve('unknown')
        if (entry.state === 'adopting') return entry.adoption!
        if (entry.state === 'holding') return Promise.resolve('pending')
        if (!entry.status || !settledStatus(entry.status)) return Promise.resolve('fenced')
        return this.adoptEntry(entry, entry.status)
    }

    /** Stops guarding a fenced, settled ticket once a committed refresh has reloaded its target. */
    release(ticket: NativeGenerationGuardTicket): boolean {
        const entry = this.guarded.get(ticketId(ticket))
        if (!entry || entry.state !== 'fenced' || !entry.status || !settledStatus(entry.status)) return false
        this.releaseEntry(entry)
        return true
    }

    /**
     * Cancels every guarded ticket for an app lifecycle end and waits for settlement and
     * adoption up to the bound, then fences the tickets that are still open.
     */
    close(boundMs: number): Promise<NativeGenerationCloseResult> {
        const entries = [...this.guarded.values()]
        const fenced = entries.filter((entry) => entry.state === 'fenced').map((entry) => ({ ...entry.ticket }))
        const waiting = entries.filter((entry) => entry.state !== 'fenced')
        for (const entry of waiting) if (entry.state === 'holding') this.cancelOnce(entry, 'lifecycle')
        if (waiting.length === 0) return Promise.resolve({ released: [], fenced })
        return new Promise((resolve) => {
            const outcomes = new Map<Entry<S>, NativeGenerationHoldOutcome>()
            let done = false
            const finish = () => {
                if (done || outcomes.size < waiting.length) return
                done = true
                this.clock.clearTimeout(handle)
                resolve({
                    released: waiting.filter((entry) => outcomes.get(entry) === 'released').map((entry) => ({ ...entry.ticket })),
                    fenced: [...fenced, ...waiting.filter((entry) => outcomes.get(entry) === 'fenced').map((entry) => ({ ...entry.ticket }))],
                })
            }
            const handle = this.clock.setTimeout(() => {
                for (const entry of waiting) {
                    if (!outcomes.has(entry) && this.guarded.get(entry.id) === entry && entry.state !== 'fenced') {
                        this.fenceEntry(entry, 'close-timeout')
                    }
                }
            }, boundMs)
            for (const entry of waiting) {
                void this.outcome(entry).then((outcome) => {
                    outcomes.set(entry, outcome)
                    finish()
                })
            }
        })
    }

    snapshot(): NativeGenerationGuardEntry[] {
        return [...this.guarded.values()].map((entry) => ({
            ticket: { ...entry.ticket }, ...entry.scope.target, state: entry.state, cancelled: entry.cancelled,
        }))
    }

    private hold(entries: readonly Entry<S>[]): NativeGenerationHold {
        const outcomes = entries.map((entry) => this.outcome(entry))
        return {
            tickets: entries.map((entry) => ({ ...entry.ticket })),
            settled: Promise.all(outcomes).then((values) => values.includes('fenced') ? 'fenced' : 'released'),
        }
    }

    private outcome(entry: Entry<S>): Promise<NativeGenerationHoldOutcome> {
        if (entry.state === 'fenced') return Promise.resolve('fenced')
        entry.waiter ??= deferred()
        return entry.waiter.promise
    }

    private cancelOnce(entry: Entry<S>, reason: NativeGenerationGuardCancelReason): void {
        if (entry.cancelled) return
        entry.cancelled = true
        try {
            const result = this.hooks.cancel({ ...entry.ticket }, reason)
            if (isThenable(result)) result.then(undefined, (error: unknown) => this.report(error))
        } catch (error) {
            this.report(error)
        }
    }

    private adoptEntry(entry: Entry<S>, status: S): Promise<NativeGenerationSettlement> {
        entry.state = 'adopting'
        const adoption = (async (): Promise<NativeGenerationSettlement> => {
            try {
                await this.hooks.adopt(status)
            } catch (error) {
                if (this.guarded.get(entry.id) === entry) this.fenceEntry(entry, 'adoption-failed', error)
                return 'fenced'
            }
            if (this.guarded.get(entry.id) === entry) this.releaseEntry(entry)
            return 'released'
        })()
        entry.adoption = adoption
        return adoption
    }

    private releaseEntry(entry: Entry<S>): void {
        if (this.guarded.get(entry.id) === entry) this.guarded.delete(entry.id)
        const waiter = entry.waiter
        entry.waiter = null
        waiter?.resolve('released')
    }

    private fenceEntry(entry: Entry<S>, reason: NativeGenerationFenceReason, error?: unknown): void {
        entry.state = 'fenced'
        entry.adoption = null
        try {
            this.hooks.fence({
                ticket: { ...entry.ticket }, target: { ...entry.scope.target }, status: entry.status, reason,
                ...(reason === 'adoption-failed' ? { error } : {}),
            })
        } catch (fenceError) {
            this.report(fenceError)
        }
        const waiter = entry.waiter
        entry.waiter = null
        waiter?.resolve('fenced')
    }

    private report(error: unknown): void {
        try { this.hooks.onError?.(error) } catch { /* reporting must not break the guard */ }
    }
}
