import type { LuaEngine, LuaFactory } from 'wasmoon'
import wasmUrl from 'wasmoon/dist/glue.wasm?url'

export async function runLuaSource(
    engine: LuaEngine,
    source: string,
    timeoutMs = 5_000,
): Promise<void> {
    const { getLuaExecutionBudget } = await import('./luaExecutionBudget')
    await getLuaExecutionBudget(engine.global.lua).runSource(engine, source, timeoutMs)
}

export function createLuaFactoryLoader(
    loadWasmoon: () => Promise<typeof import('wasmoon')>,
): () => Promise<LuaFactory> {
    let wasmoonModulePromise: Promise<typeof import('wasmoon')> | undefined

    return async () => {
        const pendingImport = (wasmoonModulePromise ??= loadWasmoon())
        let wasmoonModule: typeof import('wasmoon')
        try {
            wasmoonModule = await pendingImport
        } catch (error) {
            if (wasmoonModulePromise === pendingImport) {
                wasmoonModulePromise = undefined
            }
            throw error
        }

        const { LuaFactory } = wasmoonModule
        const factory = new LuaFactory(typeof process !== 'undefined' && process.versions?.node ? undefined : wasmUrl)
        const createEngine = factory.createEngine
        factory.createEngine = async (options) => {
            const { getLuaExecutionBudget } = await import('./luaExecutionBudget')
            const { functionTimeout, ...engineOptions } = options ?? {}
            const engine = await createEngine.call(factory, engineOptions)
            try {
                getLuaExecutionBudget(engine.global.lua).attach(engine, functionTimeout)
                return engine
            } catch (error) {
                engine.global.close()
                throw error
            }
        }
        return factory
    }
}

export const createLuaFactory = createLuaFactoryLoader(() => import('wasmoon'))
