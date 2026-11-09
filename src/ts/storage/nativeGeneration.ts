import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../platform'
import { NativeGenerationUnknownTicketError, type NativeGenerationPort } from './nativeGenerationPort'

/** An unsigned 64-bit value as decimal text, so it never passes through a JS number. */
export type NativeGenerationDecimal = string

export interface NativeGenerationTicket {
    epoch: NativeGenerationDecimal
    sequence: NativeGenerationDecimal
}

export type NativeGenerationPhase =
    | 'reserved' | 'running' | 'completed' | 'cancelled' | 'stale' | 'interrupted' | 'failed'

export type NativeGenerationCancelReason = 'user' | 'edit' | 'plugin' | 'authority' | 'lifecycle'

export interface NativeGenerationReserveRequest {
    characterId: string
    conversationId: string
    expectedRevision: number
    libraryIdentity: string
    bindingAuthority: NativeGenerationDecimal
    effectivePresetId: string
    /** What the native CBS stage reads from this JavaScript engine; without it, the functions that read it decline. */
    host?: NativeGenerationHost
}

export interface NativeGenerationHost {
    /** Whether the default locale's `toLocaleLowerCase` or `toLocaleUpperCase` differs from the root mapping. */
    tailoredCase: boolean
    /** The Unicode version of `toLowerCase` and `toUpperCase`. */
    caseUnicode: 'before16' | 'unicode16' | 'unicode17' | 'from18'
    /** The offsets `Date`'s local getters add: each `[start, offset]` holds from its start until the next, in milliseconds, up to `end`. */
    timeZone: { transitions: [number, number][]; end: number }
    /** The Unicode version of the RegExp property and case folding tables. */
    regexUnicode: 'before17' | 'unicode17' | 'from18'
    /** Whether this start excludes regex, so a script run takes only the character's scripts. */
    regexExcluded: boolean
}

export type NativeGenerationReservation =
    | { ticket: NativeGenerationTicket }
    | { unsupported: string }

export type NativeGenerationFailureCategory =
    | 'unsupported' | 'invalid' | 'tokenizer' | 'context-limit'
    | 'limit' | 'compat' | 'decode' | 'network' | 'type' | 'sdk' | 'http' | 'consumer'
    | 'unsupported-output' | 'stale' | 'interrupted'

export interface NativeGenerationFailure {
    category: NativeGenerationFailureCategory
    /** The unsupported setting for `unsupported`, the limit name for `limit`. */
    field: string | null
    /** The transport stage or the preparation/commit stage that failed. */
    stage: string | null
    httpStatus: number | null
    sdkMessage: string | null
}

export interface NativeGenerationStatus {
    ticket: NativeGenerationTicket
    /** The target the operation was reserved for, so a reload can reinstall its guard. */
    characterId: string
    conversationId: string
    phase: NativeGenerationPhase
    messageId: string
    checkpoint: NativeGenerationDecimal
    committedRevision: number | null
    pendingCommit: boolean
    preparationReady: boolean
    preparationError: { kind: string; field?: string } | null
    dispatch: 'none' | 'issued' | 'uncertain'
    output: 'none' | 'committed' | 'unavailable'
    cancelDecision: NativeGenerationCancelReason | null
    failure: NativeGenerationFailure | null
    diagnosticsAvailable: boolean
}

export type NativeGenerationStatusObservation = Pick<NativeGenerationStatus,
    'phase' | 'dispatch' | 'checkpoint' | 'committedRevision' | 'pendingCommit'
    | 'preparationReady' | 'output' | 'cancelDecision' | 'diagnosticsAvailable'>

export interface NativeGenerationLease {
    token: string
    revision: number
}

export interface NativeGenerationWitness {
    ticket: NativeGenerationTicket
    originalRevision: number
    originalCount: number
    rows: { originalIndex: number; rowHash: string; idState: 'missing' | 'null' | 'present' }[]
    [field: string]: unknown
}

export interface NativeGenerationProof {
    ticket: NativeGenerationTicket
    revision: number
    at: 'issued' | Exclude<NativeGenerationPhase, 'reserved' | 'running'>
    /** JSON-encoded unit keys, the encoding the unit projection consumes. */
    affectedUnitKeys: string[]
    outputMessageId: string | null
    receipts: { normalization: unknown | null; effects: unknown[]; output: unknown | null }
    idDeltas: unknown[] | null
    mappingUnavailable: boolean
    failedWithOutput: boolean
}

export const NATIVE_GENERATION_STATUS_WAIT_CAP_MS = 25_000

function requireTauri(): void {
    if (!isTauri) throw new Error('Native generation requires Tauri')
}

/** Native-side stored once; the web build never reaches this module's commands. */
export async function nativeGenerationHostInit(userAgent: string): Promise<void> {
    requireTauri()
    await invoke('native_generation_host_init', { userAgent })
}

export async function nativeGenerationReserveOllama(
    request: NativeGenerationReserveRequest,
): Promise<NativeGenerationReservation> {
    requireTauri()
    return invoke<NativeGenerationReservation>('native_generation_reserve_ollama', { request })
}

export async function nativeGenerationExecute(
    ticket: NativeGenerationTicket,
): Promise<NativeGenerationStatus> {
    requireTauri()
    return invoke<NativeGenerationStatus>('native_generation_execute', { ticket })
}

export async function nativeGenerationCancel(
    ticket: NativeGenerationTicket,
    reason: NativeGenerationCancelReason,
): Promise<NativeGenerationStatus> {
    requireTauri()
    return invoke<NativeGenerationStatus>('native_generation_cancel', { ticket, reason })
}

/** Resolves after a phase or dispatch change, or once the wait (capped at 25 s) ends. */
export async function nativeGenerationStatus(
    ticket: NativeGenerationTicket,
    waitMs?: number,
    after?: NativeGenerationStatusObservation,
): Promise<NativeGenerationStatus> {
    requireTauri()
    const wait = waitMs === undefined
        ? null
        : Math.min(Math.max(Math.trunc(waitMs), 0), NATIVE_GENERATION_STATUS_WAIT_CAP_MS)
    return invoke<NativeGenerationStatus>('native_generation_status', { ticket, waitMs: wait, ...(after === undefined ? {} : { after }) })
}

export async function nativeGenerationActive(): Promise<NativeGenerationStatus[]> {
    requireTauri()
    return invoke<NativeGenerationStatus[]>('native_generation_active')
}

export async function nativeGenerationCaptureWitness(
    ticket: NativeGenerationTicket,
    original: NativeGenerationLease,
    indices: readonly number[],
): Promise<NativeGenerationWitness> {
    requireTauri()
    return invoke<NativeGenerationWitness>('native_generation_capture_witness', {
        ticket, original, indices: [...indices],
    })
}

export async function nativeGenerationProof(
    ticket: NativeGenerationTicket,
    normalization?: {
        witness: NativeGenerationWitness
        original: NativeGenerationLease
        current: NativeGenerationLease
    },
): Promise<NativeGenerationProof> {
    requireTauri()
    return invoke<NativeGenerationProof>('native_generation_proof', {
        ticket, normalization: normalization ?? null,
    })
}

const providerErrorCategories: ReadonlySet<NativeGenerationFailureCategory> =
    new Set(['http', 'sdk', 'network', 'type', 'decode'])

/**
 * The text the TS route shows for the same provider outcome. TS lets the thrown
 * SDK, fetch or parse error through to `alertError`, which shows the error's
 * message trimmed, with no prefix. Failures TS never words (unsupported, limit,
 * consumer, stale and similar) return null, so the caller chooses its own text.
 */
export function nativeGenerationFailureMessage(failure: NativeGenerationFailure): string | null {
    if (!providerErrorCategories.has(failure.category) || failure.sdkMessage === null) return null
    return failure.sdkMessage.trim()
}

/** Adapts the command facade to the storage runtime's recovery and adoption port. */
export function createNativeGenerationPort(): NativeGenerationPort {
    const observed = new Map<string, string>()
    const key = (ticket: NativeGenerationTicket) => JSON.stringify([ticket.epoch, ticket.sequence])
    const remember = (status: NativeGenerationStatus) => {
        const id = key(status.ticket)
        const value = JSON.stringify(status)
        const previous = observed.get(id)
        observed.delete(id)
        observed.set(id, value)
        // Match the native host's bound on retained operations.
        if (observed.size > 64) observed.delete(observed.keys().next().value!)
        return previous !== value
    }
    const result = async <T>(operation: Promise<T>): Promise<T> => {
        try { return await operation } catch (error) {
            if (error !== null && typeof error === 'object'
                && 'code' in error && error.code === 'validation'
                && 'message' in error && (error.message === 'native-generation:unknown-ticket'
                    || error.message === 'native-generation:expired-ticket')) {
                throw new NativeGenerationUnknownTicketError(error.message)
            }
            throw error
        }
    }
    return {
        async active() {
            const statuses = await result(nativeGenerationActive())
            for (const status of statuses) remember(status)
            return statuses
        },
        async status(ticket, waitMs) {
            const current = await result(nativeGenerationStatus(ticket))
            const changed = remember(current)
            if (!waitMs || waitMs <= 0 || changed || (current.phase !== 'reserved' && current.phase !== 'running')) return current
            const after: NativeGenerationStatusObservation = {
                phase: current.phase, dispatch: current.dispatch, checkpoint: current.checkpoint,
                committedRevision: current.committedRevision, pendingCommit: current.pendingCommit,
                preparationReady: current.preparationReady, output: current.output,
                cancelDecision: current.cancelDecision, diagnosticsAvailable: current.diagnosticsAvailable,
            }
            const next = await result(nativeGenerationStatus(ticket, waitMs, after))
            remember(next)
            return next
        },
        proof: (ticket) => result(nativeGenerationProof(ticket)),
        async cancel(ticket, reason) {
            const status = await result(nativeGenerationCancel(ticket, reason))
            remember(status)
            return status
        },
    }
}
