import type { PluginSnapshotBaseline, PluginSnapshotChunk } from './pluginReadBaselines'

export type PluginSnapshotFrame = PluginSnapshotChunk | { type: 'batch'; chunks: PluginSnapshotChunk[] }

export function createPluginSnapshotStream(
    source: AsyncGenerator<PluginSnapshotChunk>,
    baseline: PluginSnapshotBaseline,
    release: () => Promise<unknown>,
    options: { signal?: AbortSignal; maxChunks?: number; maxBytes?: number } = {},
): ReadableStream<PluginSnapshotFrame> {
    const maxChunks = options.maxChunks ?? 128
    const maxBytes = options.maxBytes ?? 256 * 1024
    let cancelled = false
    let cleanup: Promise<unknown> | undefined
    const check = () => {
        if (cancelled || options.signal?.aborted) throw new DOMException('Plugin snapshot cancelled', 'AbortError')
    }
    const cleanupOnce = () => cleanup ??= Promise.resolve().then(release)
    const frames = (async function* (): AsyncGenerator<PluginSnapshotFrame> {
        let failed = false
        try {
            let chunks: PluginSnapshotChunk[] = []
            let bytes = 0
            for await (const chunk of source) {
                check()
                const cost = baseline.add(chunk)
                if (chunks.length && (chunks.length >= maxChunks || bytes + cost > maxBytes)) {
                    yield { type: 'batch', chunks }
                    check()
                    chunks = []
                    bytes = 0
                }
                if (cost > maxBytes) yield chunk
                else {
                    chunks.push(chunk)
                    bytes += cost
                }
            }
            if (chunks.length) yield { type: 'batch', chunks }
            await cleanupOnce()
            check()
            yield { type: 'provenance', entries: [] }
        } catch (error) {
            failed = true
            throw error
        } finally {
            try { await cleanupOnce() }
            catch (error) { if (!failed) throw error }
        }
    })()
    let closing: Promise<void> | undefined
    const close = () => closing ??= (async () => {
        cancelled = true
        options.signal?.removeEventListener('abort', abort)
        try { await frames.return(undefined) }
        finally { await cleanupOnce() }
    })()
    const abort = () => { void close().catch(() => undefined) }
    options.signal?.addEventListener('abort', abort, { once: true })
    return new ReadableStream<PluginSnapshotFrame>({
        async pull(controller) {
            try {
                check()
                const next = await frames.next()
                check()
                if (next.done) {
                    options.signal?.removeEventListener('abort', abort)
                    controller.close()
                } else {
                    if (next.value.type === 'provenance') next.value.entries = baseline.finish()
                    controller.enqueue(next.value)
                }
            } catch (error) {
                await close().catch(() => undefined)
                controller.error(error)
            }
        },
        cancel: close,
    }, { highWaterMark: 0 })
}
