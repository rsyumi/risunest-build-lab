import type { DataRevision } from './persistentDataStore'
import type {
    NativeGenerationGuardCancelReason,
    NativeGenerationGuardPhase,
    NativeGenerationGuardStatus,
    NativeGenerationGuardTicket,
} from './nativeGenerationGuard'

// The native generation commands the storage runtime calls. The command facade
// implements this port; the runtime never names a command itself.

/** The `RouteStatus` fields storage reads; the command DTO is assignable to it. */
export interface NativeGenerationRouteStatus extends NativeGenerationGuardStatus {
    readonly characterId: string
    readonly conversationId: string
    /** The revision of the ticket's last commit, or null before its first. */
    readonly committedRevision?: DataRevision | null
}

/** The `Proof` fields adoption reads; the command DTO is assignable to it. */
export interface NativeGenerationProof {
    readonly ticket: NativeGenerationGuardTicket
    readonly revision: DataRevision
    readonly at: 'issued' | Exclude<NativeGenerationGuardPhase, 'reserved' | 'running'>
    /** Unit keys in the encoding the working-set projection consumes. */
    readonly affectedUnitKeys: readonly string[]
}

export type NativeGenerationCancelReason = NativeGenerationGuardCancelReason | 'user' | 'authority'

/** The longest `native_generation_status` wait native accepts. */
export const NATIVE_GENERATION_STATUS_WAIT_MS = 25_000

/** The status refusal for a ticket native no longer knows, as the port reports it. */
export class NativeGenerationUnknownTicketError extends Error {
    constructor(message = 'Native generation ticket is unknown') {
        super(message)
        this.name = 'NativeGenerationUnknownTicketError'
    }
}

export interface NativeGenerationPort {
    /** `native_generation_active`: Running or unresolved tickets of the active library. */
    active(): Promise<readonly NativeGenerationRouteStatus[]>
    /**
     * `native_generation_status`, returning after a phase or dispatch change or the wait. It
     * rejects with `NativeGenerationUnknownTicketError` when native no longer knows the ticket.
     */
    status(ticket: NativeGenerationGuardTicket, waitMs?: number): Promise<NativeGenerationRouteStatus>
    /** `native_generation_proof` at Issued or at the terminal phase. */
    proof(ticket: NativeGenerationGuardTicket): Promise<NativeGenerationProof>
    /** `native_generation_cancel`. */
    cancel(ticket: NativeGenerationGuardTicket, reason: NativeGenerationCancelReason): Promise<unknown>
}
