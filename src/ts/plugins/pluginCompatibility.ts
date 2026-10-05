import { Mutex } from '../mutex'

interface PluginLoadDependencies<T> {
    resetRegistry(isCurrent: () => boolean): Promise<unknown>
    loadV3(plugins: readonly T[]): Promise<unknown>
}

export interface PluginLoadReentrancyGuard {
    runEvaluation<T>(evaluate: () => Promise<T>): Promise<T>
    settle(operation: Promise<void>): Promise<void>
}

export function createPluginLoadReentrancyGuard(
    onBackgroundError: (error: unknown) => void,
): PluginLoadReentrancyGuard {
    let evaluationDepth = 0
    return {
        async runEvaluation<T>(evaluate: () => Promise<T>): Promise<T> {
            evaluationDepth++
            try {
                return await evaluate()
            } finally {
                evaluationDepth--
            }
        },
        settle(operation: Promise<void>): Promise<void> {
            if (evaluationDepth === 0) return operation
            void operation.catch(onBackgroundError)
            return Promise.resolve()
        },
    }
}

export async function runPluginUnloadCallbacks(
    callbacks: Set<() => void | Promise<void>>,
    isCurrent: () => boolean,
    reentrancy: PluginLoadReentrancyGuard,
): Promise<boolean> {
    for (const callback of [...callbacks]) {
        callbacks.delete(callback)
        await reentrancy.runEvaluation(async () => callback())
        if (!isCurrent()) return false
    }
    return isCurrent()
}

export function createPluginLoadOrchestrator<T>(dependencies: PluginLoadDependencies<T>) {
    let loadGeneration = 0
    let latestLoad: Promise<void> = Promise.resolve()
    const operationMutex = new Mutex()

    return (plugins: readonly T[]): Promise<void> => {
        const generation = ++loadGeneration
        const isCurrent = () => generation === loadGeneration

        const operation = operationMutex.runExclusive(async () => {
            await dependencies.resetRegistry(isCurrent)
            if (!isCurrent()) return

            await dependencies.loadV3(plugins)
        })
        // A caller whose load was replaced waits for the replacement, so the runtime it
        // asked to change has changed when its promise settles.
        const settled = operation.then(() => isCurrent() ? undefined : latestLoad)
        latestLoad = settled
        return settled
    }
}
