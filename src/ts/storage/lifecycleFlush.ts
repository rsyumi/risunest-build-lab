import type { LifecycleCommitReason } from './lifecycleCommit'
import type { PersistentDataRuntime } from './persistentDataRuntime'

export interface LifecycleFlushDependencies {
    runtime: Pick<PersistentDataRuntime, 'flushPendingData' | 'flushPendingDataLocally'>
    isTauriAndroid: boolean
    isTauriDesktop: boolean
    nativePlatform(): string
    forgetInlayProviderImages(): void
    releaseIdleTransformerModels(): Promise<void>
    /** Removes the record of running display work, which must not outlive the app in the background. */
    suspendDisplayWork(reason: string): Promise<void>
}

export function createLifecycleFlush(dependencies: LifecycleFlushDependencies) {
    const { runtime } = dependencies
    const flush = (reason: string, locally = false): Promise<void> => {
        dependencies.forgetInlayProviderImages()
        void dependencies.releaseIdleTransformerModels().catch(() => {
            console.warn('Optional model memory cleanup failed')
        })
        const saved = locally ? runtime.flushPendingDataLocally(reason) : runtime.flushPendingData(reason)
        return Promise.all([saved, dependencies.suspendDisplayWork(reason)]).then(() => undefined)
    }
    /** Saves locally where the OS may end the process before sync settles. */
    const flushForLifecycle = (reason: LifecycleCommitReason): Promise<void> => flush(
        reason,
        dependencies.isTauriAndroid
            ? reason === 'stop' || reason === 'trim-memory'
            : dependencies.isTauriDesktop
                && (dependencies.nativePlatform() === 'windows' || dependencies.nativePlatform() === 'linux')
                && reason === 'stop',
    )
    return { flush, flushForLifecycle }
}
