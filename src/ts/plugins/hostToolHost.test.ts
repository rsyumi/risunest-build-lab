// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({
    moduleMcps: [] as string[],
    selected: { characterId: 'char-a', conversationId: 'conv-a' } as { characterId: string; conversationId: string } | null,
}))

// A remote MCP server the module set names; `held` keeps tool calls waiting until released.
const remote = vi.hoisted(() => ({
    url: 'https://remote.example/mcp',
    methods: [] as string[],
    held: null as Promise<void> | null,
}))

vi.mock(import('katex'), () => ({}))
vi.mock(import('src/ts/lite'), () => ({}))
vi.mock('src/ts/process/modules', () => ({ getModuleMcps: () => [...state.moduleMcps] }))
vi.mock('src/ts/globalApi.svelte', () => ({
    openURL: vi.fn(),
    fetchNative: vi.fn(async (url: string, init: { body?: string }) => {
        if (url !== remote.url) throw new Error(`Unexpected request to ${url}`)
        const message = JSON.parse(init.body ?? '{}')
        remote.methods.push(message.method)
        if (message.id === undefined) return new Response(null, { status: 202 })
        if (message.method === 'tools/call') await remote.held
        const result = message.method === 'initialize'
            ? { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'Remote', version: '1.0.0' } }
            : message.method === 'tools/list'
                ? { tools: [{ name: 'lookup', description: 'Lookup', inputSchema: {} }] }
                : { content: [{ type: 'text', text: `found ${JSON.stringify(message.params.arguments)}` }] }
        return new Response(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }), { headers: { 'Content-Type': 'application/json' } })
    }),
}))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', async (importOriginal) => ({
    ...(await importOriginal<Record<string, unknown>>()),
    captureSelectedConversationTarget: () => state.selected,
}))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(async () => false), alertInput: vi.fn(), alertError: vi.fn(), alertNormal: vi.fn() }))
vi.mock('src/ts/stores.svelte', () => ({
    DBState: { db: { characters: [], enabledModules: [], authRefreshes: [], modules: [{ id: 'module-a', name: 'Module A', description: '', lorebook: [], regex: [] }] } },
    selIdState: { selId: 0 },
}))

import { alertConfirm } from 'src/ts/alert'
import { DBState } from 'src/ts/stores.svelte'
import { hostToolBridge, registerOwnedPluginMCP } from './hostToolHost'
import { registeredCustomPluginMCPs } from '../process/mcp/pluginmcp'
import { callMCPTool, initializeMCPs, MCPs } from '../process/mcp/mcp'

const plugin = (identifier: string) => [
    { identifier, name: identifier, version: '1.0.0', description: '' },
    async () => [{ name: 'echo', description: 'Echo', inputSchema: {} }],
    async (_tool: string, args: unknown) => [{ type: 'text' as const, text: `${identifier}:${JSON.stringify(args)}` }],
] as const

function gate() {
    let open!: () => void
    const opened = new Promise<void>((resolve) => { open = resolve })
    return { opened, open }
}

describe('host tool bridge wiring', () => {
    afterEach(async () => {
        registeredCustomPluginMCPs.clear()
        Reflect.deleteProperty(window, 'showDirectoryPicker')
        state.moduleMcps = []
        remote.held = null
        // The registry drops every client the module set no longer names.
        await initializeMCPs()
        remote.methods.length = 0
        vi.mocked(alertConfirm).mockReset().mockResolvedValue(false)
        DBState.db.modules[0].name = 'Module A'
    })

    it('initializes the module MCPs and leaves out the MCPs the caller registered', async () => {
        state.moduleMcps = [remote.url]
        await registerOwnedPluginMCP('plugin-a', ...plugin('plugin:a'))
        await registerOwnedPluginMCP('plugin-b', ...plugin('plugin:b'))

        const sources = new Set((await hostToolBridge.forPlugin('plugin-a').listTools()).tools.map((tool) => tool.source))
        expect(remote.methods).toEqual(['initialize', 'notifications/initialized', 'tools/list'])
        expect(sources).toEqual(new Set([
            remote.url, 'internal:risuai', 'internal:aiaccess', 'internal:googlesearch',
            'internal:graphmem', 'internal:dice', 'plugin:b',
        ]))
        const forB = await hostToolBridge.forPlugin('plugin-b').listTools()
        expect(forB.tools.map((tool) => tool.source)).toContain('plugin:a')
        await expect(hostToolBridge.forPlugin('plugin-b').callTool({ scope: forB.scope, source: 'plugin:a', name: 'echo', arguments: { x: 1 } }))
            .resolves.toEqual([{ type: 'text', text: 'plugin:a:{"x":1}' }])
        await expect(hostToolBridge.forPlugin('plugin-b').callTool({ scope: forB.scope, source: remote.url, name: 'lookup', arguments: { q: 'b' } }))
            .resolves.toEqual([{ type: 'text', text: 'found {"q":"b"}' }])
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

    it('lists the tools while a generation waits on a remote tool call, without dropping its reply', async () => {
        state.moduleMcps = [remote.url]
        const held = gate()
        remote.held = held.opened
        const generation = callMCPTool('lookup', { q: 'held' })
        await vi.waitFor(() => expect(remote.methods).toContain('tools/call'))
        const client = MCPs[remote.url]

        const listed = await hostToolBridge.forPlugin('caller').listTools()
        expect(listed.tools.filter((tool) => tool.source === remote.url).map((tool) => tool.name)).toEqual(['lookup'])
        expect(MCPs[remote.url]).toBe(client)
        held.open()

        await expect(generation).resolves.toEqual([{ type: 'text', text: 'found {"q":"held"}' }])
        expect(remote.methods.filter((method) => method === 'initialize')).toHaveLength(1)
    })

    it('returns the reply of a call whose MCP a listing for another module set removed', async () => {
        state.moduleMcps = [remote.url]
        const held = gate()
        remote.held = held.opened
        const generation = callMCPTool('lookup', { q: 'moved' })
        await vi.waitFor(() => expect(remote.methods).toContain('tools/call'))

        state.moduleMcps = []
        const listed = await hostToolBridge.forPlugin('caller').listTools()
        expect(listed.tools.map((tool) => tool.source)).not.toContain(remote.url)
        expect(MCPs[remote.url]).toBeUndefined()
        held.open()

        await expect(generation).resolves.toEqual([{ type: 'text', text: 'found {"q":"moved"}' }])
    })

    it('applies an internal:risuai write the user confirms after a listing taken while it waited', async () => {
        const confirmation = gate()
        vi.mocked(alertConfirm).mockImplementation(async () => {
            await confirmation.opened
            return true
        })
        const generation = callMCPTool('risu-set-module-info', { id: 'module-a', data: { name: 'Renamed' } })
        await vi.waitFor(() => expect(alertConfirm).toHaveBeenCalledTimes(1))

        const listed = await hostToolBridge.forPlugin('caller').listTools()
        expect(listed.tools.some((tool) => tool.source === 'internal:risuai' && tool.name === 'risu-set-module-info')).toBe(true)
        confirmation.open()

        await generation
        expect(DBState.db.modules[0].name).toBe('Renamed')
        expect(alertConfirm).toHaveBeenCalledTimes(1)
    })
})
