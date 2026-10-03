import { beforeEach, expect, it, vi } from 'vitest'
import { readFileSync } from 'node:fs'
const f = vi.hoisted(() => ({ configure: vi.fn(), bind: vi.fn() }))
vi.mock('src/ts/storage/sync/serverSyncProduction', () => ({ configureServerSyncConnection: f.configure }))
vi.mock('src/ts/storage/sync/bindingRegistry', () => ({ bindSyncTarget: f.bind }))
import { connectServerOnboardingTarget } from './externalStorageOnboardingFlow'

const config = { endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }
beforeEach(() => { vi.clearAllMocks(); f.configure.mockResolvedValue(undefined); f.bind.mockResolvedValue({ kind: 'bound' }) })
it('configures the registered native target before binding exactly once', async () => {
    const order: string[] = []
    f.configure.mockImplementation(async () => { order.push('configure') })
    f.bind.mockImplementation(async () => { order.push('bind'); return { kind: 'bound' } })
    expect(await connectServerOnboardingTarget(config)).toEqual({ kind: 'bound' })
    expect(f.configure).toHaveBeenCalledExactlyOnceWith(config)
    expect(f.bind).toHaveBeenCalledExactlyOnceWith({ kind: 'server', connectionId: 'server' }, undefined)
    expect(order).toEqual(['configure', 'bind'])
})
it('passes explicit new-device mode to the single shared binding action', async () => {
    await connectServerOnboardingTarget(config, true)
    expect(f.bind).toHaveBeenCalledExactlyOnceWith({ kind: 'server', connectionId: 'server' }, { mode: 'new-device' })
})
it('preserves cancellation and rejects configuration errors without binding', async () => {
    const cancelled = { kind: 'cancelled' }
    f.bind.mockResolvedValue(cancelled)
    expect(await connectServerOnboardingTarget(config)).toBe(cancelled)
    f.bind.mockClear(); f.configure.mockRejectedValue(new Error('invalid-endpoint'))
    await expect(connectServerOnboardingTarget(config)).rejects.toThrow('invalid-endpoint')
    expect(f.bind).not.toHaveBeenCalled()
})
it('preserves binding failure instead of claiming onboarding completion', async () => {
    f.bind.mockRejectedValue(new Error('refresh-failed'))
    await expect(connectServerOnboardingTarget(config)).rejects.toThrow('refresh-failed')
})
it('wires the native-only entry and advances only a bound server screen', () => {
    const source = readFileSync('src/lib/Others/Onboarding/Onboarding.svelte', 'utf8')
    expect(source).toMatch(/\{#if isTauri\}\s*<button[^>]*onclick=\{\(\) => goTo\('sync-server'\)\}/)
    expect(source).toContain('<ServerSyncSettings connectTarget={onServerConnected} />')
    expect(source).toContain("if (outcome.kind === 'bound' && flow.state === 'sync-server')")
    expect(source).toContain("goToOnboardingState(flow, 'done', 'server')")
})
