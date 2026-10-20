import { afterEach, beforeEach, vi } from 'vitest'

/** Runs each test without `AbortSignal.any` (Chrome 116), as on Android System WebView 113. */
export function withoutAbortSignalAny(): void {
    // happy-dom's AbortSignal inherits `any`, so shadow it instead of deleting it.
    const own = Object.getOwnPropertyDescriptor(AbortSignal, 'any')
    beforeEach(() => { Object.defineProperty(AbortSignal, 'any', { configurable: true, writable: true, value: undefined }) })
    afterEach(() => {
        if (own) Object.defineProperty(AbortSignal, 'any', own)
        else Reflect.deleteProperty(AbortSignal, 'any')
    })
}

/** Lists the abort listeners added to a signal that has not aborted and not removed since tracking began. */
export function trackAbortListeners(signal: AbortSignal): () => unknown[] {
    const added = vi.spyOn(signal, 'addEventListener'), removed = vi.spyOn(signal, 'removeEventListener')
    const listeners = (calls: unknown[][]) => calls.filter(([type]) => type === 'abort').map(([, listener]) => listener)
    return () => {
        const gone = listeners(removed.mock.calls)
        return listeners(added.mock.calls).filter(listener => !gone.includes(listener))
    }
}
