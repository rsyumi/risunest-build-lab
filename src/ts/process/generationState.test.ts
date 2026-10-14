import { expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { doingChat, generationCancellation, reserveGeneration } from './generationState'

it('keeps ownership until cleanup after an idempotent cancellation request', () => {
    const cancel = vi.fn()
    const reservation = reserveGeneration(cancel)!
    try {
        const stop = get(generationCancellation)!
        stop()
        stop()
        expect(cancel).toHaveBeenCalledOnce()
        expect(get(doingChat)).toBe(true)
        doingChat.set(false)
        expect(get(doingChat)).toBe(true)
        expect(reserveGeneration()).toBeNull()
    } finally {
        reservation.release()
    }
    expect(get(doingChat)).toBe(false)
    expect(get(generationCancellation)).toBeNull()
})

it('ignores an obsolete Stop callback and release after another send starts', () => {
    const firstCancel = vi.fn()
    const first = reserveGeneration(firstCancel)!
    const oldStop = get(generationCancellation)!
    first.release()
    const secondCancel = vi.fn()
    const second = reserveGeneration(secondCancel)!
    try {
        oldStop()
        first.release()
        expect(firstCancel).not.toHaveBeenCalled()
        expect(secondCancel).not.toHaveBeenCalled()
        expect(get(doingChat)).toBe(true)
        get(generationCancellation)!()
        expect(secondCancel).toHaveBeenCalledOnce()
    } finally {
        second.release()
    }
})
