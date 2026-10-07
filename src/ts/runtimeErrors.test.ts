import { afterEach, describe, expect, it, vi } from 'vitest'
import { registerRuntimeErrorHandlers } from './runtimeErrors'

const cleanup: (() => void)[] = []
afterEach(() => {
    for (const stop of cleanup.splice(0)) stop()
    vi.restoreAllMocks()
})

function harness(recordError = vi.fn(async (_message: string) => {})) {
    const target = new EventTarget() as Window
    const showError = vi.fn()
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const stop = registerRuntimeErrorHandlers(target, showError, recordError)
    cleanup.push(stop)
    return { target, showError, recordError, stop }
}

function reject(target: Window, reason: unknown) {
    const event = new Event('unhandledrejection')
    Object.defineProperty(event, 'reason', { value: reason })
    target.dispatchEvent(event)
}

describe('runtime error handling', () => {
    it('records and presents an error event without an Error object', () => {
        const h = harness()
        expect(() => h.target.dispatchEvent(new ErrorEvent('error', {
            message: 'Synthetic script failure',
        }))).not.toThrow()
        expect(h.recordError).toHaveBeenCalledWith('Uncaught error: Synthetic script failure')
        expect(h.showError).toHaveBeenCalledWith('Synthetic script failure')
    })

    it.each([
        'ResizeObserver loop completed with undelivered notifications.',
        'ResizeObserver loop limit exceeded',
    ])('ignores the browser ResizeObserver loop notice: %s', (message) => {
        const h = harness()
        h.target.dispatchEvent(new ErrorEvent('error', { message }))
        expect(h.showError).not.toHaveBeenCalled()
        expect(h.recordError).not.toHaveBeenCalled()
        h.target.dispatchEvent(new ErrorEvent('error', { message: 'Synthetic script failure' }))
        expect(h.showError).toHaveBeenCalledExactlyOnceWith('Synthetic script failure')
        expect(h.recordError).toHaveBeenCalledExactlyOnceWith('Uncaught error: Synthetic script failure')
    })

    it('still presents an Error object that carries a ResizeObserver loop message', () => {
        const h = harness()
        const error = new Error('ResizeObserver loop completed with undelivered notifications.')
        h.target.dispatchEvent(new ErrorEvent('error', { error, message: error.message }))
        expect(h.showError).toHaveBeenCalledExactlyOnceWith(error)
    })

    it('records uncaught errors and rejected errors with their kind and name', () => {
        const h = harness()
        const error = new TypeError('Synthetic render failure')
        h.target.dispatchEvent(new ErrorEvent('error', { error }))
        reject(h.target, new Error('Synthetic async failure'))
        expect(h.recordError.mock.calls.map(([message]) => message)).toEqual([
            'Uncaught error: TypeError: Synthetic render failure',
            'Unhandled rejection: Error: Synthetic async failure',
        ])
        expect(h.showError).toHaveBeenNthCalledWith(1, error)
    })

    it('bounds text before IPC and never serializes arbitrary rejected objects', () => {
        const h = harness()
        reject(h.target, 'x'.repeat(10_000))
        const value = { data: 'synthetic-private-content', toString() { throw new Error('no') } }
        reject(h.target, value)
        expect(h.recordError.mock.calls[0][0]).toHaveLength(2048)
        expect(h.recordError.mock.calls[1][0]).toBe('Unhandled rejection: Unhandled non-Error value')
        expect(h.showError).toHaveBeenLastCalledWith(value)
    })

    it('keeps alerts working when native logging rejects or throws', async () => {
        const h = harness()
        h.recordError.mockRejectedValueOnce(new Error('Synthetic IPC rejection'))
        h.recordError.mockImplementationOnce(() => { throw new Error('Synthetic IPC throw') })
        reject(h.target, 'first')
        reject(h.target, 'second')
        await Promise.resolve()
        expect(h.showError.mock.calls).toEqual([['first'], ['second']])
    })

    it('supports the web without native logging and removes both listeners', () => {
        const target = new EventTarget() as Window
        const showError = vi.fn()
        vi.spyOn(console, 'error').mockImplementation(() => {})
        const stop = registerRuntimeErrorHandlers(target, showError)
        reject(target, 'before cleanup')
        stop()
        reject(target, 'after cleanup')
        target.dispatchEvent(new ErrorEvent('error', { message: 'after cleanup' }))
        expect(showError).toHaveBeenCalledExactlyOnceWith('before cleanup')
    })
})
