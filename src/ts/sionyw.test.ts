import { webcrypto } from 'node:crypto'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { Database } from './storage/database.svelte'

const f = vi.hoisted(() => ({
    refresh: vi.fn(),
    keys: null as CryptoKeyPair | null,
    now: 1_800_000_000_000,
}))

vi.mock('./characterCards', () => ({ hubURL: 'https://hub.invalid' }) satisfies Partial<typeof import('./characterCards')>)
vi.mock('./globalApi.svelte', () => ({ fetchNative: vi.fn() }) satisfies Partial<typeof import('./globalApi.svelte')>)
vi.mock('./stores.svelte', () => ({ DBState: { db: {} as Database } }) satisfies Partial<typeof import('./stores.svelte')>)
vi.mock('./platform', async () => (await import('./platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('./util', () => ({
    getKeypairStore: async () => f.keys,
    saveKeypairStore: vi.fn(),
}) satisfies Partial<typeof import('./util')>)
vi.mock('openid-client', async (importOriginal) => {
    const actual = await importOriginal<typeof import('openid-client')>()
    return {
        ...actual,
        discovery: async () => new actual.Configuration({ issuer: 'https://account.sionyw.com/' }, 'synthetic-client'),
        refreshTokenGrant: f.refresh,
    } satisfies typeof import('openid-client')
})

beforeEach(async () => {
    vi.resetModules()
    f.refresh.mockReset()
    f.now = 1_800_000_000_000
    vi.spyOn(Date, 'now').mockImplementation(() => f.now)
    vi.stubGlobal('crypto', webcrypto)
    f.keys = await webcrypto.subtle.generateKey(
        { name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign', 'verify'],
    ) as unknown as CryptoKeyPair
    localStorage.clear()
    localStorage.setItem('oauthData.json', Buffer.from(JSON.stringify({
        refresh_token: 'synthetic-original', client_id: 'synthetic-client', client_secret: 'synthetic-secret',
    })).toString('base64'))
})

afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
    localStorage.clear()
})

it.each([true, false])('uses the persisted refresh token after expiry (rotation: %s)', async (rotated) => {
    const expectedRefresh = rotated ? 'synthetic-rotated' : 'synthetic-original'
    f.refresh.mockImplementationOnce(async () => ({
        access_token: 'synthetic-first-access', expires_in: 3600, token_type: 'DPoP',
        ...(rotated ? { refresh_token: expectedRefresh } : {}),
    }))
    f.refresh.mockImplementationOnce(async (_config, refreshToken) => ({
        access_token: refreshToken === expectedRefresh ? 'synthetic-second-access' : 'synthetic-wrong-access',
        expires_in: 3600, token_type: 'DPoP',
    }))
    vi.stubGlobal('fetch', vi.fn(async (_url, options: RequestInit) => {
        const authorization = new Headers(options.headers).get('Authorization')
        return new Response(authorization === 'Bearer synthetic-second-access' ? 'renewed' : 'first')
    }))
    const { fetchProtectedResource } = await import('./sionyw')

    await fetchProtectedResource('/synthetic-resource')
    expect(JSON.parse(Buffer.from(localStorage.getItem('oauthData.json')!, 'base64').toString())).toEqual({
        refresh_token: expectedRefresh, client_id: 'synthetic-client', client_secret: 'synthetic-secret',
    })
    f.now += 3_600_000
    const renewed = await fetchProtectedResource('/synthetic-resource')

    expect(await renewed.text()).toBe('renewed')
})
