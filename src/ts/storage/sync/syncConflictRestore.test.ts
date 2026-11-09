// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { language } from 'src/lang'

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
    nativeRestore: vi.fn(),
    nativeSource: vi.fn(),
    characters: vi.fn(),
    beginReplacement: vi.fn(),
    confirmReplacement: vi.fn(),
    release: vi.fn(),
    hold: vi.fn(),
}))
vi.mock('@tauri-apps/api/core', async original => ({...await original<typeof import('@tauri-apps/api/core')>(),invoke:mocks.nativeSource}))
vi.mock('../portableBackupFileRouteProduction.svelte', () => ({restoreBackupFromNativeSource:mocks.nativeRestore}))
vi.mock('../../platform', async () => (await import('../../platform.testSupport')).platformModule(() => ({ runtime: mocks.native.enabled ? 'tauri' : 'web', os: 'windows' })))
vi.mock('../libraryCharacters', () => ({ hasLibraryCharacters: mocks.characters }))
vi.mock('./serverSyncProduction', () => ({
    getServerSyncController: () => ({ beginReplacement: mocks.beginReplacement, confirmReplacement: mocks.confirmReplacement }),
    holdServerSyncAfterRestore: mocks.hold,
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
        mocks.nativeSource.mockReset().mockResolvedValue({type:"desktopPath",path:"synthetic-conflict.risudat"})
        mocks.nativeRestore.mockReset().mockImplementation(async source => {await source({signal:new AbortController().signal, onSource:vi.fn()});return {warningCodes:[]}})
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
        mocks.characters.mockReset().mockResolvedValue(true)
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
            [expect.stringContaining(language.syncBackupDatabaseOnly)],
            language.syncConflictBackups,
        )
        expect(mocks.alertConfirm).toHaveBeenCalledWith(language.syncConflictRestoreConfirm)
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

    it('restores over a library without characters without asking on the web', async () => {
        mocks.characters.mockResolvedValue(false)
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertConfirm).not.toHaveBeenCalled()
        expect(mocks.installLocalBackup).toHaveBeenCalledOnce()
    })

    it('never stores upstream cold plugin storage references from a web backup', async () => {
        mocks.decodeRisuSave.mockResolvedValue({
            characters: [],
            pluginCustomStorage: { kept: null, _coldpluginIndex: 'index-id', _coldplugin: { legacy: 'legacy-id' } },
        })
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.replacePersistentDatabase.mock.calls[0][0].pluginCustomStorage).toEqual({ kept: null })
    })

    it('keeps the library when the web confirmation is declined', async () => {
        mocks.alertConfirm.mockResolvedValue(false)
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertConfirm).toHaveBeenCalledWith(language.syncConflictRestoreConfirm)
        expect(mocks.installLocalBackup).not.toHaveBeenCalled()
    })

    it.each(['missing', 'decode', 'shape'])('localizes an unreadable %s backup without beginning replacement', async (failure) => {
        if (failure === 'missing') mocks.backupStore.read.mockResolvedValue(undefined)
        if (failure === 'decode') mocks.decodeRisuSave.mockRejectedValue(new Error('bad payload'))
        if (failure === 'shape') mocks.decodeRisuSave.mockResolvedValue({})
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertError).toHaveBeenCalledWith(language.syncConflictBackupUnreadable)
        expect(mocks.installLocalBackup).not.toHaveBeenCalled()
    })

    it('ends a stalled native saved backup lookup without replacing the library', async () => {
        mocks.native.enabled = true
        mocks.nativeSource.mockReturnValue(new Promise(() => {}))
        const { openSyncConflictBackups } = await import('./syncConflictRestore')
        vi.useFakeTimers()
        try {
            const work = openSyncConflictBackups()
            await vi.waitFor(() => expect(mocks.nativeSource).toHaveBeenCalled())
            await vi.advanceTimersByTimeAsync(30_000)
            await work
            expect(mocks.alertError).toHaveBeenCalledWith(language.risuNest.backup.actionFailed)
            expect(mocks.backupStore.read).not.toHaveBeenCalled()
            expect(mocks.replacePersistentDatabase).not.toHaveBeenCalled()
        } finally { vi.useRealTimers() }
    })

    it('restores a native saved conflict backup by ID without loading the database in JavaScript', async () => {
        mocks.native.enabled = true
        mocks.nativeSource.mockResolvedValue({type:'savedConflict', token:'opaque-native-token'})
        const sources: Array<{name: string; bytes: number}> = []
        mocks.nativeRestore.mockImplementation(async source => {
            expect(await source({signal:new AbortController().signal, onSource:(value: typeof sources[number]) => sources.push(value)}))
                .toEqual({type:'savedConflict', token:'opaque-native-token'})
        })
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(sources).toEqual([{name:expect.stringContaining(language.syncBackupSideRemote), bytes:3}])
        expect(sources[0].name).not.toContain('opaque-native-token')
        expect(mocks.nativeSource).toHaveBeenCalledWith('native_conflict_backup_source', {backupId:'backup'})
        expect(mocks.nativeRestore).toHaveBeenCalledWith(expect.any(Function), expect.objectContaining({confirmationWarnings:[language.syncConflictRestoreScope]}))
        expect(mocks.backupStore.read).not.toHaveBeenCalled()
        expect(mocks.decodeRisuSave).not.toHaveBeenCalled()
        expect(mocks.replacePersistentDatabase).not.toHaveBeenCalled()
        expect(mocks.alertConfirm).not.toHaveBeenCalled()
    })

    it('holds sync as soon as the native commit is known and preserves the hold while reattaching', async () => {
        mocks.native.enabled = true
        mocks.nativeRestore.mockImplementation(async (_source, options) => {
            await options.onNativeStatus({state:'running'})
            expect(mocks.hold).not.toHaveBeenCalled()
            await options.onNativeStatus({state:'succeeded', result:{revision:8}})
            expect(mocks.hold).toHaveBeenCalledOnce()
            expect(options.holdServerSyncAfterRestore).toBe(true)
        })
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.hold).toHaveBeenCalledOnce()
    })

    it('shows a managed failure once and preserves the specific admission error', async () => {
        mocks.native.enabled = true
        const {nativeFileOperationOutcome, NativeFileOperationBusyError} = await import('../nativeFileJobManager')
        mocks.nativeRestore.mockImplementation(async () => {
            nativeFileOperationOutcome.set({kind:'import', startedAt:Date.now(), finishedAt:Date.now(), state:'failed', observedStages:[], warningCodes:[], partialWritesPossible:false})
            throw Error('Already shown')
        })
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertError).not.toHaveBeenCalled()
        nativeFileOperationOutcome.set(null)
        mocks.nativeRestore.mockRejectedValue(new NativeFileOperationBusyError())
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        expect(mocks.alertError).toHaveBeenCalledExactlyOnceWith(language.risuNest.backup.fileBusy)
    })

    it.each(['cancelled','sync-unavailable','restore-failed'])('preserves native %s outcomes', async code => {
        mocks.native.enabled = true
        mocks.nativeRestore.mockRejectedValue(code === 'cancelled' ? new DOMException('Cancelled','AbortError') : Object.assign(new Error('Synthetic failure'),{code}))
        await (await import('./syncConflictRestore')).openSyncConflictBackups()
        if (code === 'cancelled') expect(mocks.alertError).not.toHaveBeenCalled()
        else expect(mocks.alertError).toHaveBeenCalledWith(code === 'sync-unavailable' ? language.risuNest.backup.syncUnavailable : language.risuNest.backup.actionFailed)
        expect(mocks.backupStore.read).not.toHaveBeenCalled()
        expect(mocks.installLocalBackup).not.toHaveBeenCalled()
    })
})
