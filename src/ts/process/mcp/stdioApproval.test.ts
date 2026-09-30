import { beforeEach, expect, test, vi } from 'vitest'
const mocks = vi.hoisted(() => ({ confirm: vi.fn(), values: new Map<string, string>(), flush: vi.fn(async () => {}) }))
vi.mock('../../alert', () => ({ alertConfirm: mocks.confirm }))
vi.mock('src/lang', () => ({ language: { mcpStdioRegisterConfirm: 'Run this local MCP?' } }))
vi.mock('../../storage/deviceMarkers', () => ({ getDeviceMarkers: () => ({
    getItem: (key: string) => mocks.values.get(key) ?? null,
    setItem: (key: string, value: string) => { mocks.values.set(key, value) },
    flush: mocks.flush,
}) }))
beforeEach(() => { vi.resetModules(); vi.clearAllMocks(); mocks.values.clear(); mocks.confirm.mockResolvedValue(true) })

test('persists exact configuration approval across restart and canonical env ordering', async () => {
    const config = { command: 'node', args: ['synthetic.js'], env: { B: 'two', A: 'one' } }
    let { approveLocalMCP } = await import('./stdioApproval')
    expect(await approveLocalMCP(config)).toBe(true)
    expect(mocks.confirm).toHaveBeenCalledOnce()
    expect(mocks.flush).toHaveBeenCalledOnce()
    const stored = mocks.values.get('mcpStdioApprovals')!
    expect(JSON.parse(stored)).toEqual([expect.stringMatching(/^[a-f0-9]{64}$/)])
    expect(stored).not.toContain('synthetic.js')
    vi.resetModules()
    ;({ approveLocalMCP } = await import('./stdioApproval'))
    expect(await approveLocalMCP({ ...config, env: { A: 'one', B: 'two' } })).toBe(true)
    expect(mocks.confirm).toHaveBeenCalledOnce()
    expect(await approveLocalMCP({ ...config, args: ['changed.js'] })).toBe(true)
    expect(mocks.confirm).toHaveBeenCalledTimes(2)
})

test('declines one configuration for this session without blocking another', async () => {
    const { approveLocalMCP } = await import('./stdioApproval')
    mocks.confirm.mockResolvedValueOnce(false)
    const config = { command: 'node', args: ['declined.js'] }
    expect(await approveLocalMCP(config)).toBe(false)
    expect(await approveLocalMCP(config)).toBe(false)
    expect(mocks.confirm).toHaveBeenCalledOnce()
    expect(await approveLocalMCP({ command: 'node', args: ['accepted.js'] })).toBe(true)
    expect(mocks.confirm).toHaveBeenCalledTimes(2)
})
