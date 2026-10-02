import type { PersistentDataRuntime, PersistentDestructiveReplacementFence } from './persistentDataRuntime'
import type {PersistentMutationToken} from './saveCoordinator'

export type UpstreamReplacementRuntime = Pick<PersistentDataRuntime,
    'withPausedPersistentWrites' | 'beginActivatedLibraryGuard' | 'refreshActivatedLibraryUnderPause'>

export async function acquireUpstreamImportPause(runtime: UpstreamReplacementRuntime, reason: string) {
    let release!: () => void
    const held = new Promise<void>((resolve) => { release = resolve })
    let ready!: (fence: PersistentDestructiveReplacementFence) => void
    let rejectReady!: (error: unknown) => void
    const admitted = new Promise<PersistentDestructiveReplacementFence>((resolve, reject) => { ready = resolve; rejectReady = reject })
    let guard: ReturnType<PersistentDataRuntime['beginActivatedLibraryGuard']> | undefined
    let admittedToken!: PersistentMutationToken
    const settled = runtime.withPausedPersistentWrites(reason, async (token) => {
        guard = runtime.beginActivatedLibraryGuard(token)
        admittedToken = token
        let released = false
        ready({
            revision: token.revision,
            async refreshCommittedWorkingSet(minimumRevision) {
                if (released) throw new Error('Import write pause was released')
                const outcome = await runtime.refreshActivatedLibraryUnderPause(token)
                if (outcome.revision < minimumRevision) throw new Error('Imported library revision is unavailable')
                return outcome
            },
            release() { released = true; release() },
        })
        await held
    })
    void settled.catch(rejectReady)
    const fence = await admitted
    return {
        fence,
        token: admittedToken,
        complete() { guard!.complete() },
        async finish() { fence.release(); await settled },
        async abortUnchanged(proveBindingUnchanged: () => Promise<void>) {
            try {
                await proveBindingUnchanged()
                await guard!.abortUnchanged()
            } finally {
                fence.release()
                await settled
            }
        },
    }
}

export async function confirmUpstreamLibraryReplacement(bound: boolean, warnings: string[] = []): Promise<boolean> {
    const [{ alertCheckboxConfirm }, { language }] = await Promise.all([import('../alert'), import('../../lang')])
    return (await alertCheckboxConfirm({
        title: language.lwwSync.restoreTitle,
        description: [bound ? language.lwwSync.restoreDescriptionBound : language.lwwSync.restoreDescription, ...warnings].join('\n\n'),
        checkboxLabel: language.lwwSync.restoreAcknowledge,
        actionLabel: language.lwwSync.restoreAction,
        cancelLabel: language.lwwSync.cancelAction,
        requireChecked: true,
    })).confirmed
}
