import { get } from 'svelte/store'
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

async function hasLibraryContent(): Promise<boolean> {
    try {
        return await (await import('./sync/bindingLocalData')).hasLocalLibraryContent()
    } catch {
        // An unreadable library still asks.
        return true
    }
}

export async function confirmUpstreamLibraryReplacement(bound: boolean, warnings: string[] = []): Promise<boolean> {
    const [alert, { language }, { nativeFileJobHost }] = await Promise.all([
        import('../alert'), import('../../lang'), import('./nativeFileJobManager'),
    ])
    // In the onboarding, an unbound restore over a library without content replaces nothing.
    if (!bound && get(nativeFileJobHost) === 'onboarding' && !await hasLibraryContent()) {
        return warnings.length === 0 || await alert.alertActionConfirm({
            title: language.lwwSync.restoreTitle,
            description: warnings.join('\n\n'),
            actionLabel: language.lwwSync.restoreAction,
            cancelLabel: language.lwwSync.cancelAction,
        })
    }
    return (await alert.alertCheckboxConfirm({
        title: language.lwwSync.restoreTitle,
        description: [bound ? language.lwwSync.restoreDescriptionBound : language.lwwSync.restoreDescription, ...warnings].join('\n\n'),
        checkboxLabel: language.lwwSync.restoreAcknowledge,
        actionLabel: language.lwwSync.restoreAction,
        cancelLabel: language.lwwSync.cancelAction,
        requireChecked: true,
    })).confirmed
}
