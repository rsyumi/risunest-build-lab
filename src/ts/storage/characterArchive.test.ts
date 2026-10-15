import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'

const harness = vi.hoisted(() => ({
    confirm: vi.fn(), error: vi.fn(), clearSelection: vi.fn(),
    preview: vi.fn(), archive: vi.fn(), restore: vi.fn(),
    capture: vi.fn(), acquire: vi.fn(), refresh: vi.fn(), release: vi.fn(),
    database: { characters: [{ chaId: 'char-a' }], account: undefined },
    runtime: undefined as object | undefined,
}))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))
vi.mock('../alert', () => ({ alertConfirm: harness.confirm, alertError: harness.error }))
vi.mock('../stores.svelte', () => ({ DBState: { db: harness.database }, selectedCharID: writable(0) }))
vi.mock('src/lib/workingSetNavigation', () => ({ clearCharacterSelection: harness.clearSelection }))
vi.mock('./persistentDataStoreFactory', () => ({ getPersistentDataStore: () => ({
    archivePreview: harness.preview, archiveCharacter: harness.archive, restoreCharacter: harness.restore,
}) }))
vi.mock('./persistentDataRuntime.svelte', () => ({ getPersistentDataRuntime: () => harness.runtime ?? ({
    capturePersistentMutationToken: harness.capture, acquireDestructiveReplacementFence: harness.acquire,
}) }))
import { language } from 'src/lang'
import { archiveCharacterWithConfirmation, restoreArchivedCharacterWithConfirmation } from './characterArchive'
import type { Database } from './database.svelte'
import { IndexedDbPersistentDataStore } from './indexedDbPersistentDataStore'
import { capturePersistentPluginStorage, capturePersistentPresets, capturePersistentRoot, createPersistentDataRuntime } from './persistentDataRuntime'
import { notifyLocalPersistentRevision, subscribeLocalPersistentRevision } from './persistentRevisionEvents'

/// A runtime over a real store, wired to the revision notifier as production wires it.
async function productionLikeRuntime(name: string) {
    const initial = {
        username: 'User', botPresets: [], pluginCustomStorage: {},
        characters: [{ type: 'character', chaId: 'char-a', name: 'Character', chatPage: 0, chats: [] }],
    } as unknown as Database
    const store = new IndexedDbPersistentDataStore(name, new IDBFactory(), IDBKeyRange)
    await store.open()
    await store.replaceFromDatabase(structuredClone(initial))
    let database = structuredClone(initial)
    const runtime = createPersistentDataRuntime({
        store,
        state: {
            captureRoot: () => capturePersistentRoot(database),
            capturePluginStorage: () => capturePersistentPluginStorage(database),
            capturePresets: () => capturePersistentPresets(database),
            captureSelectedCharacter: () => null,
            captureCharacter: (id) => database.characters.find((character) => character.chaId === id) ?? null,
            getSelectedCharacterId: () => undefined,
            replaceDatabase: (replacement) => { database = structuredClone(replacement) },
            publishCharacter: () => undefined,
            publishConversation: () => undefined,
        },
        prepareDatabase: async (value) => value,
        onLocalRevision: (revision) => notifyLocalPersistentRevision(revision),
    })
    await runtime.initializeActiveWorkingSet(database)
    return { store, runtime }
}

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
        expect(harness.error).toHaveBeenCalledWith(language.risuNest.archive.restoreRefreshFailed)
        expect(harness.release).toHaveBeenCalledOnce()
    })

    it('reports mutation failure and always releases the fence', async () => {
        harness.archive.mockRejectedValue(new Error('synthetic archive error'))
        const log = vi.spyOn(console, 'error').mockImplementation(() => {})
        try {
            expect(await archiveCharacterWithConfirmation('char-a')).toBe(false)
            expect(harness.error).toHaveBeenCalledWith(language.risuNest.archive.archiveFailed)
            expect(harness.refresh).not.toHaveBeenCalled()
            expect(harness.release).toHaveBeenCalledOnce()
        } finally { log.mockRestore() }
    })

    it('names a file the native store could not fetch or find', async () => {
        const log = vi.spyOn(console, 'error').mockImplementation(() => {})
        const strings = language.risuNest.archive
        try {
            for (const [message, restoreText, archiveText] of [
                ['character archive data could not be fetched', strings.restoreRemoteAssetUnavailable, strings.archiveRemoteAssetUnavailable],
                ['character archive data is missing', strings.restoreAssetMissing, strings.archiveAssetMissing],
                ['archived character exceeds the decoded size limit', strings.restoreTooLarge, strings.archiveTooLarge],
                ['synthetic other failure', strings.restoreFailed, strings.archiveFailed],
            ]) {
                const failure = Object.assign(new Error(message), { code: 'validation' })
                harness.error.mockClear()
                harness.restore.mockRejectedValueOnce(failure)
                expect(await restoreArchivedCharacterWithConfirmation('char-a')).toBe(false)
                expect(harness.error).toHaveBeenCalledWith(restoreText)
                harness.error.mockClear()
                harness.archive.mockRejectedValueOnce(failure)
                expect(await archiveCharacterWithConfirmation('char-a')).toBe(false)
                expect(harness.error).toHaveBeenCalledWith(archiveText)
            }
        } finally { log.mockRestore() }
    })
})

describe('character archive sync notification', () => {
    afterEach(() => { harness.runtime = undefined })

    it.each([
        ['archive', () => archiveCharacterWithConfirmation('char-a'), harness.archive],
        ['restore', () => restoreArchivedCharacterWithConfirmation('char-a'), harness.restore],
    ] as const)('reports the committed %s once as a local revision', async (name, run, mutation) => {
        const { store, runtime } = await productionLikeRuntime(`character-${name}-local-revision`)
        harness.runtime = runtime
        // The native store commits the archive change at the fenced revision.
        mutation.mockImplementation((characterId: string, expectedRevision: number) => store.commit({
            expectedRevision,
            unitMutations: [{ type: 'set', key: JSON.stringify(['character', characterId, 'notes']), value: name }],
        }))
        const before = runtime.revision
        const notified = vi.fn()
        const unsubscribe = subscribeLocalPersistentRevision(notified)
        try {
            expect(await run()).toBe(true)
        } finally {
            unsubscribe()
        }
        expect(harness.error).not.toHaveBeenCalled()
        expect(runtime.revision).toBe(before + 1)
        expect(notified).toHaveBeenCalledExactlyOnceWith(before + 1, 'edit')
    })

    it('reports nothing when the archive is not committed', async () => {
        const { runtime } = await productionLikeRuntime('character-archive-failed-local-revision')
        harness.runtime = runtime
        harness.archive.mockRejectedValue(new Error('synthetic archive error'))
        const notified = vi.fn()
        const unsubscribe = subscribeLocalPersistentRevision(notified)
        const log = vi.spyOn(console, 'error').mockImplementation(() => {})
        try {
            expect(await archiveCharacterWithConfirmation('char-a')).toBe(false)
        } finally {
            unsubscribe()
            log.mockRestore()
        }
        expect(notified).not.toHaveBeenCalled()
    })
})
