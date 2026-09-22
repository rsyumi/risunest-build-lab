import { describe, expect, it, vi } from 'vitest'
import { classifyTestRequest, createTestNetworkPolicy } from './testNetwork'

describe('test network policy', () => {
    it('denies arbitrary HTTP and Realm paths even on an approved origin', () => {
        const origins = new Set(['http://127.0.0.1:4321'])
        expect(classifyTestRequest('https://example.test/api', origins)).toBeTruthy()
        expect(classifyTestRequest('http://127.0.0.1:4321/api', origins)).toBeNull()
        expect(classifyTestRequest('http://127.0.0.1:4322/api', origins)).toBeTruthy()
        expect(classifyTestRequest('http://127.0.0.1:4321/rs/fixture', origins)).toContain('RisuRealm')
    })
    it('reports swallowed failures and resets both violations and scoped permissions', () => {
        const policy = createTestNetworkPolicy()
        const revoke = policy.allowLoopbackOrigin('http://127.0.0.1:4321')
        policy.check('http://127.0.0.1:4321/api')
        revoke()
        try { policy.check('http://127.0.0.1:4321/api') } catch {}
        expect(() => policy.finish()).toThrow('Blocked test requests (1)')
        expect(() => policy.finish()).not.toThrow()
        policy.allowLoopbackOrigin('http://localhost:4321')
        policy.finish()
        expect(() => policy.check('http://localhost:4321/api')).toThrow()
        expect(() => policy.finish()).toThrow()
    })
    it('rejects non-loopback and non-origin exceptions', () => {
        const policy = createTestNetworkPolicy()
        for (const origin of ['https://example.test', 'http://localhost:4321/path', 'file:///tmp']) {
            expect(() => policy.allowLoopbackOrigin(origin)).toThrow()
        }
    })
    it('restores the installed guard after a synthetic fetch mock', async () => {
        const guard = globalThis.fetch
        vi.stubGlobal('fetch', vi.fn(async () => new Response('synthetic')))
        expect(await (await fetch('https://example.test')).text()).toBe('synthetic')
        vi.unstubAllGlobals()
        expect(globalThis.fetch).toBe(guard)
    })
})
