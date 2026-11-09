import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

type LuaRuntime = typeof import('../../src/ts/process/luaRuntime')

// wasmoon takes its Node loader only when neither `document` nor `location` exists, and happy-dom defines both, so the
// first Lua script loads the module with them hidden. Use as the `vi.mock` factory of `src/ts/process/luaRuntime`.
export async function luaRuntimeWithNodeLoader(importOriginal: () => Promise<LuaRuntime>) {
    const actual = await importOriginal()
    return { ...actual, createLuaFactory: async () => {
        const hidden = ['document', 'location'].map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const)
        for (const [name] of hidden) Object.defineProperty(globalThis, name, { value: undefined, configurable: true, writable: true })
        try {
            const factory = await actual.createLuaFactory()
            // The module finishes loading only when this resolves, so the globals stay hidden until then.
            const load = (factory as unknown as { getLuaModule?: () => Promise<unknown> }).getLuaModule
            if (typeof load !== 'function') throw new Error('wasmoon LuaFactory has no getLuaModule to await its module load')
            await load.call(factory)
            return factory
        } finally {
            for (const [name, descriptor] of hidden) {
                if (descriptor) Object.defineProperty(globalThis, name, descriptor)
                else delete (globalThis as Record<string, unknown>)[name]
            }
        }
    } } satisfies LuaRuntime
}

// The Lua runtime fetches its bundled JSON module once, before the first Lua script runs; a fetch stub answers it here.
export function luaJsonModuleResponse(input: RequestInfo | URL): Response | null {
    return String(input) === '/lua/json.lua' ? new Response(readFileSync(resolve('public/lua/json.lua'), 'utf8')) : null
}
