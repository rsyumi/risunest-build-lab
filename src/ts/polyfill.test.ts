import { afterEach, expect, it, vi } from 'vitest'

vi.mock('./platform', () => ({ isIOS: () => false }))

afterEach(() => {
    vi.unstubAllGlobals()
    vi.resetModules()
})

it('preserves the WebView stream constructors when they are available', async () => {
    const original = [globalThis.ReadableStream, globalThis.WritableStream, globalThis.TransformStream]
    await import('./polyfill')
    expect([globalThis.ReadableStream, globalThis.WritableStream, globalThis.TransformStream]).toEqual(original)
})

it('installs working stream fallbacks with transform, close and cancellation', async () => {
    vi.stubGlobal('ReadableStream', undefined)
    vi.stubGlobal('WritableStream', undefined)
    vi.stubGlobal('TransformStream', undefined)
    await import('./polyfill')

    const output: string[] = []
    const closed = vi.fn()
    const source = new ReadableStream<string>({
        start(controller) {
            controller.enqueue('합성🐿️')
            controller.enqueue('tail')
            controller.close()
        },
    })
    await source.pipeThrough(new TransformStream<string, string>({
        transform(chunk, controller) { controller.enqueue(`[${chunk}]`) },
    })).pipeTo(new WritableStream<string>({
        write(chunk) { output.push(chunk) },
        close: closed,
    }))
    expect(output).toEqual(['[합성🐿️]', '[tail]'])
    expect(closed).toHaveBeenCalledOnce()

    const cancelled = vi.fn()
    const reader = new ReadableStream({ cancel: cancelled }).getReader()
    await reader.cancel('synthetic-stop')
    expect(cancelled).toHaveBeenCalledWith('synthetic-stop')
})
