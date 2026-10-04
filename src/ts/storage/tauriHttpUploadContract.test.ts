import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
}))

import { fetch as tauriFetch } from '@tauri-apps/plugin-http'

function sentHeaders(): [string, string][] {
    return mocks.invoke.mock.calls[0][1].clientConfig.headers
        .map(([name, value]: [string, string]) => [name.toLowerCase(), value])
}

describe('installed Tauri HTTP upload contract', () => {
    beforeEach(() => {
        mocks.invoke.mockReset()
        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'plugin:http|fetch') return 1
            if (command === 'plugin:http|fetch_send') {
                return {
                    status: 204,
                    statusText: 'No Content',
                    url: 'https://account.invalid/write',
                    headers: [],
                    rid: 2,
                }
            }
            return undefined
        })
        ;(window as any).__TAURI_INTERNALS__ = { invoke: mocks.invoke }
    })

    afterEach(() => {
        delete (window as any).__TAURI_INTERNALS__
    })

    it('drains a request stream into one byte array before starting native fetch', async () => {
        const body = new ReadableStream<Uint8Array>({
            start(controller) {
                controller.enqueue(Uint8Array.of(1, 2))
                controller.enqueue(Uint8Array.of(3, 4))
                controller.close()
            },
        })

        await tauriFetch('https://account.invalid/write', {
            method: 'POST',
            body,
            duplex: 'half',
        } as RequestInit)

        expect(mocks.invoke.mock.calls[0][0]).toBe('plugin:http|fetch')
        expect(mocks.invoke.mock.calls[0][1]).toEqual(expect.objectContaining({
            clientConfig: expect.objectContaining({ data: [1, 2, 3, 4] }),
        }))
    })

    it('encodes a native export path as text instead of reading its file bytes', async () => {
        const path = 'C:\\app\\persistent\\exports\\snapshot.risudat'

        await tauriFetch('https://account.invalid/write', {
            method: 'POST',
            body: path,
        })

        expect(mocks.invoke.mock.calls[0][1].clientConfig.data).toEqual(
            [...new TextEncoder().encode(path)],
        )
        expect(mocks.invoke.mock.calls[0][1].clientConfig.dataText).toBeNull()
        expect(sentHeaders()).toContainEqual(['content-type', 'text/plain;charset=UTF-8'])
    })

    it('sends dataText as one string without a byte array or an implied content type', async () => {
        const text = JSON.stringify({ prompt: 'synthetic', bytes: 'x'.repeat(1024) })

        await tauriFetch('https://account.invalid/write', {
            method: 'POST',
            headers: { 'X-Request': 'value' },
            dataText: text,
        })

        const clientConfig = mocks.invoke.mock.calls[0][1].clientConfig
        expect(clientConfig.data).toBeNull()
        expect(clientConfig.dataText).toBe(text)
        expect(sentHeaders()).toEqual([['x-request', 'value']])
    })

    it('keeps the content type the caller set for dataText', async () => {
        await tauriFetch('https://account.invalid/write', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            dataText: '{}',
        })

        expect(sentHeaders()).toEqual([['content-type', 'application/json']])
    })

    it('sends an empty dataText as no body', async () => {
        await tauriFetch('https://account.invalid/write', { method: 'POST', dataText: '' })

        const clientConfig = mocks.invoke.mock.calls[0][1].clientConfig
        expect(clientConfig.data).toBeNull()
        expect(clientConfig.dataText).toBeNull()
    })

    it.each([
        ['native toWellFormed', false],
        ['fallback scan', true],
    ])('replaces lone surrogates in dataText the way Request encodes them (%s)', async (_name, withoutNative) => {
        const lone = String.fromCharCode(0xd800)
        const trailing = String.fromCharCode(0xdc00)
        const pair = String.fromCodePoint(0x1f600)
        const text = `a${lone}b${pair}${trailing}${lone}`
        const prototype = String.prototype as { toWellFormed?: () => string }
        const native = prototype.toWellFormed
        if (withoutNative) delete prototype.toWellFormed
        try {
            await tauriFetch('https://account.invalid/write', { method: 'POST', dataText: text })
        }
        finally {
            if (withoutNative) prototype.toWellFormed = native
        }

        const replacement = String.fromCharCode(0xfffd)
        const sent: string = mocks.invoke.mock.calls[0][1].clientConfig.dataText
        expect(sent).toBe(`a${replacement}b${pair}${replacement}${replacement}`)
        expect(new TextEncoder().encode(sent)).toEqual(
            new Uint8Array(await new Request('https://account.invalid/', { method: 'POST', body: text }).arrayBuffer()),
        )
    })

    it.each(['GET', 'HEAD'])('rejects dataText on %s before invoking native fetch', async (method) => {
        await expect(tauriFetch('https://account.invalid/read', { method, dataText: '{}' }))
            .rejects.toThrow(TypeError)
        expect(mocks.invoke).not.toHaveBeenCalled()
    })

    it('rejects dataText combined with body before invoking native fetch', async () => {
        await expect(tauriFetch('https://account.invalid/write', {
            method: 'POST',
            body: 'other',
            dataText: '{}',
        })).rejects.toThrow(TypeError)
        expect(mocks.invoke).not.toHaveBeenCalled()
    })
})
