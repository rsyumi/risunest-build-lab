import { describe, expect, it, vi } from 'vitest'
import { createMacosExitHandler } from './macosLifecycle'

function harness() {
    const dependencies = {
        flush: vi.fn(async () => {}),
        checkpoint: vi.fn(async () => {}),
        confirmExitWithoutSaving: vi.fn(async () => false),
        sync: {
            isSyncActive: () => true,
            hasPendingSync: () => true,
            confirmExit: vi.fn(async () => true),
        },
        respond: vi.fn(async (_token: string, _exit: boolean) => {}),
        reportError: vi.fn(),
        saveLocally: vi.fn(async () => {}),
    }
    const handler = createMacosExitHandler(dependencies)
    return {
        ...dependencies,
        handle: (token: string) => handler({ token, sessionEnd: false }),
    }
}

function coordinated(limitMillis?: number) {
    const coordinator = {
        requestExit: vi.fn(async () => 'exit' as const),
    }
    const dependencies = {
        coordinator,
        respond: vi.fn(async (_token: string, _exit: boolean) => {}),
        reportError: vi.fn(),
        saveLocally: vi.fn(async () => {}),
    }
    return {
        ...dependencies,
        handle: createMacosExitHandler(dependencies, limitMillis),
    }
}

describe('macOS acknowledged quit', () => {
    it('uses the remaining native budget instead of starting five more seconds on delivery', async () => {
        vi.useFakeTimers()
        try {
            const c = coordinated()
            c.saveLocally.mockImplementation(() => new Promise<void>(() => {}))
            const request = c.handle({ token: 'late', sessionEnd: true, deadlineUnixMillis: Date.now() + 500 })
            await vi.advanceTimersByTimeAsync(499)
            expect(c.respond).not.toHaveBeenCalled()
            await vi.advanceTimersByTimeAsync(1)
            await request
            expect(c.respond).toHaveBeenCalledExactlyOnceWith('late', true)
        } finally { vi.useRealTimers() }
    })

    it('upgrades an ordinary pending quit without responding from its superseded handler', async () => {
        const c = coordinated()
        let finish!: (value: 'exit') => void
        c.coordinator.requestExit.mockImplementationOnce(() => new Promise(resolve => { finish = resolve }))
        const ordinary = c.handle({ token: 'same', sessionEnd: false })
        await c.handle({ token: 'same', sessionEnd: true, deadlineUnixMillis: Date.now() + 5_000 })
        finish('exit')
        await ordinary
        expect(c.respond).toHaveBeenCalledExactlyOnceWith('same', true)
        expect(c.saveLocally).toHaveBeenCalledOnce()
    })

    it('delegates the held request to the shared coordinator', async () => {
        const c = coordinated()

        await c.handle({ token: 'coordinated', sessionEnd: false })

        expect(c.coordinator.requestExit).toHaveBeenCalledOnce()
        expect(c.saveLocally).not.toHaveBeenCalled()
        expect(c.respond).toHaveBeenCalledWith('coordinated', true)
    })

    it('saves locally and approves a session-end quit without sync or questions', async () => {
        const c = coordinated()

        await c.handle({ token: 'logout', sessionEnd: true, deadlineUnixMillis: Date.now() + 5_000 })

        expect(c.saveLocally).toHaveBeenCalledOnce()
        expect(c.coordinator.requestExit).not.toHaveBeenCalled()
        expect(c.respond).toHaveBeenCalledExactlyOnceWith('logout', true)
    })

    it('approves a session-end quit when the local save fails or stalls', async () => {
        const failed = coordinated()
        failed.saveLocally.mockRejectedValueOnce(new Error('synthetic failure'))
        await failed.handle({ token: 'failed', sessionEnd: true, deadlineUnixMillis: Date.now() + 5_000 })
        expect(failed.reportError).toHaveBeenCalledOnce()
        expect(failed.respond).toHaveBeenCalledExactlyOnceWith('failed', true)

        const stalled = coordinated(10)
        stalled.saveLocally.mockImplementationOnce(() => new Promise<void>(() => {}))
        await stalled.handle({ token: 'stalled', sessionEnd: true, deadlineUnixMillis: Date.now() + 5_000 })
        expect(stalled.reportError).toHaveBeenCalledOnce()
        expect(stalled.respond).toHaveBeenCalledExactlyOnceWith('stalled', true)
    })

    it('awaits the save and checkpoint, then sync confirmation before responding', async () => {
        const h = harness()
        let release!: () => void
        h.flush.mockImplementationOnce(
            () =>
                new Promise<void>((resolve) => {
                    release = resolve
                }),
        )
        const first = h.handle('first')
        await h.handle('first')
        expect(h.respond).not.toHaveBeenCalled()
        expect(h.checkpoint).not.toHaveBeenCalled()
        release()
        await first
        expect(h.flush).toHaveBeenCalledTimes(1)
        expect(h.checkpoint).toHaveBeenCalledOnce()
        expect(h.sync.confirmExit).toHaveBeenCalledOnce()
        expect(h.respond).toHaveBeenCalledExactlyOnceWith('first', true)
    })

    it.each(['flush', 'checkpoint'] as const)(
        'keeps the app after %s failure and supports retry',
        async (method) => {
            const h = harness()
            h[method].mockRejectedValueOnce(new Error('synthetic failure'))
            await h.handle('first')
            expect(h.respond).toHaveBeenLastCalledWith('first', false)
            expect(h.sync.confirmExit).not.toHaveBeenCalled()
            await h.handle('retry')
            expect(h.respond).toHaveBeenLastCalledWith('retry', true)
        },
    )

    it('keeps the app when sync confirmation is cancelled', async () => {
        const h = harness()
        h.sync.confirmExit.mockResolvedValueOnce(false)
        await h.handle('first')
        expect(h.respond).toHaveBeenLastCalledWith('first', false)
    })

    it('only exits after failed saving when explicitly confirmed', async () => {
        const h = harness()
        h.flush.mockRejectedValueOnce(new Error('synthetic failure'))
        h.confirmExitWithoutSaving.mockResolvedValueOnce(true)
        await h.handle('first')
        expect(h.respond).toHaveBeenLastCalledWith('first', true)
    })

    it('returns a cancellation even when the confirmation UI fails', async () => {
        const h = harness()
        h.sync.confirmExit.mockRejectedValueOnce(
            new Error('synthetic UI error'),
        )
        await h.handle('first')
        expect(h.respond).toHaveBeenLastCalledWith('first', false)
    })
})
