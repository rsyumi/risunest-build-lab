import { createStorageMutationGate } from '../storageMutationGate'
import { createSyncBindingFlow, type BindingActivationGuard, type BindingPluginLifecycle, type BindingRecoveryRegistration, type SyncBindingNative } from './bindingFlow'
import type { PersistentDataRuntime } from '../persistentDataRuntime'
import type { PersistentMutationToken } from '../saveCoordinator'
import { registerCommittedWorkingSetContinuation } from '../committedWorkingSetContinuation'
import { confirmPreviousStorageFiles, confirmSyncBindingReplacement, downloadPreviousStorageFiles } from './bindingDialog'
import { hasLocalBindingData, hasLocalSharedBindingData } from './bindingLocalData'
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
}) {
    const flow = createSyncBindingFlow({
        ...dependencies,
        native: dependencies.native ?? createNativeSyncBindingBridge(),
        resolveTransport: getSyncBindingTransport,
        gate: createStorageMutationGate(),
        hasNonDefaultData: hasLocalBindingData,
        hasNonDefaultSharedData: hasLocalSharedBindingData,
        confirmReplacement: confirmSyncBindingReplacement,
        confirmPreviousStorageFiles,
        downloadPreviousStorageFiles: () => downloadPreviousStorageFiles(),
    })
    return { flow, dispose: registerSyncBindingFlow(flow) }
}
