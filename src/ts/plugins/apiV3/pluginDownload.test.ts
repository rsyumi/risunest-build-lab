import { describe, expect, it } from 'vitest'
import { createPluginDownloadHandler } from './pluginDownload'

function fixture() {
    const saved: { name: string; bytes: number[] }[] = []
    const errors: unknown[] = []
    return {
        saved, errors,
        dependencies: {
            fetch: async (_url: string, _options: { signal: AbortSignal }): Promise<Response> => { throw new Error('Unexpected network request') },
            save: async (name: string, bytes: Uint8Array) => { saved.push({ name, bytes: [...bytes] }); return true },
            reportError: (error: unknown) => { errors.push(error) },
        },
    }
}

describe('plugin native download delivery', () => {
    it('preserves binary bytes and falls back to the File name', async () => {
        const { saved, dependencies } = fixture()
        const save = createPluginDownloadHandler(dependencies)
        const file = new File([new Uint8Array([0, 255, 13, 10])], '한글.charx')
        expect(await save('', file, new AbortController().signal)).toBe(true)
        expect(saved).toEqual([{ name: '한글.charx', bytes: [0, 255, 13, 10] }])
        expect(await file.arrayBuffer()).toEqual(new Uint8Array([0, 255, 13, 10]).buffer)
    })

    it('uses the existing plugin fetch boundary for HTTP and reports a rejected response', async () => {
        const { saved, errors, dependencies } = fixture()
        dependencies.fetch = async url => new Response(url.endsWith('/missing') ? 'missing' : new Uint8Array([1, 2, 3]), {
            status: url.endsWith('/missing') ? 404 : 200,
        })
        const save = createPluginDownloadHandler(dependencies)
        await expect(save('', 'https://synthetic.invalid/%ED%95%9C%EA%B8%80.zip', new AbortController().signal)).resolves.toBe(true)
        await expect(save('bad.zip', 'https://synthetic.invalid/missing', new AbortController().signal)).rejects.toThrow('HTTP 404')
        expect(saved).toEqual([{ name: '한글.zip', bytes: [1, 2, 3] }])
        expect(errors.map(String)).toEqual(['Error: Download failed: HTTP 404'])
    })

    it.each(['file:///secret', 'blob:null/foreign', 'javascript:alert(1)', 'tauri://localhost/secret'])('rejects unsupported source %s', async source => {
        const { saved, dependencies } = fixture()
        await expect(createPluginDownloadHandler(dependencies)('file', source, new AbortController().signal)).rejects.toThrow('Unsupported download URL')
        expect(saved).toEqual([])
    })

    it('serializes pickers across plugins, drops an unloaded queued request and continues after failure', async () => {
        const first = fixture()
        const next = fixture()
        let release!: () => void
        let started!: () => void
        const ready = new Promise<void>(resolve => { started = resolve })
        first.dependencies.save = async () => { started(); await new Promise<void>(resolve => { release = resolve }); throw new Error('Storage full') }
        const saveFirst = createPluginDownloadHandler(first.dependencies)
        const saveNext = createPluginDownloadHandler(next.dependencies)
        const pending = saveFirst('first', new Blob(['first']), new AbortController().signal)
        const rejected = expect(pending).rejects.toThrow('Storage full')
        await ready
        const lifetime = new AbortController()
        const cancelled = saveNext('unloaded', new Blob(['cancelled']), lifetime.signal)
        const final = saveNext('last', new Blob(['ok']), new AbortController().signal)
        lifetime.abort()
        expect(next.saved).toEqual([])
        release()
        await rejected
        expect(await cancelled).toBe(false)
        expect(await final).toBe(true)
        expect(next.saved).toEqual([{ name: 'last', bytes: [111, 107] }])
        expect(first.errors.map(String)).toEqual(['Error: Storage full'])
        expect(next.errors).toEqual([])
    })

    it('does not display an error for picker cancellation', async () => {
        const { errors, dependencies } = fixture()
        dependencies.save = async () => false
        expect(await createPluginDownloadHandler(dependencies)('file', new Blob(), new AbortController().signal)).toBe(false)
        dependencies.save = async () => { throw new DOMException('Cancelled', 'AbortError') }
        expect(await createPluginDownloadHandler(dependencies)('file', new Blob(), new AbortController().signal)).toBe(false)
        expect(errors).toEqual([])
    })
})
