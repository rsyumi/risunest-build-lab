// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({
    MCPs: {} as Record<string, unknown>,
    callOnlyMCPs: {} as Record<string, unknown>,
    initializeMCPs: vi.fn(async () => undefined),
    selected: { characterId: 'char-a', conversationId: 'conv-a' } as { characterId: string; conversationId: string } | null,
}))

vi.mock(import('katex'), () => ({}))
vi.mock(import('src/ts/lite'), () => ({}))
vi.mock('src/ts/process/mcp/mcp', () => ({
    MCPs: state.MCPs,
    callOnlyMCPs: state.callOnlyMCPs,
    initializeMCPs: state.initializeMCPs,
}))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', async (importOriginal) => ({
    ...(await importOriginal<Record<string, unknown>>()),
    captureSelectedConversationTarget: () => state.selected,
}))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(async () => false), alertInput: vi.fn(), alertError: vi.fn() }))
vi.mock('src/ts/stores.svelte', () => ({
    DBState: { db: { characters: [], enabledModules: [], modules: [{ id: 'module-a', name: 'Module A', description: '', lorebook: [], regex: [] }] } },
    selIdState: { selId: 0 },
}))

import { alertConfirm } from 'src/ts/alert'
import { DBState } from 'src/ts/stores.svelte'
import { hostToolBridge, registerOwnedPluginMCP } from './hostToolHost'
import { registeredCustomPluginMCPs } from '../process/mcp/pluginmcp'
import { DiceClient } from '../process/mcp/dice'

const plugin = (identifier: string) => [
    { identifier, name: identifier, version: '1.0.0', description: '' },
    async () => [{ name: 'echo', description: 'Echo', inputSchema: {} }],
    async (_tool: string, args: unknown) => [{ type: 'text' as const, text: `${identifier}:${JSON.stringify(args)}` }],
] as const

describe('host tool bridge wiring', () => {
    afterEach(() => {
        registeredCustomPluginMCPs.clear()
        for (const key of Object.keys(state.MCPs)) delete state.MCPs[key]
        Reflect.deleteProperty(window, 'showDirectoryPicker')
    })

    it('initializes the module MCPs and leaves out the MCPs the caller registered', async () => {
        state.MCPs['https://remote.example/mcp'] = new DiceClient()
        await registerOwnedPluginMCP('plugin-a', ...plugin('plugin:a'))
        await registerOwnedPluginMCP('plugin-b', ...plugin('plugin:b'))

        const sources = new Set((await hostToolBridge.forPlugin('plugin-a').listTools()).tools.map((tool) => tool.source))
        expect(state.initializeMCPs).toHaveBeenCalled()
        expect(sources).toEqual(new Set([
            'https://remote.example/mcp', 'internal:risuai', 'internal:aiaccess', 'internal:googlesearch',
            'internal:graphmem', 'internal:dice', 'plugin:b',
        ]))
        const forB = await hostToolBridge.forPlugin('plugin-b').listTools()
        expect(forB.tools.map((tool) => tool.source)).toContain('plugin:a')
        await expect(hostToolBridge.forPlugin('plugin-b').callTool({ scope: forB.scope, source: 'plugin:a', name: 'echo', arguments: { x: 1 } }))
            .resolves.toEqual([{ type: 'text', text: 'plugin:a:{"x":1}' }])
    })

    it('offers the file system source only where the folder picker exists', async () => {
        expect((await hostToolBridge.forPlugin('caller').listTools()).tools.map((tool) => tool.source)).not.toContain('internal:fs')
        Object.defineProperty(window, 'showDirectoryPicker', { configurable: true, value: vi.fn() })
        expect((await hostToolBridge.forPlugin('caller').listTools()).tools.map((tool) => tool.source)).toContain('internal:fs')
    })

    it('keeps the host confirmation of internal:risuai writes', async () => {
        const access = hostToolBridge.forPlugin('caller')
        const { scope } = await access.listTools()

        await expect(access.callTool({ scope, source: 'internal:risuai', name: 'risu-set-module-info', arguments: { id: 'module-a', data: { name: 'Renamed' } } }))
            .resolves.toEqual([{ type: 'text', text: 'Access denied by user.' }])
        expect(alertConfirm).toHaveBeenCalledTimes(1)
        expect(DBState.db.modules[0].name).toBe('Module A')
        const rolled = await access.callTool({ scope, source: 'internal:dice', name: 'rollDice', arguments: { notation: '1d6' } })
        expect(rolled).toHaveLength(1)
    })
})
