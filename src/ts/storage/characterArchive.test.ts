import { beforeEach, describe, expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'

const harness = vi.hoisted(() => ({
    confirm: vi.fn(), error: vi.fn(), clearSelection: vi.fn(),
    preview: vi.fn(), archive: vi.fn(), restore: vi.fn(),
    capture: vi.fn(), acquire: vi.fn(), refresh: vi.fn(), release: vi.fn(),
    database: { characters: [{ chaId: 'char-a' }], account: undefined },
}))
vi.mock('../platform', () => ({ isTauri: true }))
vi.mock('../alert', () => ({ alertConfirm: harness.confirm, alertError: harness.error }))
vi.mock('src/lang', () => ({ language: { risuNest: { archive: {
    confirmTitle: 'Archive', confirmBody: 'Confirm archive', confirmCounts: '{0}/{1}',
    restoreTitle: 'Restore', restoreBody: 'Confirm restore',
    archiveFailed: 'archive-failed', restoreFailed: 'restore-failed',
    archiveRefreshFailed: 'archive-refresh-failed', restoreRefreshFailed: 'restore-refresh-failed',
} } } }))
vi.mock('../stores.svelte', () => ({ DBState: { db: harness.database }, selectedCharID: writable(0) }))
vi.mock('src/lib/workingSetNavigation', () => ({ clearCharacterSelection: harness.clearSelection }))
vi.mock('./persistentDataStoreFactory', () => ({ getPersistentDataStore: () => ({
    archivePreview: harness.preview, archiveCharacter: harness.archive, restoreCharacter: harness.restore,
}) }))
vi.mock('./persistentDataRuntime.svelte', () => ({ getPersistentDataRuntime: () => ({
    capturePersistentMutationToken: harness.capture, acquireDestructiveReplacementFence: harness.acquire,
}) }))
import { archiveCharacterWithConfirmation, restoreArchivedCharacterWithConfirmation } from './characterArchive'

beforeEach(() => {
    vi.clearAllMocks()
    harness.confirm.mockResolvedValue(true)
    harness.clearSelection.mockResolvedValue(true)
    harness.preview.mockResolvedValue({ archived: false, conversationCount: 2, messageCount: 3 })
    harness.capture.mockResolvedValue({ revision: 7 })
    harness.acquire.mockResolvedValue({ refreshCommittedWorkingSet: harness.refresh, release: harness.release })
    harness.archive.mockResolvedValue({ revision: 8 })
    harness.restore.mockResolvedValue({ revision: 8 })
    harness.refresh.mockResolvedValue({ kind: 'committed', revision: 8, projection: 'applied' })
})

describe('character archive ownership', () => {
    it('clears selected ownership before capture and retains the fence through committed refresh', async () => {
        let finish!: (value: { revision: number }) => void
        harness.archive.mockImplementation(() => new Promise((resolve) => { finish = resolve }))
        const operation = archiveCharacterWithConfirmation('char-a')
        await vi.waitFor(() => expect(harness.archive).toHaveBeenCalledWith('char-a', 7, undefined))
        expect(harness.clearSelection.mock.invocationCallOrder[0]).toBeLessThan(harness.capture.mock.invocationCallOrder[0])
        expect(harness.acquire.mock.invocationCallOrder[0]).toBeLessThan(harness.archive.mock.invocationCallOrder[0])
        expect(harness.release).not.toHaveBeenCalled()
        finish({ revision: 8 })
        expect(await operation).toBe(true)
        expect(harness.refresh).toHaveBeenCalledWith(8)
        expect(harness.release.mock.invocationCallOrder[0]).toBeGreaterThan(harness.refresh.mock.invocationCallOrder[0])
    })

    it('does not archive when selected ownership cannot be released', async () => {
        harness.clearSelection.mockResolvedValue(false)
        expect(await archiveCharacterWithConfirmation('char-a')).toBe(false)
        expect(harness.archive).not.toHaveBeenCalled()
        expect(harness.capture).not.toHaveBeenCalled()
    })

    it('reports a committed restore with pending refresh as committed', async () => {
        harness.refresh.mockResolvedValue({ kind: 'committed', revision: 8, projection: 'refresh-required' })
        expect(await restoreArchivedCharacterWithConfirmation('char-a')).toBe(true)
        expect(harness.error).toHaveBeenCalledWith('restore-refresh-failed')
        expect(harness.release).toHaveBeenCalledOnce()
    })

    it('reports mutation failure and always releases the fence', async () => {
        harness.archive.mockRejectedValue(new Error('synthetic archive error'))
        const log = vi.spyOn(console, 'error').mockImplementation(() => {})
        try {
            expect(await archiveCharacterWithConfirmation('char-a')).toBe(false)
            expect(harness.error).toHaveBeenCalledWith('archive-failed')
            expect(harness.refresh).not.toHaveBeenCalled()
            expect(harness.release).toHaveBeenCalledOnce()
        } finally { log.mockRestore() }
    })
})
