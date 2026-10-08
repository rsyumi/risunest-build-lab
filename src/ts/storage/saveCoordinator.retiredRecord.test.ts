import { describe, expect, it, vi } from 'vitest'
import { captureRoot, makeDatabase, makeStore, SaveCoordinator } from './saveCoordinator.testSupport'

async function backgroundCommits(failure: Error) {
    vi.useFakeTimers()
    try {
        const database = makeDatabase()
        const commit = vi.fn(async () => { throw failure })
        const onBackgroundError = vi.fn()
        const coordinator = new SaveCoordinator({
            store: makeStore(commit),
            captureRoot: () => captureRoot(database),
            captureSelectedCharacter: () => database.characters[0],
            replaceDatabase: () => undefined,
            onBackgroundError,
        })
        coordinator.initialize(1)
        database.username = 'Edited'
        coordinator.markPersistentDataDirty(1)
        await vi.advanceTimersByTimeAsync(500)
        expect(onBackgroundError).toHaveBeenCalledWith(failure)
        await vi.advanceTimersByTimeAsync(120_000)
        return commit.mock.calls.length
    } finally {
        vi.useRealTimers()
    }
}

describe('background save of an edit the store refuses for a deleted record', () => {
    it('does not retry the refused commit on its own', async () => {
        await expect(backgroundCommits(new Error('retired-record-id'))).resolves.toBe(1)
    })

    it('still retries an ordinary failure', async () => {
        await expect(backgroundCommits(new Error('offline'))).resolves.toBeGreaterThan(1)
    })
})

describe('background save failures from native commands', () => {
    it('reports each distinct plain native error', async () => {
        vi.useFakeTimers()
        try {
            const database = makeDatabase()
            const busy = { code: 'commit-busy' }
            const stored = { code: 'committed', revision: 2, message: 'journal write failed' }
            const commit = vi.fn(async () => { throw commit.mock.calls.length === 1 ? busy : stored })
            const onBackgroundError = vi.fn()
            const coordinator = new SaveCoordinator({
                store: makeStore(commit),
                captureRoot: () => captureRoot(database),
                captureSelectedCharacter: () => database.characters[0],
                replaceDatabase: () => undefined,
                onBackgroundError,
            })
            coordinator.initialize(1)
            database.username = 'Edited'
            coordinator.markPersistentDataDirty(1)
            await vi.advanceTimersByTimeAsync(120_000)
            expect(commit.mock.calls.length).toBeGreaterThan(1)
            expect(onBackgroundError).toHaveBeenNthCalledWith(1, busy)
            expect(onBackgroundError).toHaveBeenNthCalledWith(2, stored)
            expect(onBackgroundError).toHaveBeenCalledTimes(2)
        } finally {
            vi.useRealTimers()
        }
    })
})
