import { beforeEach, describe, expect, it, vi } from 'vitest'
import { spawnMCPProcess, writeMCPMessage } from './stdio'

const mocks = vi.hoisted(() => ({
    configured: [] as string[],
    stdout: undefined as undefined | ((line: string) => void),
    write: vi.fn(),
    spawn: vi.fn(),
    create: vi.fn(),
    input: vi.fn(),
    confirm: vi.fn(),
    error: vi.fn(),
    database: { modules: [] as Array<{ mcp: { url: string } }> },
    desktop: true,
}))
vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => mocks.database, getCurrentChat: vi.fn() }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: {} } }))
vi.mock('../modules', () => ({ getModuleMcps: () => [...mocks.configured] }))
vi.mock('src/ts/alert', () => ({ alertError: mocks.error, alertInput: mocks.input, alertConfirm: mocks.confirm, alertNormal: vi.fn() }))
vi.mock('src/ts/platform', () => ({ get isTauriDesktop() { return mocks.desktop } }))
vi.mock('src/ts/util', () => ({ sleep: async () => {} }))
vi.mock('./pluginmcp', () => ({ registeredCustomPluginMCPs: new Map() }))
vi.mock('@tauri-apps/plugin-shell', () => ({ Command: { create: mocks.create } }))
vi.mock('./mcplib', () => ({
    MCPClient: class {
        serverInfo = { serverInfo: { name: 'Synthetic MCP', version: '1' } }
        customTransport?: { send(message: unknown): Promise<void> }
        async checkHandshake() {
            await this.customTransport?.send({ jsonrpc: '2.0', id: 'init', method: 'initialize' })
            return {}
        }
    },
}))

describe('desktop MCP stdio framing', () => {
    beforeEach(() => {
        vi.resetModules()
        vi.clearAllMocks()
        mocks.stdout = undefined
        mocks.configured = []
        mocks.database.modules = []
        mocks.desktop = true
        mocks.confirm.mockResolvedValue(false)
        mocks.create.mockImplementation(() => ({
            stdout: { on: (_event: string, listener: (line: string) => void) => { mocks.stdout = listener } },
            spawn: mocks.spawn,
        }))
        mocks.spawn.mockResolvedValue({ write: mocks.write, kill: vi.fn() })
    })

    it('does not start or register a directly entered local MCP when confirmation is declined', async () => {
        const configuration = { command: 'node', args: ['synthetic.js'], env: { PATH: '/synthetic/bin' } }
        mocks.input.mockResolvedValue(`stdio:${JSON.stringify(configuration)}`)
        const { importMCPModule } = await import('./mcp')
        await importMCPModule()
        expect(mocks.confirm).toHaveBeenCalledOnce()
        expect(mocks.confirm.mock.calls[0][0]).toContain(JSON.stringify(configuration, null, 2))
        expect(mocks.create).not.toHaveBeenCalled()
        expect(mocks.spawn).not.toHaveBeenCalled()
        expect(mocks.database.modules).toEqual([])
    })

    it('confirms before starting a directly registered MCP and does not ask again on initialization', async () => {
        const configuration = { command: 'node', args: ['synthetic.js'] }
        const url = `stdio:${JSON.stringify(configuration)}`
        mocks.input.mockResolvedValue(url)
        mocks.confirm.mockResolvedValue(true)
        mocks.write.mockImplementation(async (line: string) => {
            const message = JSON.parse(line)
            if (message.method === 'ping') mocks.stdout?.(JSON.stringify({ jsonrpc: '2.0', id: message.id, result: {} }))
        })
        const { importMCPModule, initializeMCPs, MCPs } = await import('./mcp')
        MCPs['internal:risuai'] = { checkHandshake: async () => ({}) } as never
        await importMCPModule()
        expect(mocks.error).not.toHaveBeenCalled()
        expect(mocks.confirm).toHaveBeenCalledOnce()
        expect(mocks.confirm.mock.invocationCallOrder[0]).toBeLessThan(mocks.create.mock.invocationCallOrder[0])
        expect(mocks.spawn).toHaveBeenCalledOnce()
        expect(mocks.database.modules).toEqual([expect.objectContaining({ mcp: { url } })])

        mocks.configured = [url]
        MCPs['internal:risuai'] = { checkHandshake: async () => ({}) } as never
        await initializeMCPs()
        expect(mocks.confirm).toHaveBeenCalledOnce()
        expect(mocks.spawn).toHaveBeenCalledOnce()
    })

    it.each(['https://synthetic.invalid/mcp', 'stdio:{"url":"https://synthetic.invalid/mcp"}'])('registers %s without a local process confirmation', async url => {
        mocks.input.mockResolvedValue(url)
        const { importMCPModule, MCPs } = await import('./mcp')
        MCPs['internal:risuai'] = { checkHandshake: async () => ({}) } as never
        await importMCPModule()
        expect(mocks.confirm).not.toHaveBeenCalled()
        expect(mocks.spawn).not.toHaveBeenCalled()
        expect(mocks.error).not.toHaveBeenCalled()
        expect(mocks.database.modules).toEqual([expect.objectContaining({ mcp: { url } })])
    })

    it('keeps direct local MCP registration unavailable outside desktop', async () => {
        mocks.desktop = false
        mocks.input.mockResolvedValue('stdio:{"command":"node","args":["synthetic.js"]}')
        const { importMCPModule } = await import('./mcp')
        await importMCPModule()
        expect(mocks.confirm).not.toHaveBeenCalled()
        expect(mocks.spawn).not.toHaveBeenCalled()
        expect(mocks.database.modules).toEqual([])
        expect(mocks.error).toHaveBeenCalledWith(expect.objectContaining({ message: 'stdio MCPs are only supported in Local Version' }))
    })

    it('writes exactly one complete JSON line including messages containing newlines', async () => {
        const child = { write: vi.fn(async (_data: string) => {}) }
        const first = { jsonrpc: '2.0' as const, id: 1, method: 'ping' }
        const second = { jsonrpc: '2.0' as const, id: 2, method: 'tools/call', params: { text: 'first\nsecond\r\n한글' } }
        await writeMCPMessage(child, first)
        await writeMCPMessage(child, second)
        const wire = child.write.mock.calls.map(([line]) => line).join('')
        expect(wire.endsWith('\n')).toBe(true)
        expect(wire.split('\n').slice(0, -1).map(line => JSON.parse(line))).toEqual([first, second])
        expect(wire.split('\n')).toHaveLength(3)
    })

    it('uses the same framed writes for startup, handshake, and normal sends without changing PATH', async () => {
        const configuration = { command: 'node', args: ['synthetic.js'], env: { PATH: '/synthetic/bin' } }
        const url = `stdio:${JSON.stringify(configuration)}`
        mocks.configured = [url]
        let unread = ''
        const received: Array<Record<string, unknown>> = []
        mocks.write.mockImplementation(async (value: string) => {
            unread += value
            while (unread.includes('\n')) {
                const newline = unread.indexOf('\n')
                const message = JSON.parse(unread.slice(0, newline))
                unread = unread.slice(newline + 1)
                received.push(message)
                if (message.method === 'ping') mocks.stdout?.(JSON.stringify({ jsonrpc: '2.0', id: message.id, result: {} }))
            }
        })
        const { initializeMCPs, MCPs } = await import('./mcp')
        MCPs['internal:risuai'] = { checkHandshake: async () => ({}) } as never
        await initializeMCPs()
        const normal = {
            jsonrpc: '2.0' as const, id: 'normal', method: 'tools/list', params: { text: 'one\ntwo' },
        }
        await (MCPs[url] as import('./mcplib').MCPClient).customTransport!.send(normal)
        expect(received.map(message => message.method)).toEqual(['ping', 'initialize', 'tools/list'])
        expect(unread).toBe('')
        expect(mocks.create).toHaveBeenCalledWith('node', ['synthetic.js'], { env: configuration.env })
        expect(mocks.spawn).toHaveBeenCalledOnce()
    })

    it.each(['spawn ENOENT', 'No such file or directory (os error 2)', 'The system cannot find the file specified.'])
    ('explains an unavailable executable without trying another command: %s', async detail => {
        const cause = new Error(detail)
        const spawn = vi.fn().mockRejectedValue(cause)
        await expect(spawnMCPProcess('node', spawn)).rejects.toMatchObject({
            message: expect.stringContaining('env.PATH'), cause,
        })
        expect(spawn).toHaveBeenCalledOnce()
    })

    it('preserves unrelated launch errors without calling them missing tools', async () => {
        await expect(spawnMCPProcess('node', async () => { throw new Error('Permission denied') }))
            .rejects.toThrow('Permission denied')
        await expect(spawnMCPProcess('node', async () => { throw new Error('Permission denied') }))
            .rejects.not.toThrow('env.PATH')
    })
})
