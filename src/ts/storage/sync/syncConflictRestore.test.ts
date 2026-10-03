import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    backupStore: {
        list: vi.fn(),
        read: vi.fn(),
        save: vi.fn(),
    },
    alertSelect: vi.fn(),
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertNormal: vi.fn(),
    decodeRisuSave: vi.fn(),
    installLocalBackup: vi.fn(),
    replacePersistentDatabase: vi.fn(),
    publishCurrentOfficialRevision: vi.fn(),
    flushPendingData: vi.fn(),
    capturePersistentMutationToken: vi.fn(),
    runtime: { store: {}, revision: 7, flushPendingData: vi.fn() },
    native: { enabled: false },
    beginReplacement: vi.fn(),
    confirmReplacement: vi.fn(),
    release: vi.fn(),
    hold: vi.fn(),
}))
vi.mock('../../platform', () => ({ get isTauri() { return mocks.native.enabled } }))
vi.mock('./serverSyncProduction', () => ({
    getServerSyncController: () => ({ beginReplacement: mocks.beginReplacement, confirmReplacement: mocks.confirmReplacement }),
    holdServerSyncAfterRestore: mocks.hold,
}))

vi.mock('src/lang', () => ({
    language: {
        syncBackupSideLocal: 'local',
        syncBackupSideRemote: 'remote',
        syncBackupEntry: '{date} {side} {count}',
        syncBackupDatabaseOnly: 'database only, no assets/cold/inlays',
        syncConflictBackups: 'backups',
        syncConflictNoBackups: 'no backups',
        syncConflictRestoreConfirm: 'restore database only?',
        syncConflictRestoreScope: 'assets, cold storage and inlays are not included',
        syncConflictBackupUnreadable: 'unreadable backup',
        risuNest: { backup: { actionFailed: 'restore failed', syncUnavailable: 'connect sync first' } },
    },
}))
vi.mock('../../alert', () => ({
    alertSelect: mocks.alertSelect,
    alertConfirm: mocks.alertConfirm,
    alertError: mocks.alertError,
    alertNormal: mocks.alertNormal,
}))
vi.mock('../database.svelte', () => ({
    getDatabase: () => ({
        characters: [{ type: 'character', chaId: 'catalog-only', chats: [] }],
    }),
}))
vi.mock('../databaseRestore', () => ({ installLocalBackup: mocks.installLocalBackup }))
vi.mock('../persistentDataRuntime.svelte', () => ({
    flushPendingData: mocks.flushPendingData,
    capturePersistentMutationToken: mocks.capturePersistentMutationToken,
    replacePersistentDatabase: mocks.replacePersistentDatabase,
    publishCurrentOfficialRevision: mocks.publishCurrentOfficialRevision,
}))
vi.mock('../risuSave', () => ({
    decodeRisuSave: mocks.decodeRisuSave,
    encodeRisuSaveLegacy: vi.fn(() => new Uint8Array([99])),
}))
vi.mock('./syncConflictBackup', () => ({
    getSyncConflictBackupStore: () => mocks.backupStore,
}))

describe('openSyncConflictBackups', () => {
    beforeEach(() => {
        vi.resetModules()
        mocks.native.enabled = false
        mocks.beginReplacement.mockReset().mockResolvedValue(mocks.release)
        mocks.confirmReplacement.mockReset().mockResolvedValue(undefined)
        mocks.release.mockReset()
        mocks.hold.mockReset()
        mocks.replacePersistentDatabase.mockReset().mockResolvedValue({ status: 'applied', revision: 8 })
        const entry = {
            id: 'backup',
            createdAt: 1,
            side: 'remote',
            characterCount: 1,
            byteLength: 3,
            scope: 'database-only',
        }
        mocks.backupStore.list.mockReset().mockResolvedValue([entry])
        mocks.backupStore.read.mockReset().mockResolvedValue(new Uint8Array([1, 2, 3]))
        mocks.backupStore.save.mockReset().mockResolvedValue(undefined)
        mocks.alertSelect.mockReset().mockResolvedValue('0')
        mocks.alertConfirm.mockReset().mockResolvedValue(true)
        mocks.alertError.mockReset()
        mocks.alertNormal.mockReset()
        mocks.decodeRisuSave.mockReset().mockResolvedValue({
            characters: [{ type: 'character', chaId: 'remote', chats: [] }],
        })
        mocks.installLocalBackup.mockReset().mockImplementation(async (database, dependencies) => {
            await dependencies.replaceDatabase(database, 'local-backup')
        })
        mocks.flushPendingData.mockReset().mockResolvedValue(undefined)
        mocks.capturePersistentMutationToken
            .mockReset()
            .mockResolvedValue({ revision: 7, mutationGeneration: 9 })
    })

    it('pins the authoritative local revision before restoring the selected backup', async () => {
        const { openSyncConflictBackups } = await import('./syncConflictRestore')

        await openSyncConflictBackups()

        expect(mocks.flushPendingData).toHaveBeenCalledWith(
            'sync-conflict-restore',
        )
        expect(mocks.capturePersistentMutationToken).toHaveBeenCalledWith(
            'sync-conflict-restore',
        )
        expect(mocks.backupStore.save).not.toHaveBeenCalled()
        expect(mocks.alertSelect).toHaveBeenCalledWith(
            [expect.stringContaining('database only, no assets/cold/inlays')],
            'backups',
        )
        expect(mocks.alertConfirm).toHaveBeenCalledWith('restore database only?')
        expect(mocks.replacePersistentDatabase).toHaveBeenCalledWith(
            expect.objectContaining({ characters: expect.any(Array) }),
            'local-backup',
            {
                authoritative: true,
                expectedRevision: 7,
                expectedMutationGeneration: 9,
            },
        )
        expect(mocks.installLocalBackup).toHaveBeenCalledOnce()
        expect(mocks.beginReplacement).not.toHaveBeenCalled()
        expect(mocks.hold).not.toHaveBeenCalled()
    })

    it.each(['missing', 'decode', 'shape'])('localizes an unreadable %s backup without beginning replacement', async (failure) => {
        if (failure === 'missing') mocks.backupStore.read.mockResolvedValue(undefined)
        if (failure === 'decode') mocks.decodeRisuSave.mockRejectedValue(new Error('bad payload'))
        if (failure === 'shape') mocks.decodeRisuSave.mockResolvedValue({})
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertError).toHaveBeenCalledWith('unreadable backup')
        expect(mocks.installLocalBackup).not.toHaveBeenCalled()
    })

    it.each(['applied', 'refresh-required'])('holds native sync immediately after a committed %s replacement', async (status) => {
        mocks.native.enabled = true
        mocks.replacePersistentDatabase.mockResolvedValue({ status, revision: 8 })
        const followup = vi.fn()
        mocks.installLocalBackup.mockImplementation(async (database, dependencies) => {
            await dependencies.replaceDatabase(database, 'local-backup')
            followup()
        })
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        const order = [mocks.beginReplacement, mocks.confirmReplacement, mocks.flushPendingData, mocks.replacePersistentDatabase, mocks.hold, followup, mocks.release]
            .map((mock) => mock.mock.invocationCallOrder[0])
        expect(order).toEqual([...order].sort((a, b) => a - b))
        expect(mocks.hold).toHaveBeenCalledOnce()
    })

    it('restores a bound device whose pull advances the revision after one checkbox confirmation', async () => {
        mocks.native.enabled = true
        let revision = 7
        const replacementConfirm = vi.fn(async () => true)
        mocks.installLocalBackup.mockImplementation(async (database, dependencies) => {
            await dependencies.replaceDatabase(database, 'local-backup', {
                publishOfficial: true,
                upstreamImport: true,
                ...(dependencies.upstreamImportWarnings ? { upstreamImportWarnings: dependencies.upstreamImportWarnings } : {}),
            })
        })
        // The bound replacement path pulls available changes, asks once, then pauses
        // writes and compares any pinned revision against the post-pull revision.
        mocks.replacePersistentDatabase.mockImplementation(async (_database, _reason, options) => {
            revision = 8
            if (!await replacementConfirm()) throw new Error('Import cancelled')
            if (options.expectedRevision !== undefined && options.expectedRevision !== revision) {
                throw new Error('persistent mutation fenced')
            }
            return { status: 'applied', revision: 9 }
        })

        await (await import('./syncConflictRestore')).openSyncConflictBackups()

        expect(mocks.alertError).not.toHaveBeenCalled()
        expect(mocks.alertConfirm).not.toHaveBeenCalled()
        expect(replacementConfirm).toHaveBeenCalledOnce()
        expect(mocks.installLocalBackup).toHaveBeenCalledWith(
            expect.anything(),
            expect.objectContaining({
                upstreamImportWarnings: ['assets, cold storage and inlays are not included'],
            }),
        )
        expect(mocks.hold).toHaveBeenCalledOnce()
        expect(mocks.release).toHaveBeenCalledOnce()
    })

    it('leaves the library unchanged without an error when the one restore dialog is cancelled', async () => {
        mocks.native.enabled = true
        mocks.replacePersistentDatabase.mockRejectedValue(new DOMException('Import cancelled', 'AbortError'))
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertConfirm).not.toHaveBeenCalled()
        expect(mocks.alertError).not.toHaveBeenCalled()
        expect(mocks.hold).not.toHaveBeenCalled()
        expect(mocks.release).toHaveBeenCalledOnce()
    })

    it('asks the user to connect when the bound sync target is unavailable', async () => {
        mocks.native.enabled = true
        mocks.replacePersistentDatabase.mockRejectedValue(
            Object.assign(new Error('Sync is unavailable for library replacement'), { code: 'sync-unavailable' }),
        )
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertError).toHaveBeenCalledWith('connect sync first')
        expect(mocks.hold).not.toHaveBeenCalled()
        expect(mocks.release).toHaveBeenCalledOnce()
    })

    it.each(['confirm', 'replace'])('releases admission without installing a hold after %s refuses', async (failure) => {
        mocks.native.enabled = true
        if (failure === 'confirm') mocks.confirmReplacement.mockRejectedValue({ code: 'resolve-pending-operation-first' })
        else mocks.replacePersistentDatabase.mockRejectedValue(new Error('failed'))
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.hold).not.toHaveBeenCalled()
        expect(mocks.release).toHaveBeenCalledOnce()
        expect(mocks.alertError).toHaveBeenCalledWith('restore failed')
    })
})
