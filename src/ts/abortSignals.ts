/**
 * `AbortSignal.any` for Android System WebView 113 and iOS before 17.4, which lack it.
 * Dispose once the work settles so long-lived inputs keep no listener.
 */
export function joinAbortSignals(signals: readonly (AbortSignal | null | undefined)[]): { signal: AbortSignal; dispose(): void } {
    const controller = new AbortController()
    const listeners = new Map<AbortSignal, () => void>()
    const dispose = () => {
        for (const [signal, listener] of listeners) signal.removeEventListener('abort', listener)
        listeners.clear()
    }
    for (const signal of signals) {
        if (!signal || listeners.has(signal)) continue
        if (signal.aborted) {
            dispose()
            controller.abort(signal.reason)
            break
        }
        const abort = () => {
            dispose()
            controller.abort(signal.reason)
        }
        listeners.set(signal, abort)
        signal.addEventListener('abort', abort, { once: true })
    }
    return { signal: controller.signal, dispose }
}
