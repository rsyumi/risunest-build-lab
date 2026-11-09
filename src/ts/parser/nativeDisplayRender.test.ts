import { afterEach, expect, it, vi } from 'vitest'

vi.unmock('katex')
vi.mock('@tauri-apps/api/core', async () => (await import('../globalApi.testSupport')).tauriCoreModule())
// The registry reads no inlay; this keeps the database and stores out of the graph.
vi.mock(import('../process/files/inlays'), () => ({}))

import { invoke } from '@tauri-apps/api/core'
import { DeferredInlayMarkerRegistry, type InlayRenderSource } from '../process/files/inlayRenderSource'
import { renderDisplayNatively, type NativeDisplayHost, type NativeDisplayRequest } from './nativeDisplayRender'

afterEach(() => vi.restoreAllMocks())

const request: NativeDisplayRequest = {
    text: 'a\uD800',
    mode: 'normal',
    character: { assetWidth: null, chatId: 3, chaId: 'c', characterImage: null, userImage: null, sourcesThrow: false },
    hideAllImages: false,
    imageReservation: false,
    thoughtMode: 'off',
    labels: { cot: 'Thoughts', toolCalled: 'Tool {{tool}}', error: 'Error' },
    quotes: { customQuotes: false, customQuotesData: null, unformatQuotes: false, blockquoteStyling: false },
}

const image: InlayRenderSource = { url: 'blob:i', mime: 'image/png', type: 'image', name: 'i.png', size: 4, width: 640, objectUrl: false }

/** Answers start and resume with `responses` in turn and keeps every call. */
function serve(responses: unknown[]) {
    const calls: [string, Record<string, unknown>][] = []
    vi.mocked(invoke).mockImplementation(async (command, args) => {
        calls.push([command, structuredClone(args as Record<string, unknown>)])
        return command === 'render_display_cancel' ? null : responses.shift()
    })
    return calls
}

function host(overrides: Partial<NativeDisplayHost> = {}): NativeDisplayHost {
    return {
        lookup: (tag, name) => tag === 'emotion' && name === 'Happy' ? { srcPaths: ['p/happy.png'], ext: 'png' } : null,
        url: async (path, image) => `url:${path}:${image}`,
        scripts: async (text) => `${text}\uDC00`,
        inlays: async (ids) => new Map(ids.map((id) => [id, id === 'i' ? image : null])),
        registry: new DeferredInlayMarkerRegistry(),
        ...overrides,
    }
}

const step = (value: unknown) => ({ session: 7, step: value })

it('answers each request in the wire form and registers the inlays the HTML holds', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    const calls = serve([
        step({ kind: 'assetLookups', pass: 0, lookups: [{ tag: 'emotion', name: 'Happy' }, { tag: 'img', name: { u: [0x61, 0xd800] } }] }),
        step({ kind: 'assetUrls', pass: 0, requests: [{ path: 'p/happy.png', image: true }, { path: 'p/b.mp3', image: false }] }),
        step({ kind: 'scripts', text: 'first' }),
        step({ kind: 'inlays', ids: ['i', 'gone'] }),
        step({ kind: 'math', tex: ['x^2', '\\frac{'] }),
        step({ kind: 'highlights', blocks: [{ language: 'javascript', code: 'let x = 1' }, { language: 'none', code: 'x' }] }),
        { session: null, step: { kind: 'done', html: { u: [0x3c, 0x70, 0x3e, 0xd800] }, registrations: [{ slot: 0, id: 'i' }], trim: true, styleHistory: false } },
    ])
    const registry = new DeferredInlayMarkerRegistry()
    const result = await renderDisplayNatively(request, host({ registry }))

    expect(result).toEqual({ kind: 'done', html: '<p>\uD800', trim: true, styleHistory: false })
    expect(calls[0][0]).toBe('render_display_start')
    expect(calls[0][1].request).toMatchObject({ text: { u: [0x61, 0xd800] }, firstSlot: 0, character: { chaId: 'c', chatId: 3 } })
    const replies = calls.slice(1).map(([command, args]) => (expect(command).toBe('render_display_resume'), expect(args.session).toBe(7), args.reply))
    expect(replies.slice(0, 4)).toEqual([
        { kind: 'assetLookups', found: [{ srcPaths: ['p/happy.png'], ext: 'png' }, null] },
        { kind: 'assetUrls', urls: ['url:p/happy.png:true', 'url:p/b.mp3:false'] },
        { kind: 'scripts', text: { u: [...'first'].map((unit) => unit.charCodeAt(0)).concat(0xdc00) } },
        { kind: 'inlays', inlays: [{ id: 'i', type: 'image', mime: 'image/png', width: 640, height: null }] },
    ])
    expect(replies[4]).toMatchObject({ kind: 'math', values: [expect.stringContaining('<math'), null] })
    expect(error).toHaveBeenCalledWith('KaTeX render error:', expect.anything())
    expect(replies[5]).toMatchObject({ kind: 'highlights', values: [expect.stringContaining('hljs-keyword'), null] })
    expect(calls).toHaveLength(7)
    expect(registry.seal('0')).toMatchObject({ id: 'i', type: 'image' })
})

it('hands back the script result after a decline and drops the session', async () => {
    const calls = serve([step({ kind: 'scripts', text: 'first' }), { declined: 'markdown-x' }])
    const result = await renderDisplayNatively(request, host())
    expect(result).toEqual({ kind: 'declined', reason: 'markdown-x', scripted: { firstParsed: 'first', data: 'first\uDC00' } })
    expect(calls.at(-1)).toEqual(['render_display_cancel', { session: 7 }])
})

it('declines without a script result when the render cannot start', async () => {
    serve([{ failed: 'budget' }])
    expect(await renderDisplayNatively(request, host())).toEqual({ kind: 'declined', reason: 'budget', scripted: null })
    vi.mocked(invoke).mockRejectedValueOnce(new Error('ipc'))
    expect(await renderDisplayNatively(request, host())).toEqual({ kind: 'declined', reason: 'invoke', scripted: null })
})

it('stops at an abort between requests and drops the session', async () => {
    const controller = new AbortController()
    const calls = serve([step({ kind: 'assetUrls', pass: 0, requests: [{ path: 'p', image: false }] })])
    const url = async () => (controller.abort(), 'u')
    await expect(renderDisplayNatively(request, host({ url, signal: controller.signal }))).rejects.toThrow()
    expect(calls.map(([command]) => command)).toEqual(['render_display_start', 'render_display_cancel'])
})

it('declines when the slots are not the ones the registry hands out, and registers none of them', async () => {
    const registry = new DeferredInlayMarkerRegistry()
    serve([
        step({ kind: 'inlays', ids: ['i'] }),
        { session: null, step: { kind: 'done', html: 'x', registrations: [{ slot: 0, id: 'i' }], trim: true, styleHistory: false } },
    ])
    const inlays = async () => (registry.register('other', image), new Map([['i', image]]))
    const result = await renderDisplayNatively(request, host({ registry, inlays }))
    expect(result).toEqual({ kind: 'declined', reason: 'display-slots', scripted: null })
    expect(registry.seal('1')).toBeUndefined()

    // A registration without an inlay or out of order declines before the registry takes the ones before it.
    for (const registrations of [[{ slot: 0, id: 'i' }, { slot: 1, id: 'gone' }], [{ slot: 0, id: 'i' }, { slot: 2, id: 'i' }]]) {
        const fresh = new DeferredInlayMarkerRegistry()
        serve([
            step({ kind: 'inlays', ids: ['i', 'gone'] }),
            { session: null, step: { kind: 'done', html: 'x', registrations, trim: true, styleHistory: false } },
        ])
        expect(await renderDisplayNatively(request, host({ registry: fresh }))).toEqual({ kind: 'declined', reason: 'display-slots', scripted: null })
        expect(fresh.nextSlot).toBe(0)
    }
})
