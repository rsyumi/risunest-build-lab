import type { LifecycleCommitReason } from './lifecycleCommit'
import type { PersistentDataRuntime } from './persistentDataRuntime'

export interface LifecycleFlushDependencies {
    runtime: Pick<PersistentDataRuntime, 'flushPendingData' | 'flushPendingDataLocally'>
    isTauriAndroid: boolean
    isTauriDesktop: boolean
    nativePlatform(): string
    forgetInlayProviderImages(): void
    releaseIdleTransformerModels(): Promise<void>
}

export function createLifecycleFlush(dependencies: LifecycleFlushDependencies) {
    const { runtime } = dependencies
    const flush = (reason: string, locally = false): Promise<void> => {
        dependencies.forgetInlayProviderImages()
        void dependencies.releaseIdleTransformerModels().catch(() => {
            console.warn('Optional model memory cleanup failed')
        })
        return locally ? runtime.flushPendingDataLocally(reason) : runtime.flushPendingData(reason)
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
