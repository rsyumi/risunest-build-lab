import { AccountReadError } from './accountReadError'

/** A deadline for headers, then for inactivity, never for the whole transfer. */
export function accountReadDeadline(parent?: AbortSignal, idleMs = 120_000) {
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout> | undefined
    const abort = () => controller.abort(parent?.reason)
    const reset = () => {
        clearTimeout(timer)
        timer = setTimeout(() => controller.abort(new AccountReadError('timeout')), idleMs)
    }
    parent?.addEventListener('abort', abort, { once: true })
    if (parent?.aborted) abort()
    reset()
    return { signal: controller.signal, reset, dispose() { clearTimeout(timer); parent?.removeEventListener('abort', abort) } }
}

export async function readAccountBody(response: Response, signal: AbortSignal, activity: () => void,
    onBytes?: (bytes: number) => void): Promise<Uint8Array> {
    activity()
    const reader = response.body?.getReader()
    if (!reader) {
        const declared = response.headers.get('content-length')
        if (!response.headers.get('content-encoding') && declared && /^\d+$/.test(declared) && Number(declared) !== 0) throw new AccountReadError('malformed', response.status)
        return new Uint8Array()
    }
    const cancel = () => { void reader.cancel(signal.reason).catch(() => {}) }
    signal.addEventListener('abort', cancel, { once: true })
    const chunks: Uint8Array[] = []
    let length = 0
    try {
        while (true) {
            if (signal.aborted) throw signal.reason
            const next = await reader.read()
            if (signal.aborted) throw signal.reason
            if (next.done) break
            if (next.value.length) { activity(); length += next.value.length; chunks.push(next.value); onBytes?.(next.value.length) }
        }
        // Content-Length describes encoded bytes when Content-Encoding is set.
        const declared = response.headers.get('content-length')
        if (!response.headers.get('content-encoding') && declared && /^\d+$/.test(declared) && Number(declared) !== length) {
            throw new AccountReadError('malformed', response.status)
        }
        const bytes = new Uint8Array(length)
        let offset = 0
        for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length }
        return bytes
    } finally { signal.removeEventListener('abort', cancel); reader.releaseLock() }
}
