import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import type { PlatformShape } from './platform.testSupport'

const state = vi.hoisted(() => ({
    platform: { runtime: 'web', os: 'windows' } as PlatformShape,
    database: { usePlainFetch: true },
    blobStore: null as any,
}))

vi.mock('./platform', async () => (await import('./platform.testSupport')).platformModule(() => state.platform))
vi.mock('./storage/platformBlobStore', async () => (await import('./globalApi.testSupport')).platformBlobStoreModule(async () => state.blobStore))
vi.mock('./storage/autoStorage', async () => (await import('./globalApi.testSupport')).autoStorageModule())
vi.mock('./characterCards', async () => (await import('./globalApi.testSupport')).characterCardsModule())
vi.mock('./util', async () => (await import('./globalApi.testSupport')).utilModule())
vi.mock('./storage/database.svelte', async () => (await import('./globalApi.testSupport')).databaseModule(() => state.database))
vi.mock('./stores.svelte', async () => (await import('./globalApi.testSupport')).storesModule({ characters: [] }))
vi.mock('./alert', async () => (await import('./globalApi.testSupport')).alertModule())
vi.mock('./storage/persistentDataRuntime.svelte', async () => (await import('./globalApi.testSupport')).persistentDataRuntimeModule())
vi.mock('./storage/persistentSaveNotifications', async () => (await import('./globalApi.testSupport')).persistentSaveNotificationsModule())
vi.mock('./storage/databasePreparation', async () => (await import('./globalApi.testSupport')).databasePreparationModule())
vi.mock('./storage/risuSave', async () => (await import('./globalApi.testSupport')).risuSaveModule())
vi.mock('./storage/defaultPrompts', async () => (await import('./globalApi.testSupport')).defaultPromptsModule())
vi.mock('./process/modules', async () => (await import('./globalApi.testSupport')).modulesModule())
vi.mock('./process/coldstorageData', async () => (await import('./globalApi.testSupport')).coldStorageDataModule())
vi.mock('./plugins/plugins.svelte', async () => (await import('./globalApi.testSupport')).pluginsModule())
vi.mock('./parser/parser.svelte', async () => (await import('./parser/parser.testSupport')).parserStubModule())
vi.mock('./drive/accounter', async () => (await import('./globalApi.testSupport')).accounterModule())
vi.mock('./observer.svelte', async () => (await import('./globalApi.testSupport')).observerModule())
vi.mock('./characters', async () => (await import('./globalApi.testSupport')).charactersModule())
vi.mock('./hotkey', async () => (await import('./globalApi.testSupport')).hotkeyModule())
vi.mock('./gui/animation', async () => (await import('./globalApi.testSupport')).animationModule())
vi.mock('./gui/colorscheme', async () => (await import('./globalApi.testSupport')).colorschemeModule())
vi.mock('./gui/guisize', async () => (await import('./globalApi.testSupport')).guisizeModule())
vi.mock('streamsaver', async () => (await import('./globalApi.testSupport')).streamsaverModule())
vi.mock('@tauri-apps/plugin-fs', async () => (await import('./globalApi.testSupport')).tauriFsModule())
vi.mock('@tauri-apps/api/core', async () => (await import('./globalApi.testSupport')).tauriCoreModule())
vi.mock('@tauri-apps/api/path', async () => (await import('./globalApi.testSupport')).tauriPathModule())
vi.mock('@tauri-apps/plugin-shell', async () => (await import('./globalApi.testSupport')).tauriShellModule())
vi.mock('@tauri-apps/plugin-dialog', async () => (await import('./globalApi.testSupport')).tauriDialogModule())
vi.mock('@tauri-apps/api/webviewWindow', async () => (await import('./globalApi.testSupport')).webviewWindowModule())
vi.mock('@tauri-apps/plugin-http', async () => (await import('./globalApi.testSupport')).tauriHttpModule())

import { fetch as pluginFetch } from '@tauri-apps/plugin-http'
import { fetchNative, getFetchLogs, globalFetch } from './globalApi.svelte'

describe('network requests without application URL restrictions', () => {
    beforeEach(() => {
        state.platform = { runtime: 'web', os: 'windows' }
        state.database.usePlainFetch = true
        vi.stubGlobal(
            'fetch',
            vi.fn(
                async () => new Response('{"fixture":true}', { status: 200 }),
            ),
        )
    })

    afterEach(() => vi.unstubAllGlobals())

    test.each([undefined, 'local_network'] as const)(
        'globalFetch attempts browser LAN requests (%s)',
        async (networkRoute) => {
            const url = 'http://localhost:11434/api/chat'
            const response = await globalFetch(url, {
                method: 'GET',
                networkRoute,
            })
            expect(response).toMatchObject({
                ok: true,
                status: 200,
                data: { fixture: true },
            })
            expect(fetch).toHaveBeenCalledWith(
                new URL(url),
                expect.objectContaining({ method: 'GET' }),
            )
        },
    )

    test.each([undefined, 'local_network'] as const)(
        'nativeFetch routes browser LAN requests directly even with the public proxy enabled (%s)',
        async (networkRoute) => {
            state.database.usePlainFetch = false
            const url = 'http://192.168.1.2:8080/v1/chat'
            const response = await fetchNative(url, {
                method: 'GET',
                networkRoute,
            })
            expect(response.status).toBe(200)
            expect(fetch).toHaveBeenCalledWith(url, expect.anything())
        },
    )

    test('nativeFetch accepts omitted options and empty POST bodies', async () => {
        await fetchNative('https://api.example.invalid/')
        expect(fetch).toHaveBeenLastCalledWith(
            'https://api.example.invalid/',
            expect.objectContaining({ method: 'GET', body: undefined }),
        )
        await fetchNative('https://api.example.invalid/', { method: 'POST' })
        expect(fetch).toHaveBeenLastCalledWith(
            'https://api.example.invalid/',
            expect.objectContaining({ method: 'POST', body: undefined }),
        )
    })

    test('nativeFetch preserves a DELETE payload and supports PATCH', async () => {
        for (const method of ['DELETE', 'PATCH']) {
            await fetchNative('https://api.example.invalid/', {
                method,
                body: 'fixture',
            })
            expect(fetch).toHaveBeenLastCalledWith(
                'https://api.example.invalid/',
                expect.objectContaining({
                    method,
                    body: new TextEncoder().encode('fixture'),
                }),
            )
        }
    })

    test('nativeFetch retains implicit POST with a body and does not mutate caller options', async () => {
        const options = { body: 'fixture' }
        await fetchNative('https://api.example.invalid/', options)
        expect(fetch).toHaveBeenCalledWith(
            'https://api.example.invalid/',
            expect.objectContaining({ method: 'POST' }),
        )
        expect(options).toEqual({ body: 'fixture' })
    })

    test('forwards browser network failures instead of replacing them with a URL policy error', async () => {
        const error = new TypeError('synthetic browser network error')
        vi.mocked(fetch).mockRejectedValue(error)
        await expect(
            fetchNative('http://localhost:8080/', { method: 'GET' }),
        ).rejects.toBe(error)
    })
})

describe('Tauri route request bodies', () => {
    beforeEach(() => {
        state.platform = { runtime: 'tauri', os: 'windows' }
        state.database.usePlainFetch = false
        vi.mocked(pluginFetch).mockReset()
        vi.mocked(pluginFetch).mockImplementation(
            async () => new Response('{"fixture":true}', { status: 200 }),
        )
    })

    afterEach(() => {
        state.platform = { runtime: 'web', os: 'windows' }
        state.database.usePlainFetch = true
    })

    test('globalFetch sends its JSON body as dataText with a JSON content type', async () => {
        const body = { messages: [{ role: 'user', content: 'synthetic' }] }
        const response = await globalFetch('https://api.example.invalid/v1', {
            body,
            headers: { Authorization: 'Bearer fixture' },
        })
        expect(response).toMatchObject({ ok: true, data: { fixture: true } })
        const [input, init] = vi.mocked(pluginFetch).mock.calls[0] as [URL, any]
        expect(String(input)).toBe('https://api.example.invalid/v1')
        expect(init).toMatchObject({
            method: 'POST',
            dataText: JSON.stringify(body),
            headers: {
                'Content-Type': 'application/json',
                Authorization: 'Bearer fixture',
            },
        })
        expect('body' in init).toBe(false)
    })

    test('nativeFetch hands a string body to the stream as text without adding a content type', async () => {
        await fetchNative('https://api.example.invalid/v1', {
            method: 'POST',
            body: '{"stream":true}',
            headers: { 'X-Request': 'value' },
            logFetch: false,
        })
        const [, init] = vi.mocked(pluginFetch).mock.calls[0] as [string, any]
        expect(init.dataText).toBe('{"stream":true}')
        expect('body' in init).toBe(false)
        expect(init.headers).toEqual({ 'X-Request': 'value' })
    })

    test('nativeFetch logs a string body without encoding it for the Tauri route', async () => {
        const body = '{"stream":true,"prompt":"synthetic"}'
        const encode = vi.spyOn(TextEncoder.prototype, 'encode')
        try {
            await fetchNative('https://api.example.invalid/v1', { method: 'POST', body })
            expect(encode).not.toHaveBeenCalledWith(body)
        } finally {
            encode.mockRestore()
        }
        const [, init] = vi.mocked(pluginFetch).mock.calls[0] as [string, any]
        expect(init.dataText).toBe(body)
        expect(getFetchLogs()[0]).toMatchObject({ url: 'https://api.example.invalid/v1', body })
    })

    test('nativeFetch keeps a byte body as bytes', async () => {
        const body = Uint8Array.of(0, 255, 1)
        await fetchNative('https://api.example.invalid/v1', {
            method: 'PUT',
            body,
            logFetch: false,
        })
        const [, init] = vi.mocked(pluginFetch).mock.calls[0] as [string, any]
        expect(init.body).toBe(body)
        expect(init.dataText).toBeUndefined()
    })
})
