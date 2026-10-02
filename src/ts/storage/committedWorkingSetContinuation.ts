import type { DataRevision } from './persistentDataStore'
import type { CommittedApplyOutcome, PersistentDataRuntime } from './persistentDataRuntime'

type CommittedWorkingSetContinuation = () => void | Promise<void>

let pending:
    | Readonly<{
          minimumRevision: DataRevision
          runtime: object
          authorityEpoch: number
          continueAfterRefresh: CommittedWorkingSetContinuation
          prepareRefresh?: CommittedWorkingSetContinuation
          retainOnFailure: boolean
          dispatch: {promise?:Promise<void>}
      }>
    | undefined

const criticalRecoveryOwners = new WeakMap<object, {owner:symbol, dispatch:object, isCurrentAuthority:()=>boolean}>()

function isOwnedCriticalRecovery(runtime: object): boolean {
    const active = criticalRecoveryOwners.get(runtime)
    return pending?.runtime === runtime && pending.retainOnFailure && active?.dispatch === pending.dispatch && active.isCurrentAuthority()
}

export function claimCommittedWorkingSetRecovery(runtime: object, authorityEpoch: number, owner: symbol, isCurrentAuthority:()=>boolean): () => void {
    if (!hasRetryableCommittedWorkingSetContinuation(runtime, authorityEpoch)) return () => {}
    if (criticalRecoveryOwners.has(runtime)) throw new Error('Critical working-set recovery is already owned')
    const active = {owner, dispatch:pending!.dispatch, isCurrentAuthority}
    criticalRecoveryOwners.set(runtime, active)
    return () => { if (criticalRecoveryOwners.get(runtime) === active) criticalRecoveryOwners.delete(runtime) }
}

export function registerCommittedWorkingSetContinuation(
    minimumRevision: DataRevision,
    runtime: object,
    authorityEpoch: number,
    continueAfterRefresh: CommittedWorkingSetContinuation,
    prepareRefresh?: CommittedWorkingSetContinuation,
    retainOnFailure = false,
): void {
    if (pending?.runtime === runtime && pending.authorityEpoch === authorityEpoch) {
        throw new Error('A committed working-set continuation is already pending')
    }
    pending = {
        minimumRevision,
        runtime,
        authorityEpoch,
        continueAfterRefresh,
        prepareRefresh,
        retainOnFailure,
        dispatch: {},
    }
}

export async function continueCommittedWorkingSetRefresh(
    revision: DataRevision,
    runtime: object,
    authorityEpoch: number,
): Promise<void> {
    if (!pending) return
    if (pending.runtime !== runtime || pending.authorityEpoch !== authorityEpoch) {
        if (isOwnedCriticalRecovery(runtime)) return
        pending = undefined
        return
    }
    if (revision < pending.minimumRevision) return
    const owned = pending
    owned.dispatch.promise ??= Promise.resolve().then(owned.continueAfterRefresh).then(() => {
        if (pending === owned) pending = undefined
    }, error => {
        if (owned.retainOnFailure) owned.dispatch.promise = undefined
        else if (pending === owned) pending = undefined
        throw error
    })
    await owned.dispatch.promise
}

export function hasRetryableCommittedWorkingSetContinuation(runtime: object, authorityEpoch: number): boolean {
    return pending?.runtime === runtime && pending.authorityEpoch === authorityEpoch && pending.retainOnFailure
}

export function rebaseCommittedWorkingSetContinuation(runtime: object, authorityEpoch: number, currentEpoch: number): void {
    if (hasRetryableCommittedWorkingSetContinuation(runtime, authorityEpoch)) pending = {...pending!, authorityEpoch:currentEpoch}
}

async function prepareCommittedWorkingSetRetry(
    runtime: object,
    authorityEpoch: number,
): Promise<boolean> {
    if (!pending) return false
    if (isOwnedCriticalRecovery(runtime)) return true
    if (pending.runtime !== runtime || pending.authorityEpoch !== authorityEpoch) {
        pending = undefined
        return false
    }
    const claimed = pending
    await claimed.prepareRefresh?.()
    if (pending === claimed && claimed.prepareRefresh) {
        pending = { ...claimed, prepareRefresh: undefined }
    }
    return true
}

function rebaseCommittedWorkingSetRetry(
    runtime: object,
    previousAuthorityEpoch: number,
    authorityEpoch: number,
): void {
    if (pending?.runtime !== runtime || pending.authorityEpoch !== previousAuthorityEpoch) return
    pending = { ...pending, authorityEpoch }
}

export async function retryCommittedWorkingSetRefreshWithContinuation(
    runtime: Pick<
        PersistentDataRuntime,
        'retryCommittedWorkingSetRefresh' | 'getStorageAuthorityEpoch'
    >,
    onContinuationError?: (error: unknown) => void,
): Promise<CommittedApplyOutcome | null> {
    const authorityEpoch = runtime.getStorageAuthorityEpoch()
    let ownsContinuation: boolean
    try {
        ownsContinuation = await prepareCommittedWorkingSetRetry(runtime, authorityEpoch)
    } catch (error) {
        try {
            onContinuationError?.(error)
        } catch {}
        throw error
    }
    const outcome = await runtime.retryCommittedWorkingSetRefresh()
    if (!ownsContinuation) return outcome
    if (outcome?.projection !== 'applied') {
        rebaseCommittedWorkingSetRetry(
            runtime,
            authorityEpoch,
            runtime.getStorageAuthorityEpoch(),
        )
        return outcome
    }
    try {
        await continueCommittedWorkingSetRefresh(
            outcome.revision,
            runtime,
            authorityEpoch,
        )
    } catch (error) {
        try {
            onContinuationError?.(error)
        } catch {}
    }
    return outcome
}
