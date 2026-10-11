import { beforeEach, describe, expect, it, vi } from 'vitest'

describe('library file admission settlement', () => {
    beforeEach(() => vi.resetModules())

    it('opens nested pickers synchronously and resumes only after the owning job releases', async () => {
        const admission = await import('./libraryFileOperation')
        const resumed = vi.fn()
        admission.subscribeLibraryFileOperationReleased(resumed)
        const release = admission.reserveLibraryFileOperation()
        let finish!: () => void
        const picker = vi.fn(() => new Promise<void>(resolve => { finish = resolve }))
        const selection = admission.withLibraryFileSelection(() => admission.withLibraryFileSelection(picker))
        expect(picker).toHaveBeenCalledOnce()
        finish(); await selection
        expect(admission.isLibraryFileOperationReserved()).toBe(true)
        expect(resumed).not.toHaveBeenCalled()
        release()
        expect(resumed).toHaveBeenCalledOnce()
    })

    it('releases failed selections, including synchronous opening errors', async () => {
        const admission = await import('./libraryFileOperation')
        await expect(admission.withLibraryFileSelection(() => { throw new Error('picker failed') })).rejects.toThrow('picker failed')
        expect(admission.isLibraryFileOperationReserved()).toBe(false)
    })

    it('waits for every registered participant and removes only the disposed gate', async () => {
        const admission = await import('./libraryFileOperation')
        let finish!: () => void
        const first = vi.fn(() => new Promise<void>(resolve => { finish = resolve }))
        const second = vi.fn(async () => {})
        const removeFirst = admission.registerLibraryFileOperationGate(first)
        admission.registerLibraryFileOperationGate(second)
        const completed = vi.fn()
        const waiting = admission.waitForLibraryFileOperation(new AbortController().signal)!.then(completed)
        await Promise.resolve(); await Promise.resolve()
        expect(first).toHaveBeenCalledOnce(); expect(second).toHaveBeenCalledOnce()
        expect(completed).not.toHaveBeenCalled()
        finish(); await waiting; removeFirst()
        await admission.waitForLibraryFileOperation(new AbortController().signal)
        expect(first).toHaveBeenCalledOnce(); expect(second).toHaveBeenCalledTimes(2)
    })

    it('notifies once after release and an older release never clears a newer reservation', async () => {
        const admission = await import('./libraryFileOperation')
        const listener = vi.fn(() => expect(admission.isLibraryFileOperationReserved()).toBe(false))
        const unsubscribe = admission.subscribeLibraryFileOperationReleased(listener)
        const first = admission.reserveLibraryFileOperation()
        expect(listener).not.toHaveBeenCalled()
        first()
        const second = admission.reserveLibraryFileOperation()
        first()
        expect(admission.isLibraryFileOperationReserved()).toBe(true)
        expect(listener).toHaveBeenCalledOnce()
        second()
        expect(listener).toHaveBeenCalledTimes(2)
        unsubscribe()
        admission.reserveLibraryFileOperation()()
        expect(listener).toHaveBeenCalledTimes(2)
    })

    it('does not turn a settled operation into failure when a scheduling listener fails', async () => {
        const admission = await import('./libraryFileOperation')
        admission.subscribeLibraryFileOperationReleased(() => { throw new Error('synthetic scheduler failure') })
        const next = vi.fn()
        admission.subscribeLibraryFileOperationReleased(next)
        expect(() => admission.reserveLibraryFileOperation()()).not.toThrow()
        expect(admission.isLibraryFileOperationReserved()).toBe(false)
        expect(next).toHaveBeenCalledOnce()
    })
})
