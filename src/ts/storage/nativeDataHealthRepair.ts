import type { PersistentDataRuntime } from './persistentDataRuntime'
import { RevisionConflictError } from './persistentDataStore'
import { NativeFileJobActivationCommittedError } from './nativeFileJobs'

export async function runNativeDataHealthRepair<T extends { revision: number }>(
    expectedRevision: number,
    mutate: (revision: number) => Promise<T>,
    runtime: Pick<PersistentDataRuntime,
        'flushPendingDataLocally' | 'capturePersistentMutationToken' | 'acquireDestructiveReplacementFence' | 'markCommittedWorkingSetRefreshRequired'>,
): Promise<T> {
    await runtime.flushPendingDataLocally('data-health-repair')
    const token = await runtime.capturePersistentMutationToken('data-health-repair', { publishOfficial: false })
    if (token.revision !== expectedRevision) {
        throw new RevisionConflictError(expectedRevision, token.revision)
    }
    const fence = await runtime.acquireDestructiveReplacementFence(token)
    try {
        const result = await mutate(fence.revision).catch((error: unknown) => {
            if (error && typeof error === 'object' && 'code' in error && error.code === 'committed'
                && 'revision' in error && typeof error.revision === 'number'
                && Number.isSafeInteger(error.revision) && error.revision >= fence.revision) {
                const committedError = new NativeFileJobActivationCommittedError(error.revision, error)
                runtime.markCommittedWorkingSetRefreshRequired(error.revision, committedError)
                throw committedError
            }
            throw error
        })
        try {
            const outcome = await fence.refreshCommittedWorkingSet(result.revision)
            if (outcome.projection === 'refresh-required') {
                throw new Error('Committed repair requires a working-set refresh')
            }
        } catch (error) {
            throw new NativeFileJobActivationCommittedError(result.revision, error)
        }
        return result
    } finally {
        fence.release()
    }
}
