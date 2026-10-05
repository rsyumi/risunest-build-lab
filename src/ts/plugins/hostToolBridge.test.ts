import { describe, expect, it, vi } from 'vitest'
import type { MCPTool } from '../process/mcp/mcplib'
import { createHostToolBridge, normalizeHostToolCallInput, type HostToolSource } from './hostToolBridge'

function source(name: string, tools: MCPTool[], handshake = vi.fn(async () => undefined)) {
    return {
        serverInfo: { serverInfo: { name } },
        checkHandshake: handshake,
        getToolList: vi.fn(async () => tools),
        callTool: vi.fn(async (tool: string, args: unknown) => [{ type: 'text' as const, text: `${name}:${tool}:${JSON.stringify(args)}` }]),
    } satisfies HostToolSource
}

const tool = (name: string, extra: Partial<MCPTool> = {}): MCPTool => ({ name, description: `${name} tool`, inputSchema: { type: 'object' }, ...extra })

function setup() {
    let selected: { characterId: string; conversationId: string } | null = { characterId: 'char-a', conversationId: 'conv-a' }
    const host = new Map<string, HostToolSource>([
        ['internal:graphmem', source('Graph', [tool('graph_add')])],
        ['internal:risuai', source('Risu', [tool('risu-list-modules', { annotations: { readOnlyHint: true } })])],
        ['stdio:{"command":"tool"}', source('Local', [tool('local_run')])],
        ['https://remote.example/mcp', source('Remote', [tool('search')])],
        ['plugin:module', source('Module plugin', [tool('module_tool')])],
    ])
    const builtIns: Record<string, ReturnType<typeof source>> = {
        'internal:risuai': source('Second Risu', [tool('never')]),
        'internal:dice': source('Dice', [tool('roll')]),
        'internal:fs': source('Files', [tool('read_file')]),
    }
    const plugins = new Map<string, HostToolSource>([
        ['plugin:module', host.get('plugin:module')!],
        ['plugin:other', source('Other plugin', [tool('search')])],
        ['plugin:mine', source('My plugin', [tool('mine')])],
    ])
    const owners: Record<string, string> = { 'plugin:mine': 'caller', 'plugin:other': 'someone', 'plugin:module': 'someone' }
    const dependencies = {
        loadHostSources: vi.fn(async () => host),
        builtInSourceIds: () => Object.keys(builtIns),
        createBuiltInSource: vi.fn(async (id: string) => builtIns[id] ?? null),
        pluginSources: () => plugins,
        pluginSourceOwner: (id: string) => owners[id],
        captureSelectedConversation: () => selected,
    }
    const bridge = createHostToolBridge(dependencies)
    return {
        host, builtIns, plugins, dependencies,
        access: bridge.forPlugin('caller'),
        select(next: typeof selected) { selected = next },
    }
}

describe('host tool bridge', () => {
    it('lists every source kind by source and leaves out the caller\'s own MCP', async () => {
        const { access, builtIns, dependencies } = setup()
        const listed = await access.listTools()

        expect(listed.tools.map((entry) => [entry.source, entry.name])).toEqual([
            ['internal:graphmem', 'graph_add'],
            ['internal:risuai', 'risu-list-modules'],
            ['stdio:{"command":"tool"}', 'local_run'],
            ['https://remote.example/mcp', 'search'],
            ['plugin:module', 'module_tool'],
            ['internal:dice', 'roll'],
            ['internal:fs', 'read_file'],
            ['plugin:other', 'search'],
        ])
        expect(listed.tools[1]).toEqual({
            source: 'internal:risuai', sourceName: 'Risu', name: 'risu-list-modules', description: 'risu-list-modules tool',
            inputSchema: { type: 'object' }, annotations: { readOnlyHint: true },
        })
        expect(dependencies.loadHostSources).toHaveBeenCalledTimes(1)
        expect(dependencies.createBuiltInSource).not.toHaveBeenCalledWith('internal:risuai')
        expect(builtIns['internal:fs'].checkHandshake).not.toHaveBeenCalled()
        await access.listTools()
        expect(dependencies.createBuiltInSource).toHaveBeenCalledTimes(2)
    })

    it('skips a source that cannot list its tools', async () => {
        const { access, host } = setup()
        vi.mocked(host.get('https://remote.example/mcp')!.getToolList).mockRejectedValueOnce(new Error('offline'))
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined)
        try {
            expect((await access.listTools()).tools.map((entry) => entry.source)).not.toContain('https://remote.example/mcp')
        } finally {
            warn.mockRestore()
        }
    })

    it('calls the addressed source when two sources share a tool name', async () => {
        const { access, host, plugins } = setup()
        const { scope } = await access.listTools()

        await expect(access.callTool({ scope, source: 'plugin:other', name: 'search', arguments: { q: 'x' } }))
            .resolves.toEqual([{ type: 'text', text: 'Other plugin:search:{"q":"x"}' }])
        expect(host.get('https://remote.example/mcp')!.callTool).not.toHaveBeenCalled()
        await access.callTool({ scope, source: 'https://remote.example/mcp', name: 'search', arguments: {} })
        expect(plugins.get('plugin:other')!.callTool).toHaveBeenCalledTimes(1)
        expect(host.get('https://remote.example/mcp')!.callTool).toHaveBeenCalledTimes(1)
    })

    it('rejects the caller\'s own MCP, an unknown source and a tool the source lacks', async () => {
        const { access, plugins } = setup()
        const { scope } = await access.listTools()

        await expect(access.callTool({ scope, source: 'plugin:mine', name: 'mine', arguments: {} })).rejects.toThrow(/not available/)
        await expect(access.callTool({ scope, source: 'plugin:missing', name: 'mine', arguments: {} })).rejects.toThrow(/not available/)
        await expect(access.callTool({ scope, source: 'plugin:other', name: 'graph_add', arguments: {} })).rejects.toThrow(/has no tool graph_add/)
        expect(plugins.get('plugin:mine')!.callTool).not.toHaveBeenCalled()
    })

    it('rejects a call after the selected conversation changed and runs one in the listed conversation', async () => {
        const { access, host, select } = setup()
        const { scope } = await access.listTools()
        const call = () => access.callTool({ scope, source: 'internal:graphmem', name: 'graph_add', arguments: {} })

        select({ characterId: 'char-a', conversationId: 'conv-b' })
        await expect(call()).rejects.toThrow(/selected conversation changed/)
        select(null)
        await expect(call()).rejects.toThrow(/selected conversation changed/)
        expect(host.get('internal:graphmem')!.callTool).not.toHaveBeenCalled()

        select({ characterId: 'char-a', conversationId: 'conv-a' })
        await expect(call()).resolves.toHaveLength(1)
        expect(host.get('internal:graphmem')!.callTool).toHaveBeenCalledTimes(1)
    })

    it('rejects a stale-scope call before the built-in handshake, the source lookup or the tool list', async () => {
        const { access, builtIns, dependencies, select } = setup()
        const { scope } = await access.listTools()
        const files = builtIns['internal:fs']
        vi.mocked(dependencies.loadHostSources).mockClear()
        files.getToolList.mockClear()

        select({ characterId: 'char-a', conversationId: 'conv-b' })
        await expect(access.callTool({ scope, source: 'internal:fs', name: 'read_file', arguments: {} }))
            .rejects.toThrow(/selected conversation changed/)
        expect(files.checkHandshake).not.toHaveBeenCalled()
        expect(files.getToolList).not.toHaveBeenCalled()
        expect(dependencies.loadHostSources).not.toHaveBeenCalled()
    })

    it('rejects a call whose conversation changed while the built-in shook hands', async () => {
        const { access, builtIns, select } = setup()
        const { scope } = await access.listTools()
        builtIns['internal:fs'].checkHandshake.mockImplementationOnce(async () => {
            select({ characterId: 'char-a', conversationId: 'conv-b' })
        })
        await expect(access.callTool({ scope, source: 'internal:fs', name: 'read_file', arguments: {} }))
            .rejects.toThrow(/selected conversation changed/)
        expect(builtIns['internal:fs'].callTool).not.toHaveBeenCalled()
    })

    it('shakes hands with a built-in source on its first call and again after a failed one', async () => {
        const { access, builtIns } = setup()
        const { scope } = await access.listTools()
        const handshake = vi.mocked(builtIns['internal:fs'].checkHandshake)
        handshake.mockRejectedValueOnce(new Error('folder selection cancelled'))
        const call = () => access.callTool({ scope, source: 'internal:fs', name: 'read_file', arguments: {} })

        await expect(call()).rejects.toThrow('folder selection cancelled')
        expect(builtIns['internal:fs'].callTool).not.toHaveBeenCalled()
        await expect(call()).resolves.toHaveLength(1)
        await expect(call()).resolves.toHaveLength(1)
        expect(handshake).toHaveBeenCalledTimes(2)
    })

    it('honors the signal until the call starts and discards a result that arrives after it fires', async () => {
        const { access, host } = setup()
        const { scope } = await access.listTools()
        const graph = vi.mocked(host.get('internal:graphmem')!.callTool)

        const before = new AbortController()
        before.abort()
        await expect(access.callTool({ scope, source: 'internal:graphmem', name: 'graph_add', arguments: {} }, before.signal))
            .rejects.toMatchObject({ name: 'AbortError' })
        expect(graph).not.toHaveBeenCalled()

        const during = new AbortController()
        graph.mockImplementationOnce(async () => {
            during.abort()
            return [{ type: 'text', text: 'late' }]
        })
        await expect(access.callTool({ scope, source: 'internal:graphmem', name: 'graph_add', arguments: {} }, during.signal))
            .rejects.toMatchObject({ name: 'AbortError' })
        expect(graph).toHaveBeenCalledTimes(1)
    })

    it.each([
        ['no input', undefined],
        ['a missing scope', { source: 's', name: 'n' }],
        ['an empty source', { scope: 'x', source: '', name: 'n' }],
        ['a non-string name', { scope: 'x', source: 's', name: 1 }],
    ])('rejects %s', (_name, input) => {
        expect(() => normalizeHostToolCallInput(input)).toThrow(TypeError)
    })
})
