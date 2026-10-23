import { expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { doingChat, generationCancellation, reserveGeneration, setGenerationBackground } from './generationState'

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

it('holds one generation background from the reservation until its release', () => {
    const end = vi.fn(async () => {})
    const start = vi.fn(() => ({ signal: new AbortController().signal, progress() {}, settle() {}, takeExpiry: () => false, end }))
    setGenerationBackground(start)
    try {
        const reservation = reserveGeneration()!
        expect(reserveGeneration()).toBeNull()
        expect(start).toHaveBeenCalledOnce()
        expect(end).not.toHaveBeenCalled()
        reservation.release()
        reservation.release()
        expect(end).toHaveBeenCalledOnce()
    } finally {
        setGenerationBackground(undefined)
    }
})
