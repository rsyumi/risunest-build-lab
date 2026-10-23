import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { compressSync } from 'fflate'
import * as transport from './coldstorage.svelte'
import { importColdPluginStorage } from './coldPayloadExpansion'

const mocks = vi.hoisted(() => ({
    database: { characters: [] as any[], account: { token: 'synthetic-risu-auth' } },
}))

vi.mock('../characterCards', () => ({ hubURL: 'https://hub.invalid' }) satisfies Partial<typeof import('../characterCards')>)
vi.mock('../globalApi.svelte', () => ({ forageStorage: { isAccount: true }, fetchNative: vi.fn() }))
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('../stores.svelte', () => ({ DBState: { db: mocks.database } }))
vi.mock('../util', () => ({
    getKeypairStore: async () => null,
    saveKeypairStore: vi.fn(),
}) satisfies Partial<typeof import('../util')>)
vi.mock('../alert', () => ({
    alertClear: vi.fn(),
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertWait: vi.fn(),
}))

const coldUrl = 'https://hub.invalid/hub/account/coldstorage'

function coldBody(value: unknown): ArrayBuffer {
    return compressSync(new TextEncoder().encode(JSON.stringify(value))).buffer as ArrayBuffer
}

function serveCold(items: Record<string, () => Response>) {
    const fetch = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
        expect(String(url)).toBe(coldUrl)
        const headers = new Headers(init?.headers)
        expect(headers.get('x-risu-auth')).toBe('synthetic-risu-auth')
        const respond = items[headers.get('x-risu-key') ?? '']
        return respond ? respond() : new Response(null, { status: 404 })
    })
    vi.stubGlobal('fetch', fetch)
    return fetch
}

beforeEach(() => {
    mocks.database.characters = []
    vi.stubGlobal('localStorage', { getItem: () => null, setItem: vi.fn(), removeItem: vi.fn() })
})

afterEach(() => {
    vi.unstubAllGlobals()
})

describe('official account cold storage read', () => {
    it('reads the exact remote key and decompresses a successful payload', async () => {
        const value = { character: { chaId: 'synthetic-character' } }
        const signal = new AbortController().signal
        const fetch = serveCold({ 'cold-a': () => new Response(coldBody(value), { status: 200 }) })

        await expect(transport.getAccountColdStorageItem('cold-a', signal)).resolves.toEqual({ kind: 'present', value })
        expect(fetch).toHaveBeenCalledWith(coldUrl, expect.objectContaining({ method: 'GET', signal }))
    })

    it('reports a stored JSON null as present', async () => {
        serveCold({ 'cold-null': () => new Response(coldBody(null), { status: 200 }) })

        await expect(transport.getAccountColdStorageItem('cold-null')).resolves.toEqual({ kind: 'present', value: null })
    })

    it.each([
        [404, 'missing'],
        [403, 'unreadable'],
        [503, 'unreadable'],
    ])('maps status %i to %s and discards the body', async (status, kind) => {
        const cancel = vi.fn()
        serveCold({ 'cold-status': () => new Response(new ReadableStream({ cancel }), { status }) })

        await expect(transport.getAccountColdStorageItem('cold-status')).resolves.toEqual({ kind })
        expect(cancel).toHaveBeenCalledOnce()
    })

    it('reports an undecodable payload as unreadable', async () => {
        serveCold({ 'cold-broken': () => new Response(new Uint8Array([1, 2, 3]), { status: 200 }) })

        await expect(transport.getAccountColdStorageItem('cold-broken')).resolves.toEqual({ kind: 'unreadable' })
    })

    it('reads a payload whose decoded JSON fills the limit and reports one byte more as too large', async () => {
        const value = 'x'.repeat(4096)
        const decodedBytes = JSON.stringify(value).length
        serveCold({ 'cold-large': () => new Response(coldBody(value), { status: 200 }) })

        await expect(transport.getAccountColdStorageItem('cold-large', undefined, decodedBytes)).resolves.toEqual({ kind: 'present', value })
        await expect(transport.getAccountColdStorageItem('cold-large', undefined, decodedBytes - 1)).resolves.toEqual({ kind: 'too-large' })
    })

    it('stops reading a response body over the limit and reports it as too large', async () => {
        const cancel = vi.fn()
        const pulled = vi.fn()
        serveCold({
            'cold-oversized': () => new Response(new ReadableStream({
                pull(controller) {
                    pulled()
                    if (pulled.mock.calls.length > 64) controller.close()
                    else controller.enqueue(new Uint8Array(1024))
                },
                cancel,
            }), { status: 200 }),
        })

        await expect(transport.getAccountColdStorageItem('cold-oversized', undefined, 4096)).resolves.toEqual({ kind: 'too-large' })
        expect(cancel).toHaveBeenCalledOnce()
        expect(pulled.mock.calls.length).toBeLessThan(10)
    })

    it('preserves read AbortError identity', async () => {
        const readAbort = new DOMException('read cancelled', 'AbortError')
        vi.stubGlobal('fetch', vi.fn(async () => { throw readAbort }))

        await expect(transport.getAccountColdStorageItem('cold-read-abort')).rejects.toBe(readAbort)
    })

    it('imports a stored null plugin value and omits a missing one', async () => {
        const storedNull = '00000000-0000-4000-8000-0000000000a1'
        const absent = '00000000-0000-4000-8000-0000000000a2'
        serveCold({ [storedNull]: () => new Response(coldBody(null), { status: 200 }) })
        const database = { pluginCustomStorage: { _coldplugin: { kept: storedNull, lost: absent } } as Record<string, unknown> }

        await expect(importColdPluginStorage(database, (key) => transport.getAccountColdStorageItem(key))).resolves.toEqual({ omitted: 1, tooLarge: 0 })
        expect(database.pluginCustomStorage).toEqual({ kept: null })
        expect(Object.hasOwn(database.pluginCustomStorage, 'lost')).toBe(false)
    })

    it('never issues a cold storage write request', async () => {

        expect(Object.keys(transport).filter((name) => /^set/.test(name))).toEqual([])
    })
})
