import { get } from 'svelte/store'
import { createStorageMutationGate } from '../storageMutationGate'
import { createSyncBindingFlow, type BindingActivationGuard, type BindingPluginLifecycle, type BindingRecoveryRegistration, type SyncBindingNative } from './bindingFlow'
import type { PersistentDataRuntime } from '../persistentDataRuntime'
import type { PersistentMutationToken } from '../saveCoordinator'
import { registerCommittedWorkingSetContinuation } from '../committedWorkingSetContinuation'
import { confirmPreviousStorageFiles, confirmSyncBindingReplacement, downloadPreviousStorageFiles } from './bindingDialog'
import { hasLocalBindingData, hasLocalLibraryContent, hasLocalSharedBindingData } from './bindingLocalData'
import { createNativeSyncBindingBridge } from './bindingNative'
import { getSyncBindingTransport, registerSyncBindingFlow } from './bindingRegistry'

export function createSyncBindingRecoveryRegistration(
    getRuntime: () => PersistentDataRuntime,
    getPausedToken: () => PersistentMutationToken,
): BindingRecoveryRegistration {
    return {
        setLifecycle: lifecycle => getRuntime().setActivatedLibraryRecoveryLifecycle(getPausedToken(), lifecycle),
        registerFailure(error, resume) {
            const runtime = getRuntime()
            const revision = runtime.revision
            runtime.markCommittedWorkingSetRefreshRequired(revision, error)
            registerCommittedWorkingSetContinuation(revision, runtime, runtime.getStorageAuthorityEpoch(), resume, undefined, true)
        },
    }
}

export function installSyncBindingFlow(dependencies: {
    native?: SyncBindingNative
    plugins: BindingPluginLifecycle
    withPausedWrites<T>(operation: () => Promise<T>): Promise<T>
    refreshActivatedLibrary(): Promise<void>
    beginActivatedLibraryGuard(): BindingActivationGuard
    recovery: BindingRecoveryRegistration
    /** Runs each question the binding asks the user, such as whether to replace this library. */
    whileAsking?<T>(ask: () => Promise<T>): Promise<T>
}) {
    const { whileAsking = ask => ask(), ...rest } = dependencies
    const flow = createSyncBindingFlow({
        ...rest,
        native: dependencies.native ?? createNativeSyncBindingBridge(),
        resolveTransport: getSyncBindingTransport,
        gate: createStorageMutationGate(),
        // The onboarding compares with what a new device stores and leaves out the settings it writes, such as the language.
        hasNonDefaultData: async () => get((await import('../nativeFileJobManager')).nativeFileJobHost) === 'onboarding'
            ? hasLocalLibraryContent() : hasLocalBindingData(),
        hasNonDefaultSharedData: hasLocalSharedBindingData,
        confirmReplacement: reason => whileAsking(() => confirmSyncBindingReplacement(reason)),
        confirmPreviousStorageFiles: context => whileAsking(() => confirmPreviousStorageFiles(context)),
        downloadPreviousStorageFiles,
    })
    return { flow, dispose: registerSyncBindingFlow(flow) }
}
