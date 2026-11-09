import type { PersistentDataRuntime } from '../storage/persistentDataRuntime'
import type { PersistentRevisionLease } from '../storage/persistentDataStore'
import { PersistentMutationFencedError } from '../storage/saveCoordinator'
import { acquireCurrentRevisionWithRetry, releasePersistentRevisionLease } from '../storage/persistentRecordIterator'
import { hasNativePersistentRevisionLease, nativePersistentRevisionLease } from '../storage/nativePersistentExport'
import {
    nativeGenerationHostInit, nativeGenerationReserveOllama, nativeGenerationExecute,
    nativeGenerationStatus, nativeGenerationCancel, nativeGenerationCaptureWitness,
    nativeGenerationProof, nativeGenerationFailureMessage,
    type NativeGenerationTicket, type NativeGenerationStatus, type NativeGenerationLease,
    type NativeGenerationWitness, type NativeGenerationStatusObservation,
} from '../storage/nativeGeneration'
import { captureNativeHost } from './nativeHostCapture'

let hostInitialization: Promise<void> | undefined
function initializeHost(): Promise<void> {
    return hostInitialization ??= nativeGenerationHostInit(navigator.userAgent).catch(error => {
        hostInitialization = undefined
        throw error
    })
}

const terminal = (status: NativeGenerationStatus) =>
    status.phase !== 'reserved' && status.phase !== 'running' && !status.pendingCommit
const observation = (status: NativeGenerationStatus): NativeGenerationStatusObservation => ({
    phase: status.phase, dispatch: status.dispatch, checkpoint: status.checkpoint,
    committedRevision: status.committedRevision, pendingCommit: status.pendingCommit,
    preparationReady: status.preparationReady, output: status.output,
    cancelDecision: status.cancelDecision, diagnosticsAvailable: status.diagnosticsAvailable,
})
function leaseDto(lease: PersistentRevisionLease): NativeGenerationLease {
    if (!hasNativePersistentRevisionLease(lease)) throw new Error('Native generation requires a native revision lease')
    return { token: lease[nativePersistentRevisionLease], revision: lease.revision }
}
async function retryRead<T>(read: () => Promise<T>): Promise<T> {
    for (let attempt = 0; ; attempt++) {
        try { return await read() } catch (error) {
            if (attempt === 2) throw error
            await new Promise(resolve => setTimeout(resolve, 50))
        }
    }
}
function failureMessage(status: NativeGenerationStatus): string {
    if (status.failure) return nativeGenerationFailureMessage(status.failure)
        ?? `Native generation ${status.failure.category}${status.failure.field ? `: ${status.failure.field}` : ''}`
    return `Native generation ${status.preparationError?.kind ?? status.phase}`
}

export interface NativeOllamaRouteOptions {
    runtime: PersistentDataRuntime
    target: { characterId: string; conversationId: string }
    effectivePresetId: string
    authorityEpoch: number
    signal?: AbortSignal
    /** Rechecked only before dispatch; navigation never revokes an executing owner. */
    isPreparationTargetCurrent(): boolean
    /** Reads resident rows only and never materializes history. */
    residentNullishIndices(): readonly number[]
    onProgress?(completed: number): void
    onPrepared?(ticket: NativeGenerationTicket, status: NativeGenerationStatus): {
        releaseNavigation(): void
        releaseMessageEdit(): void
    }
}

/**
 * Runs the selected native route once. A reservation refusal comes before any native effect and returns `refused`, so
 * the send can take the JS route; any other failure never enters JS generation.
 */
export async function runNativeOllamaRoute(options: NativeOllamaRouteOptions): Promise<{ completed: boolean; message: string } | { refused: string }> {
    const { runtime, target, signal } = options
    const store = runtime.store
    let refused: string | undefined
    let ticket: NativeGenerationTicket | undefined
    let status: NativeGenerationStatus | undefined
    let guarded = false
    let prepared: ReturnType<NonNullable<NativeOllamaRouteOptions['onPrepared']>> | undefined
    let original: PersistentRevisionLease | undefined
    let witness: NativeGenerationWitness | undefined
    let cancellation: Promise<NativeGenerationStatus> | undefined
    const cancel = (reason: 'user' | 'authority') => {
        prepared?.releaseMessageEdit()
        if (!ticket) return undefined
        return cancellation ??= nativeGenerationCancel(ticket, reason)
    }
    const abort = () => { void cancel('user')?.catch(() => {}) }
    signal?.addEventListener('abort', abort, { once: true })
    const assertAuthority = () => runtime.assertPersistentMutationAllowed(options.authorityEpoch)
    const readStatus = async () => {
        assertAuthority()
        if (signal?.aborted) abort()
        const previous = status
        return retryRead(() => nativeGenerationStatus(ticket!, previous ? 25_000 : undefined,
            previous ? observation(previous) : undefined))
    }
    const proof = async () => {
        if (!original || !witness) return retryRead(() => nativeGenerationProof(ticket!))
        const current = await acquireCurrentRevisionWithRetry(
            revision => store.acquireRevision(revision),
            () => store.readRevision ? store.readRevision() : store.readRoot().then(value => value.revision),
        )
        try {
            return await retryRead(() => nativeGenerationProof(ticket!, {
                witness: witness!, original: leaseDto(original!), current: leaseDto(current),
            }))
        } finally { await releasePersistentRevisionLease(current) }
    }
    const settle = (final: NativeGenerationStatus) => runtime.settleNativeGeneration(final)
    try {
        await runtime.withPausedPersistentWrites('native-ollama-send', async token => {
            assertAuthority()
            if (signal?.aborted) return
            if (!options.isPreparationTargetCurrent()) throw new PersistentMutationFencedError()
            if (!store.readLibraryIdentity || !store.lwwBindingState) throw new Error('Native generation storage is unavailable')
            const libraryIdentity = await store.readLibraryIdentity()
            const binding = await store.lwwBindingState()
            await initializeHost()
            runtime.assertNativeGenerationPreparationCurrent(token)
            if (signal?.aborted) return
            if (!options.isPreparationTargetCurrent()) throw new PersistentMutationFencedError()
            const reservation = await nativeGenerationReserveOllama({
                ...target, expectedRevision: token.revision, libraryIdentity,
                bindingAuthority: binding.targetAuthority, effectivePresetId: options.effectivePresetId,
                host: captureNativeHost(),
            })
            if ('unsupported' in reservation) {
                refused = reservation.unsupported
                return
            }
            ticket = reservation.ticket
            runtime.assertNativeGenerationPreparationCurrent(token)
            if (!options.isPreparationTargetCurrent()) throw new PersistentMutationFencedError()
            const effectKeys = [
                ['root', 'statics'], ['character', target.characterId, 'lastInteraction'],
                ['conversation', target.characterId, target.conversationId, 'lastMemory'],
            ].map(key => JSON.stringify(key))
            runtime.installNativeGenerationGuard(ticket, target, effectKeys)
            guarded = true
            original = await store.acquireRevision(token.revision)
            witness = await nativeGenerationCaptureWitness(ticket, leaseDto(original), options.residentNullishIndices().slice(0, 128))
            runtime.assertNativeGenerationPreparationCurrent(token)
            if (!options.isPreparationTargetCurrent()) throw new PersistentMutationFencedError()
            if (signal?.aborted) {
                status = await cancel('user')!
            } else {
                // A lost execute reply is recovered by status, never by executing again.
                try { status = await nativeGenerationExecute(ticket) }
                catch (error) {
                    status = await readStatus()
                    if (status.phase === 'reserved') throw error
                }
            }
            while (status.dispatch !== 'issued' && !terminal(status)) status = await readStatus()
            await runtime.adoptNativeGenerationCommitInQueue(token, await proof())
            options.onProgress?.(1)
        })
        if (refused !== undefined) return { refused }
        if (!ticket) return { completed: false, message: '' }
        if (!status) status = await readStatus()
        prepared = options.onPrepared?.(ticket, status)
        while (!terminal(status)) {
            status = await readStatus()
            if (status.phase !== 'running' || status.dispatch !== 'issued' || status.pendingCommit || status.cancelDecision !== null) {
                prepared?.releaseMessageEdit()
            }
        }
        prepared?.releaseMessageEdit()
        assertAuthority()
        const settlement = await settle(status)
        if (settlement !== 'released') throw new Error('Native generation requires recovery before editing can continue')
        options.onProgress?.(2)
        if (status.phase === 'failed' || status.phase === 'interrupted' || status.phase === 'stale') throw new Error(failureMessage(status))
        if (status.phase !== 'completed' || signal?.aborted) return { completed: false, message: '' }
        if (status.output !== 'committed') throw new Error('Native generation completed without an attached output')
        const output = await store.readConversationWindow({ ...target, anchorMessageId: status.messageId, before: 0, after: 0, limit: 1 })
        return { completed: true, message: output?.value.messages.find(row => row.chatId === status!.messageId)?.data ?? '' }
    } catch (error) {
        prepared?.releaseMessageEdit()
        if (ticket && (!status || !terminal(status))) {
            try { status = await cancel(signal?.aborted ? 'user' : 'authority')! } catch {}
        }
        if (guarded && runtime.getNativeGenerationGuards().some(entry => entry.ticket.epoch === ticket!.epoch && entry.ticket.sequence === ticket!.sequence)) {
            try {
                if (status && terminal(status)) await settle(status)
                else await runtime.settleNativeGeneration({ ...target, ticket: ticket!, phase: 'failed', pendingCommit: false,
                    output: 'unavailable', committedRevision: status?.committedRevision })
            } catch {
                await runtime.settleNativeGeneration({ ...target, ticket: ticket!, phase: 'failed', pendingCommit: false,
                    output: 'unavailable', committedRevision: status?.committedRevision })
            }
        }
        throw error
    } finally {
        prepared?.releaseMessageEdit()
        prepared?.releaseNavigation()
        signal?.removeEventListener('abort', abort)
        if (original) await releasePersistentRevisionLease(original)
    }
}
