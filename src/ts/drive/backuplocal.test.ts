import 'fake-indexeddb/auto'
import { UNOWNED_PLUGIN_OWNER } from '../plugins/pluginOwner'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BlobStore } from '../storage/blobStore'
import type { Database } from '../storage/database.svelte'
import { IndexedDbPersistentDataStore } from '../storage/indexedDbPersistentDataStore'
import type { PersistentDataRuntime } from '../storage/persistentDataRuntime'
import { createLeaseStub } from '../storage/persistentDataStore.testSupport'
import { decodeRisuSave, encodeRisuSaveLegacy } from '../storage/risuSave'
import { risuSaveFixtureDatabase } from '../storage/tests/risuSaveFixtures'
import { sleep } from '../util'
import { invoke } from '@tauri-apps/api/core'
import { getBackupInlayName } from './backupAssets'
import { backupArchive, backupEntry, backupFile } from './backupRestore.testSupport'
import { readableFile } from '../storage/tauriFs.testSupport'

const state = vi.hoisted(() => ({
    native: true,
    mobile: false,
    characters: true,
    blobStore: null as BlobStore | null,
    currentDatabase: null as Database | null,
    runtime: null as PersistentDataRuntime | null,
    written: new Map<string, Uint8Array>(),
    nativeFile: new Uint8Array([7, 8, 9]),
    openNativeFile: vi.fn(),
    nativeFileClose: vi.fn(async () => undefined),
    nativeRestore: vi.fn(async () => null),
    fullReadFile: vi.fn(async () => new Uint8Array([99])),
    restoreEvents: [] as string[],
    replacePersistentDatabase: vi.fn(async (_database: Database, _reason: string, _options?: import('../storage/saveCoordinator').PersistentReplacementOptions) => ({
        kind: 'committed', revision: 1, projection: 'applied',
    } as const)),
    getUncleanables: vi.fn(async () => ['assets/second-read.png']),
    fallbackContext: {
        signal: new AbortController().signal,
        onStatus: vi.fn(),
        setSource: vi.fn(),
        setPartialWritesPossible: vi.fn(),
    },
}))

vi.mock('../alert', async () => (await import('./backupRestore.testSupport')).alertModule())
vi.mock('../storage/portableBackupFileRouteProduction.svelte', () => ({restoreBackupFromSystemPicker: state.nativeRestore}))
vi.mock('@tauri-apps/api/core', async importOriginal => ({
    ...await importOriginal<typeof import('@tauri-apps/api/core')>(), invoke: vi.fn(async () => undefined),
}))
vi.mock('../storage/libraryCharacters', async () => (await import('./backupRestore.testSupport')).libraryCharactersModule(async () => state.characters))

vi.mock('../globalApi.svelte', () => ({
    forageStorage: { Init: vi.fn(async () => undefined), isAccount: false },
    getUncleanables: state.getUncleanables,
    LocalWriter: class {
        async init() {
            return true
        }

        async writeBackup(name: string, bytes: Uint8Array) {
            state.written.set(name, bytes.slice())
        }

        async writeBackupStream(
            name: string,
            byteLength: number,
            chunks: AsyncIterable<Uint8Array>,
        ) {
            const value = new Uint8Array(byteLength)
            let offset = 0
            for await (const chunk of chunks) {
                value.set(chunk, offset)
                offset += chunk.byteLength
            }
            if (offset !== byteLength) throw new Error('unexpected test stream length')
            state.written.set(name, value)
        }

        async close() {}
    },
}))

vi.mock('../storage/platformBlobStore', async () => (await import('./backupRestore.testSupport')).blobStoreModule(() => state.blobStore!))
vi.mock('../storage/database.svelte', async () => (await import('./backupRestore.testSupport')).databaseModule(() => state.currentDatabase!))
vi.mock('../storage/persistentDataRuntime.svelte', async () => (await import('./backupRestore.testSupport')).persistentRuntimeModule({
    getPersistentDataRuntime: () => state.runtime!,
    replacePersistentDatabase: (database, reason, options) => state.replacePersistentDatabase(database, reason, options),
}))
vi.mock('../process/coldstorage.svelte', async () => (await import('./backupRestore.testSupport')).coldStorageModule())
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule(() => ({
    runtime: state.native ? 'tauri' : 'web',
    os: state.mobile ? 'android' : 'windows',
})))
vi.mock('@tauri-apps/plugin-fs', async () => (await import('../storage/tauriFs.testSupport')).pluginFsModule({
    open: state.openNativeFile,
    readFile: state.fullReadFile,
}))
vi.mock('../desktopRelaunch', async () => (await import('./backupRestore.testSupport')).relaunchModule())
vi.mock('../storage/nativeFileJobManager', async (original) => ({
    ...await original<object>(),
    runSharedNativeFileOperation: vi.fn(async (_kind, _label, run) => run(state.fallbackContext)),
}))
vi.mock('../util', async () => (await import('./backupRestore.testSupport')).utilModule())
vi.mock('../characterCards', async () => (await import('./backupRestore.testSupport')).characterCardsModule())

function emptyBlobStore(): BlobStore {
    return {
        put: vi.fn(),
        read: vi.fn(async () => null),
        stat: vi.fn(async () => null),
        list: vi.fn(async () => []),
        remove: vi.fn(),
        resolveUrl: vi.fn(async () => null),
    }
}

describe('local backup persistent snapshot', () => {
    afterEach(() => {
        vi.restoreAllMocks()
    })

    beforeEach(() => {
        state.native = true
        state.nativeRestore.mockReset().mockResolvedValue(null)
        state.mobile = false
        delete window.RisuNativeFileSelection
        state.characters = true
        vi.clearAllMocks()
        state.written.clear()
        state.restoreEvents = []
        state.replacePersistentDatabase.mockReset().mockImplementation(async (_database, reason) => {
            expect(reason).toBe('local-backup')
            state.restoreEvents.push('database')
            return { kind: 'committed', revision: 1, projection: 'applied' }
        })
        state.blobStore = emptyBlobStore()
        state.nativeFileClose.mockClear()
        state.fullReadFile.mockClear()
        state.openNativeFile.mockReset()
        state.openNativeFile.mockResolvedValue({ ...readableFile(state.nativeFile, 2), close: state.nativeFileClose })
    })

    it('routes native imports through the descriptor picker and refuses direct WebView decoding', async () => {
        const {LoadLocalBackup, importLegacyBackupWithWebView} = await import('./backuplocal')
        await LoadLocalBackup()
        expect(state.nativeRestore).toHaveBeenCalledOnce()
        await expect(importLegacyBackupWithWebView(state.fallbackContext)).rejects.toMatchObject({code:'native-required'})
        expect(state.replacePersistentDatabase).not.toHaveBeenCalled()
        expect(state.fullReadFile).not.toHaveBeenCalled()
    })

    it.each([false, true])('expands cold chat and skips an upstream plugin index when present (%s)', async (pluginIndex) => {
        state.native = false
        const coldKey = '00000000-0000-4000-8000-00000000c01d'
        const database = structuredClone(risuSaveFixtureDatabase) as Database & { pluginStorage: Record<string, unknown> }
        database.pluginStorage = { inline: 'preserved', ...(pluginIndex ? { _coldpluginIndex: '00000000-0000-4000-8000-00000000c02d' } : {}) }
        database.characters[0].coldstorage = coldKey
        const restoredName = 'Restored cold character'
        const cold = {
            character: {
                ...structuredClone(database.characters[0]),
                name: restoredName,
                coldstorage: undefined,
            },
        }
        const archive = backupArchive(
            ...(pluginIndex ? [backupEntry('coldstorage_00000000-0000-4000-8000-00000000c02d.json', new TextEncoder().encode(JSON.stringify({
                type: 'risu-plugin-storage-index', version: 1, entries: [{ k: 'omitted', s: '"plugin value"' }], trash: [],
            })))] : []),
            backupEntry(`coldstorage_${coldKey}.json`, new TextEncoder().encode(JSON.stringify(cold))),
            backupEntry('database.risudat', encodeRisuSaveLegacy(database, 'compression')),
        )
        const file = backupFile('backup.bin', archive)
        const input = document.createElement('input')
        Object.defineProperty(input, 'files', { value: [file] })
        const click = vi.spyOn(input, 'click').mockImplementation(() => {})
        const createElement = vi.spyOn(document, 'createElement').mockReturnValueOnce(input)
        const { LoadLocalBackup } = await import('./backuplocal')
        const { alertWait } = await import('../alert')
        const pending = LoadLocalBackup()
        await vi.waitFor(() => expect(click).toHaveBeenCalledOnce())
        input.dispatchEvent(new Event('change'))
        await pending
        createElement.mockRestore()

        expect(state.restoreEvents).toEqual(['database'])
        expect(state.replacePersistentDatabase).toHaveBeenCalledOnce()
        const replaced = state.replacePersistentDatabase.mock.calls[0][0] as typeof database
        expect(replaced.characters[0].name).toBe(restoredName)
        expect(replaced.characters[0].coldstorage).toBeUndefined()
        expect(replaced.pluginStorage.inline).toBe('preserved')
        expect(replaced.pluginStorage.omitted).toBeUndefined()
        expect(alertWait).not.toHaveBeenCalled()

        const context = state.fallbackContext
        expect(context.setSource).toHaveBeenCalledExactlyOnceWith({ name: 'backup.bin', bytes: archive.byteLength })
        // Cold payloads become record bodies, so this archive writes nothing before activation.
        expect(context.setPartialWritesPossible).not.toHaveBeenCalled()
        const stages = context.onStatus.mock.calls.map(([status]) => status.detail?.stage)
        expect(stages[0]).toBe('reading-archive')
        expect(stages.filter((stage, index) => stage !== stages[index - 1])).toEqual([
            'reading-archive', 'decoding-database', 'activating', 'restarting-app',
        ])
        const last = context.onStatus.mock.calls.at(-1)?.[0]
        expect(last?.kind).toBe('restore-legacy-local-backup')
        expect(last?.progress).toEqual({
            completedBytes: archive.byteLength,
            totalBytes: archive.byteLength,
            completedItems: pluginIndex ? 3 : 2,
            totalItems: pluginIndex ? 3 : 2,
        })
        expect(last?.detail?.counts).toMatchObject({
            entriesRead: pluginIndex ? 3 : 2,
            entriesTotal: pluginIndex ? 3 : 2,
            coldStorage: pluginIndex ? 2 : 1,
            assets: 0,
            skipped: pluginIndex ? 1 : 0,
            characters: database.characters.length,
            charactersTotal: database.characters.length,
            presets: database.botPresets.length,
        })
        expect(JSON.stringify(context.onStatus.mock.calls)).toContain(`coldstorage_${coldKey}.json`)
    })

    it.each([[true,false,false,false,true],[false,false,false,false,true],[true,true,false,false,true],[true,false,false,true,true],[true,false,false,false,false]])('preflights, acknowledges, and preserves committed outcomes (%s,%s,%s, first run %s, characters %s)', async (confirmed, bodyFails, native, firstRun, characters) => {
        state.native = native
        state.characters = characters
        const { alertCheckboxConfirm } = await import('../alert')
        vi.mocked(alertCheckboxConfirm).mockResolvedValueOnce({ confirmed, checked: confirmed })
        const database = structuredClone(risuSaveFixtureDatabase) as Database
        const damagedInlayName = getBackupInlayName('damaged-inlay')
        const archive = backupArchive(
            backupEntry(damagedInlayName, Uint8Array.of(9, 0, 0, 0, 1, 2)),
            backupEntry('database.risudat', encodeRisuSaveLegacy(database, 'compression')),
        )
        const file = backupFile('damaged-inlay.bin', archive, (start, end) => {
            if (start === 8 + new TextEncoder().encode(damagedInlayName).byteLength && end - start === 6) {
                expect(state.replacePersistentDatabase).toHaveBeenCalledOnce()
                if (bodyFails) throw new Error('synthetic attachment read failure')
            }
        })
        const input = document.createElement('input')
        Object.defineProperty(input, 'files', { value: [file] })
        const click = vi.spyOn(input, 'click').mockImplementation(() => {})
        const createElement = vi.spyOn(document, 'createElement')
            .mockReturnValueOnce(input)
        const { importLegacyBackupWithWebView } = await import('./backuplocal')
        const context = {
            signal: new AbortController().signal,
            onStatus: vi.fn(),
            setSource: vi.fn(),
            setPartialWritesPossible: vi.fn(),
        }

        const {language} = await import('../../lang')
        const pending = importLegacyBackupWithWebView(context, {firstRun}, language.pocketRisuImportConfirm)
        await vi.waitFor(() => expect(click).toHaveBeenCalledOnce())
        input.dispatchEvent(new Event('change'))
        if (!confirmed) {
            await expect(pending).rejects.toMatchObject({name:'AbortError'})
            createElement.mockRestore()
            expect(alertCheckboxConfirm).toHaveBeenCalledOnce()
            expect(state.replacePersistentDatabase).not.toHaveBeenCalled()
            expect(state.blobStore?.put).not.toHaveBeenCalled()
            return
        }
        if (bodyFails) {
            await expect(pending).rejects.toMatchObject({name:'NativeFileJobActivationCommittedError',committedRevision:1})
            createElement.mockRestore()
            expect(state.replacePersistentDatabase).toHaveBeenCalledOnce()
            expect(context.setPartialWritesPossible).toHaveBeenCalledWith(true)
            expect(state.blobStore?.put).not.toHaveBeenCalled()
            return
        }
        const result = await pending
        createElement.mockRestore()
        if (native) {
            expect(alertCheckboxConfirm).not.toHaveBeenCalled()
            expect(state.replacePersistentDatabase.mock.calls[0][2]).toMatchObject({upstreamImport:true, upstreamImportWarnings:[], ...(firstRun ? {firstRun:true} : {})})
        } else if (characters) {
            expect(alertCheckboxConfirm).toHaveBeenCalledOnce()
            expect(alertCheckboxConfirm).toHaveBeenCalledWith(expect.objectContaining({title:language.pocketRisuImportConfirm}))
        } else {
            expect(alertCheckboxConfirm).not.toHaveBeenCalled()
        }

        expect(result).toEqual({ warningCodes: ['upstream-restore-losses'], upstreamLosses: {coldMissing:0,invalidInlays:1,pocketInlays:0} })
        expect(state.blobStore?.put).not.toHaveBeenCalled()
        const last = context.onStatus.mock.calls.at(-1)?.[0]
        expect(last?.detail?.counts).toMatchObject({
            entriesRead: 2,
            inlays: 0,
            assets: 0,
            skipped: 1,
        })
    })

    it.each([true, false])('shows only the cold storage warning on the web when the library holds no character, answer=%s', async answer => {
        state.native = false
        state.characters = false
        const { alertActionConfirm, alertCheckboxConfirm } = await import('../alert')
        vi.mocked(alertActionConfirm).mockResolvedValueOnce(answer)
        const database = structuredClone(risuSaveFixtureDatabase) as Database
        database.characters[0].coldstorage = 'missing-cold'
        const file = backupFile('cold-missing.bin', backupEntry('database.risudat', encodeRisuSaveLegacy(database, 'compression')))
        const input = document.createElement('input')
        Object.defineProperty(input, 'files', { value: [file] })
        const click = vi.spyOn(input, 'click').mockImplementation(() => {})
        const createElement = vi.spyOn(document, 'createElement').mockReturnValueOnce(input)
        const { importLegacyBackupWithWebView } = await import('./backuplocal')
        const { language } = await import('../../lang')
        const pending = importLegacyBackupWithWebView(state.fallbackContext, {}, language.backupLoadConfirm)
        await vi.waitFor(() => expect(click).toHaveBeenCalledOnce())
        input.dispatchEvent(new Event('change'))
        if (answer) await pending
        else await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
        createElement.mockRestore()
        expect(alertCheckboxConfirm).not.toHaveBeenCalled()
        expect(alertActionConfirm).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ title: language.backupLoadConfirm }))
        expect(vi.mocked(alertActionConfirm).mock.calls[0][0].description).not.toContain(language.checkboxConfirmation.dataReplacementDescription)
        expect(state.replacePersistentDatabase).toHaveBeenCalledTimes(answer ? 1 : 0)
    })

    it('stops a WebView import after cancellation without publishing staged attachments', async () => {
        state.native = false
        const controller = new AbortController()
        // Cancel after reading the first entry header, before any attachment body.
        const file = backupFile('partial.bin', backupEntry('partial-asset.png', new Uint8Array([1, 2, 3])))
        const input = document.createElement('input')
        Object.defineProperty(input, 'files', { value: [file] })
        const click = vi.spyOn(input, 'click').mockImplementation(() => {})
        const createElement = vi.spyOn(document, 'createElement')
            .mockReturnValueOnce(input)
        const { importLegacyBackupWithWebView } = await import('./backuplocal')
        const context = {
            signal: controller.signal,
            onStatus: vi.fn((status) => { if (status.detail?.counts?.entriesRead > 0) controller.abort() }),
            setSource: vi.fn(),
            setPartialWritesPossible: vi.fn(),
        }

        const pending = importLegacyBackupWithWebView(context)
        await vi.waitFor(() => expect(click).toHaveBeenCalledOnce())
        input.dispatchEvent(new Event('change'))
        await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
        createElement.mockRestore()

        expect(state.blobStore?.put).not.toHaveBeenCalled()
        expect(context.setPartialWritesPossible).not.toHaveBeenCalled()
        expect(state.replacePersistentDatabase).not.toHaveBeenCalled()
    })

    it('resolves to nothing when the WebView picker is closed or the operation is cancelled first', async () => {
        state.native = false
        const controller = new AbortController()
        controller.abort()
        const { importLegacyBackupWithWebView } = await import('./backuplocal')
        const context = {
            signal: controller.signal,
            onStatus: vi.fn(),
            setSource: vi.fn(),
            setPartialWritesPossible: vi.fn(),
        }

        await expect(importLegacyBackupWithWebView(context)).resolves.toBeNull()
        expect(context.setSource).not.toHaveBeenCalled()
    })

    it('records the native failure code and reason for diagnostics without a second alert', async () => {
        const { NativeFileJobError } = await import('../storage/nativeFileJobs')
        const { alertError } = await import('../alert')
        vi.mocked(alertError).mockClear()
        state.nativeRestore.mockRejectedValueOnce(
            new NativeFileJobError('revision-conflict', 'The database changed during import'),
        )
        const { LoadLocalBackup } = await import('./backuplocal')
        await LoadLocalBackup()
        expect(invoke).toHaveBeenCalledWith('native_log_error', {
            message: 'Backup import failed [revision-conflict]: The database changed during import',
        })
        expect(alertError).not.toHaveBeenCalled()
    })

    it('distinguishes an imported database from a failed screen refresh in diagnostics', async () => {
        const { NativeFileJobActivationCommittedError } = await import('../storage/nativeFileJobs')
        const { alertError } = await import('../alert')
        vi.mocked(alertError).mockClear()
        state.nativeRestore.mockRejectedValueOnce(
            new NativeFileJobActivationCommittedError(2, new Error('Synthetic refresh failed')),
        )
        const { LoadLocalBackup } = await import('./backuplocal')
        await LoadLocalBackup()
        expect(invoke).toHaveBeenCalledWith('native_log_error', {
            message: expect.stringContaining('Backup data was imported, but the app could not refresh'),
        })
        expect(invoke).toHaveBeenCalledWith('native_log_error', {
            message: expect.stringContaining('activation-committed-refresh-failed'),
        })
        expect(invoke).toHaveBeenCalledWith('native_log_error', {
            message: expect.stringContaining('Synthetic refresh failed'),
        })
        expect(alertError).not.toHaveBeenCalled()
    })

    it('stays silent for cancelled imports because the dialog already reported them', async () => {
        const { alertError } = await import('../alert')
        vi.mocked(alertError).mockClear()
        state.nativeRestore.mockRejectedValueOnce(
            new DOMException('cancelled', 'AbortError'),
        )
        const { LoadLocalBackup } = await import('./backuplocal')
        await LoadLocalBackup()
        expect(invoke).not.toHaveBeenCalledWith('native_log_error', expect.anything())
        expect(alertError).not.toHaveBeenCalled()
    })

    it('writes the database from the flushed store revision', async () => {
        const store = new IndexedDbPersistentDataStore(
            `local-backup-${crypto.randomUUID()}`,
            new IDBFactory(),
            IDBKeyRange,
        )
        await store.open()
        const persisted = structuredClone(risuSaveFixtureDatabase) as Database
        persisted.botPresets.forEach((preset, index) => { preset.id = `synthetic-local-backup-preset-${index}` })
        const pluginAssetKey = 'assets/plugin-storage.bin'
        const pluginAssetBytes = Uint8Array.of(11, 22, 33)
        persisted.pluginCustomStorage = {
            nested: { asset: pluginAssetKey },
        }
        const imported = await store.replaceFromDatabase(persisted)
        const materializeDatabase = vi.spyOn(store, 'materializeDatabase').mockRejectedValue(
            new Error('local backup must not materialize the database'),
        )
        const live = structuredClone(persisted)
        live.characters[0].chats[0].message = []
        state.currentDatabase = live
        state.runtime = {
            store,
            revision: imported.revision,
            capturePersistentMutationToken: vi.fn(async () => ({
                revision: imported.revision,
                mutationGeneration: 0,
            })),
        } as unknown as PersistentDataRuntime
        state.blobStore = {
            ...emptyBlobStore(),
            read: vi.fn(async (key: string) => key === pluginAssetKey
                ? pluginAssetBytes.slice()
                : null),
        }

        const { SaveLocalBackup } = await import('./backuplocal')
        await SaveLocalBackup()

        const databaseBytes = state.written.get('database.risudat')
        expect(databaseBytes).toBeDefined()
        const backedUp = await decodeRisuSave(databaseBytes!)
        expect(backedUp.characters[0].chats[0].message).toEqual(
            persisted.characters[0].chats[0].message,
        )
        expect(backedUp.pluginCustomStorage).toEqual(persisted.pluginCustomStorage)
        expect(state.written.get('plugin-storage.bin')).toEqual(pluginAssetBytes)
        expect(materializeDatabase).not.toHaveBeenCalled()
        expect(state.runtime.capturePersistentMutationToken).toHaveBeenCalledWith('local-backup')
    })

    it('writes a partial backup database from the flushed store revision', async () => {
        const store = new IndexedDbPersistentDataStore(
            `partial-local-backup-${crypto.randomUUID()}`,
            new IDBFactory(),
            IDBKeyRange,
        )
        await store.open()
        const persisted = structuredClone(risuSaveFixtureDatabase) as Database
        persisted.botPresets.forEach((preset, index) => { preset.id = `synthetic-local-backup-preset-${index}` })
        persisted.pluginCustomStorage = {
            zero: 0,
            nested: {
                asset: 'assets/plugin-partial.bin',
                inlay: '{{inlay::plugin-partial-inlay}}',
            },
        }
        const imported = await store.replaceFromDatabase(persisted)
        const materializeDatabase = vi.spyOn(store, 'materializeDatabase').mockRejectedValue(
            new Error('partial backup must not materialize the database'),
        )
        const acquireRevision = store.acquireRevision.bind(store)
        let readPluginStorage: ReturnType<typeof vi.fn> | undefined
        vi.spyOn(store, 'acquireRevision').mockImplementation(async (revision) => {
            const lease = await acquireRevision(revision)
            const readPinnedPluginStorage = lease.readPluginStorage.bind(lease)
            readPluginStorage = vi.fn(readPinnedPluginStorage)
            lease.readPluginStorage = readPluginStorage
            return lease
        })
        const live = structuredClone(persisted)
        live.characters[0].chats[0].message = []
        state.currentDatabase = live
        state.runtime = {
            store,
            revision: imported.revision,
            capturePersistentMutationToken: vi.fn(async () => ({
                revision: imported.revision,
                mutationGeneration: 0,
            })),
        } as unknown as PersistentDataRuntime
        state.blobStore = {
            ...emptyBlobStore(),
            read: vi.fn(async (key: string) => [
                'assets/plugin-partial.bin',
                'plugin-partial-inlay',
            ].includes(key) ? Uint8Array.of(7, 7) : null),
            list: vi.fn(async () => [{
                key: 'plugin-partial-inlay',
                kind: 'inlay' as const,
                size: 2,
                mime: 'application/octet-stream',
                name: 'partial.bin',
                ext: 'bin',
                inlayType: 'signature' as const,
            }]),
        }

        const { SavePartialLocalBackup } = await import('./backuplocal')
        await SavePartialLocalBackup()

        const databaseBytes = state.written.get('database.risudat')
        expect(databaseBytes).toBeDefined()
        const backedUp = await decodeRisuSave(databaseBytes!)
        expect(backedUp.characters[0].chats[0].message).toEqual(
            persisted.characters[0].chats[0].message,
        )
        expect(backedUp.pluginCustomStorage).toEqual(persisted.pluginCustomStorage)
        expect(backedUp.pluginCustomStorage.zero).toBe(0)
        expect(backedUp.pluginCustomStorage.nested).toEqual({
            asset: 'assets/plugin-partial.bin',
            inlay: '{{inlay::plugin-partial-inlay}}',
        })
        expect(state.written.has('plugin-partial.bin')).toBe(false)
        expect(state.written.has(getBackupInlayName('plugin-partial-inlay'))).toBe(false)
        expect(readPluginStorage).toHaveBeenCalledTimes(2)
        expect(readPluginStorage).toHaveBeenNthCalledWith(1, UNOWNED_PLUGIN_OWNER, 'zero')
        expect(readPluginStorage).toHaveBeenNthCalledWith(2, UNOWNED_PLUGIN_OWNER, 'nested')
        expect(materializeDatabase).not.toHaveBeenCalled()
        expect(state.runtime.capturePersistentMutationToken).toHaveBeenCalledWith(
            'partial-local-backup',
        )
        expect([...state.written.keys()].some((name) => name.startsWith('coldstorage_'))).toBe(false)
    })

    it('reads a partial backup asset from the normalized key after the raw key misses', async () => {
        const store = new IndexedDbPersistentDataStore(
            `partial-local-backslash-${crypto.randomUUID()}`,
            new IDBFactory(),
            IDBKeyRange,
        )
        await store.open()
        const persisted = structuredClone(risuSaveFixtureDatabase) as Database
        persisted.botPresets.forEach((preset, index) => { preset.id = `synthetic-local-backup-preset-${index}` })
        persisted.characters[0].image = 'assets\\partial-profile.png'
        const imported = await store.replaceFromDatabase(persisted)
        state.currentDatabase = structuredClone(persisted)
        state.runtime = {
            store,
            revision: imported.revision,
            capturePersistentMutationToken: vi.fn(async () => ({
                revision: imported.revision,
                mutationGeneration: 0,
            })),
        } as unknown as PersistentDataRuntime
        const profileBytes = Uint8Array.of(4, 2)
        const read = vi.fn(async (key: string) => key === 'assets/partial-profile.png'
            ? profileBytes.slice()
            : null)
        state.blobStore = {
            ...emptyBlobStore(),
            read,
        }

        const { SavePartialLocalBackup } = await import('./backuplocal')
        await SavePartialLocalBackup()

        expect(read).toHaveBeenCalledWith('assets\\partial-profile.png')
        expect(read).toHaveBeenCalledWith('assets/partial-profile.png')
        expect(state.written.get('assets\\partial-profile.png')).toEqual(profileBytes)
        const { alertMd, alertNormal } = await import('../alert')
        expect(alertMd).not.toHaveBeenCalled()
        expect(alertNormal).toHaveBeenCalledWith('Success')
    })

    it('streams a native pinned export into the local backup entry', async () => {
        const collectBytes = vi.fn(async () => new Uint8Array([1]))
        const events: string[] = []
        const withNativeFile = vi.fn(async (_options, callback) => {
            try {
                return await callback({
                    path: 'C:\\app\\persistent\\exports\\risusave-test.risudat',
                    bytes: 3,
                })
            } finally {
                events.push('cleanup')
            }
        })
        const writeBackupStream = vi.fn(async (
            name: string,
            byteLength: number,
            chunks: AsyncIterable<Uint8Array>,
        ) => {
            const values: number[] = []
            for await (const chunk of chunks) values.push(...chunk)
            expect(name).toBe('database.risudat')
            expect(byteLength).toBe(3)
            expect(values).toEqual([7, 8, 9])
        })
        const { writePinnedLocalBackupDatabase } = await import('./backuplocal')

        await expect(writePinnedLocalBackupDatabase({ writeBackupStream } as any, {
            revision: 3,
            mutationGeneration: 0,
            reader: createLeaseStub({ revision: 3 }),
            countCharacters: vi.fn(),
            materializeDatabase: vi.fn(),
            stream: vi.fn(),
            collectBytes,
            withNativeFile,
        })).resolves.toBeUndefined()

        expect(withNativeFile).toHaveBeenCalledOnce()
        expect(withNativeFile.mock.calls[0][0]).toEqual({ omitAccount: true })
        expect(writeBackupStream).toHaveBeenCalledOnce()
        expect(state.openNativeFile).toHaveBeenCalledWith(
            'C:\\app\\persistent\\exports\\risusave-test.risudat',
            { read: true },
        )
        expect(state.nativeFileClose).toHaveBeenCalledOnce()
        expect(events).toEqual(['cleanup'])
        expect(state.fullReadFile).not.toHaveBeenCalled()
        expect(collectBytes).not.toHaveBeenCalled()
    })

    it('closes and cleans the native export when its source read fails', async () => {
        const sourceError = new Error('source failed')
        state.openNativeFile.mockResolvedValueOnce({
            read: vi.fn().mockRejectedValue(sourceError),
            close: state.nativeFileClose,
        })
        const cleanup = vi.fn()
        const withNativeFile = vi.fn(async (_options, callback) => {
            try {
                return await callback({ path: 'native.risudat', bytes: 3 })
            } finally {
                cleanup()
            }
        })
        const writer = {
            writeBackupStream: async (_name: string, _length: number, chunks: AsyncIterable<Uint8Array>) => {
                for await (const _chunk of chunks) {
                    // consume the source
                }
            },
        }
        const { writePinnedLocalBackupDatabase } = await import('./backuplocal')

        await expect(writePinnedLocalBackupDatabase(writer as any, {
            revision: 3,
            mutationGeneration: 0,
            reader: createLeaseStub({ revision: 3 }),
            countCharacters: vi.fn(),
            materializeDatabase: vi.fn(),
            stream: vi.fn(),
            collectBytes: vi.fn(),
            withNativeFile,
        })).rejects.toBe(sourceError)

        expect(state.nativeFileClose).toHaveBeenCalledOnce()
        expect(cleanup).toHaveBeenCalledOnce()
    })

    it('closes and cleans the native export when the destination fails', async () => {
        const destinationError = new Error('destination failed')
        const cleanup = vi.fn()
        const withNativeFile = vi.fn(async (_options, callback) => {
            try {
                return await callback({ path: 'native.risudat', bytes: 3 })
            } finally {
                cleanup()
            }
        })
        const writer = {
            writeBackupStream: async (_name: string, _length: number, chunks: AsyncIterable<Uint8Array>) => {
                for await (const _chunk of chunks) throw destinationError
            },
        }
        const { writePinnedLocalBackupDatabase } = await import('./backuplocal')

        await expect(writePinnedLocalBackupDatabase(writer as any, {
            revision: 3,
            mutationGeneration: 0,
            reader: createLeaseStub({ revision: 3 }),
            countCharacters: vi.fn(),
            materializeDatabase: vi.fn(),
            stream: vi.fn(),
            collectBytes: vi.fn(),
            withNativeFile,
        })).rejects.toBe(destinationError)

        expect(state.nativeFileClose).toHaveBeenCalledOnce()
        expect(cleanup).toHaveBeenCalledOnce()
    })

    it('closes the native source when a chunk consumer returns early', async () => {
        const { streamNativeBackupFile } = await import('./backuplocal')
        const chunks = streamNativeBackupFile('native.risudat', 3)

        await expect(chunks.next()).resolves.toEqual({
            done: false,
            value: new Uint8Array([7, 8]),
        })
        await chunks.return(undefined)

        expect(state.nativeFileClose).toHaveBeenCalledOnce()
    })
})
