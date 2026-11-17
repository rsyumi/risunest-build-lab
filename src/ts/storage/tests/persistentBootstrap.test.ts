import { UNOWNED_PLUGIN_OWNER } from '../../plugins/pluginOwner'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPersistentStoreHarness } from '../../../../tests/support/persistentStoreHarness'
import type { character, Database } from '../database.svelte'
import { IndexedDbPersistentDataStore } from '../indexedDbPersistentDataStore'
import { RevisionConflictError } from '../persistentDataStore'
import { bootstrapPersistentDatabase } from '../persistentBootstrap'
import {
    createCatalogPresetWorkingSet,
    getCatalogConversationCount,
    isCatalogCharacterStub,
    projectCatalogWorkingSet,
} from '../workingSetCatalog'
import { canonicalJson, SaveCoordinator } from '../saveCoordinator'
import { capturePersistentRoot } from '../persistentDataRuntime'
import { fixtureDatabase } from './persistentDataFixtures'

const scheduling = vi.hoisted(() => ({
    yieldToMainThread: vi.fn(async () => undefined),
}))

vi.mock('../../ui/yieldToUi', () => ({
    yieldToMainThread: scheduling.yieldToMainThread,
}))

function preparedResult(input: Database, output: Database) {
    return {
        database: structuredClone(output),
        changed: canonicalJson(input) !== canonicalJson(output),
    }
}

function plugin(version: '2.1', enabled: boolean): Database['plugins'][number] {
    return { enabled, version } as Database['plugins'][number]
}

describe('bootstrapPersistentDatabase', () => {
    beforeEach(() => {
        scheduling.yieldToMainThread.mockReset()
        scheduling.yieldToMainThread.mockResolvedValue(undefined)
    })

    it('starts a blank store from one prepared empty database', async () => {
        const harness = await createPersistentStoreHarness()
        const materialize = harness.spy('materializeDatabase')
        const prepared = structuredClone(fixtureDatabase)
        const prepareDatabase = vi.fn(async (input) =>
            preparedResult(input, prepared),
        )

        const result = await bootstrapPersistentDatabase({ store: harness.store, prepareDatabase })

        expect(prepareDatabase).toHaveBeenCalledTimes(1)
        expect(prepareDatabase).toHaveBeenCalledWith({
            streamingDisplayOptimizationMode: 'balanced',
        })
        expect(materialize).not.toHaveBeenCalled()
        expect(result).toEqual({ database: prepared, revision: 1 })
        expect(await harness.store.materializeDatabase(1)).toEqual(prepared)
    })

    it('reads a nonblank persistent revision without rewriting it', async () => {
        const persistent = structuredClone(fixtureDatabase)
        persistent.username = 'Persistent user'
        persistent.plugins = [plugin('2.1', true)]
        persistent.streamingDisplayOptimizationMode = 'off'
        const harness = await createPersistentStoreHarness(persistent)
        const readRoot = harness.spy('readRoot')
        const readRevision = vi.fn(async () => harness.revision)

        const result = await bootstrapPersistentDatabase({
            store: Object.assign(harness.store, { readRevision }),
            prepareDatabase: async (database) =>
                preparedResult(database, database),
        })

        expect(result).toEqual({
            database: persistent,
            revision: harness.revision,
        })
        // The revision check after preparation does not read the whole root again.
        expect(readRoot).toHaveBeenCalledOnce()
        expect(readRevision).toHaveBeenCalledOnce()
        expect((await harness.store.readRoot()).revision).toBe(harness.revision)
    })

    it('awaits visible storage and compatibility phases before their expensive work', async () => {
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', true)]
        persistent.language = 'ko'
        const harness = await createPersistentStoreHarness(persistent)
        const events: string[] = []
        harness.observe('open', () => { events.push('open') })
        harness.observe('materializeDatabase', () => { events.push('materialize') })
        await bootstrapPersistentDatabase({
            store: harness.store,
            onPhase: async (phase, locale) => {
                await Promise.resolve()
                if (phase !== 'storage') expect(locale).toBe('ko')
                events.push(phase)
            },
            prepareDatabase: async (database) =>
                preparedResult(database, database),
        })
        expect(events).toEqual([
            'storage',
            'open',
            'data',
            'compatibility',
            'materialize',
        ])
    })

    it('projects a defaulted shared root on existing storage without writing any units', async () => {
        const store = new IndexedDbPersistentDataStore('bootstrap-no-default-write-'+crypto.randomUUID(),new IDBFactory(),IDBKeyRange)
        await store.open()
        const database = structuredClone(fixtureDatabase)
        delete (database as Partial<Database>).askRemoval
        const initialized = await store.replaceFromDatabase(database)
        const commit = vi.spyOn(store,'commit')
        const replace = vi.spyOn(store,'replaceFromDatabase')
        const result = await bootstrapPersistentDatabase({store,
            prepareDatabase:async(value)=>preparedResult(value,value),
            prepareRoot:async(root)=>({...root,askRemoval:true}),
            projectScalableWorkingSet:(input)=>projectCatalogWorkingSet(input.root,input.characters,createCatalogPresetWorkingSet(input.presetCatalog,input.activePreset)),
        })
        expect(result.database.askRemoval).toBe(true)
        expect(result.revision).toBe(initialized.revision)
        expect(commit).not.toHaveBeenCalled()
        expect(replace).not.toHaveBeenCalled()
        expect((await store.readRoot()).value).not.toHaveProperty('askRemoval')
        const coordinator = new SaveCoordinator({store,captureRoot:()=>capturePersistentRoot(result.database),capturePresets:()=>result.database.botPresets,captureSelectedCharacter:()=>null,captureCharacter:()=>null,replaceDatabase:()=>undefined})
        coordinator.initialize(result.revision,result.database)
        await coordinator.flushPendingDataLocally('projected-startup')
        expect(commit).not.toHaveBeenCalled()
        result.database.askRemoval = false
        await coordinator.flushPendingDataLocally('explicit-user-edit')
        expect(commit).toHaveBeenCalledOnce()
        expect((await store.readRoot()).value.askRemoval).toBe(false)
    })

    it('projects preparation changes without replacing existing persistent data', async () => {
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', true)]
        const changed = structuredClone(persistent)
        changed.username = 'Normalized user'
        const harness = await createPersistentStoreHarness(persistent)

        const result = await bootstrapPersistentDatabase({
            store: harness.store,
            prepareDatabase: async (database) =>
                preparedResult(database, changed),
        })

        expect(result).toEqual({
            database: changed,
            revision: harness.revision,
        })
        const stored = await harness.store.readRoot()
        expect(stored.revision).toBe(harness.revision)
        expect(stored.value.username).toBe(persistent.username)
    })

    it('boots a scalable nonblank revision from root, preset, and catalog queries only', async () => {
        const indexedDB = new IDBFactory()
        const databaseName = `bootstrap-scalable-${crypto.randomUUID()}`
        const store = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', false)]
        persistent.botPresetsId = 1
        persistent.pluginCustomStorage = {
            'plugin-memory': { entries: [1, 2, 3] },
        }
        await store.open()
        await store.replaceFromDatabase(persistent)
        const materializeDatabase = vi.spyOn(store, 'materializeDatabase')
        const readPreset = vi.spyOn(store, 'readPreset')
        const prepareDatabase = vi.fn(async () => {
            throw new Error('Full database preparation must not run in scalable boot')
        })
        const projectScalableWorkingSet = vi.fn((input) =>
            projectCatalogWorkingSet(
                input.root,
                input.characters,
                createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset),
            ),
        )

        const result = await bootstrapPersistentDatabase({
            store,
            now: () => 500,
            prepareDatabase,
            prepareRoot: async (root) => structuredClone(root),
            projectScalableWorkingSet,
        })

        expect(materializeDatabase).not.toHaveBeenCalled()
        expect(prepareDatabase).not.toHaveBeenCalled()
        expect(readPreset).toHaveBeenCalledOnce()
        expect(readPreset).toHaveBeenCalledWith('preset-alpha')
        expect(projectScalableWorkingSet).toHaveBeenCalledWith(expect.objectContaining({
            activePreset: {
                summary: expect.objectContaining({ id: 'preset-alpha', configuredIndex: 1 }),
                value: persistent.botPresets[1],
            },
            presetCatalog: expect.objectContaining({
                items: expect.arrayContaining([
                    expect.objectContaining({ id: 'preset-beta', configuredIndex: 0 }),
                    expect.objectContaining({ id: 'preset-alpha', configuredIndex: 1 }),
                ]),
            }),
        }))
        expect(result.revision).toBe(1)
        expect(result.database.botPresets[0]).toEqual({
            id: persistent.botPresets[0]['id'],
            name: persistent.botPresets[0].name,
            image: persistent.botPresets[0].image,
        })
        expect(result.database.botPresets[1]).toEqual(persistent.botPresets[1])
        expect(result.database.characters.map((character) => character.chaId)).toEqual([
            'char-b',
            'char-a',
            'char-c',
        ])
        expect(result.database.characters.every(isCatalogCharacterStub)).toBe(true)
        expect(result.database.characters.map((character) => character.chats)).toEqual([
            [],
            [],
            [],
        ])
        expect(result.database.characters.map(getCatalogConversationCount)).toEqual([1, 2, 1])
        expect(result.database.pluginCustomStorage).toEqual({})
        expect((await store.readPluginStorage(UNOWNED_PLUGIN_OWNER, 'plugin-memory'))?.value).toEqual(
            persistent.pluginCustomStorage['plugin-memory'],
        )
    })

    it('does not run upstream character conversion against an existing active library', async () => {
        const indexedDB = new IDBFactory()
        const store = new IndexedDbPersistentDataStore(
            `bootstrap-scalable-migration-${crypto.randomUUID()}`,
            indexedDB,
            IDBKeyRange,
        )
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', false)]
        delete (persistent as Partial<Database>).formatversion
        persistent.characters[0].image = 'C:\\synthetic\\assets\\avatar.png'
        persistent.characters[0].emotionImages = [
            ['happy', 'C:\\synthetic\\assets\\happy.png'],
        ]
        await store.open()
        await store.replaceFromDatabase(persistent)
        const materializeDatabase = vi.spyOn(store, 'materializeDatabase')

        const result = await bootstrapPersistentDatabase({
            store,
            now: () => 500,
            prepareDatabase: async (database) => {
                const migrated = structuredClone(database)
                migrated.formatversion = 5
                migrated.characters[0].image = 'assets/avatar.png'
                migrated.characters[0].emotionImages = [
                    ['happy', 'assets/happy.png'],
                ]
                return preparedResult(database, migrated)
            },
            prepareRoot: async (root) => structuredClone(root),
            projectScalableWorkingSet: (input) => projectCatalogWorkingSet(
                input.root,
                input.characters,
                createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset),
            ),
        })

        expect(materializeDatabase).not.toHaveBeenCalled()
        expect(result.revision).toBe(1)
        expect(result.database.characters.every(isCatalogCharacterStub)).toBe(true)
        const stored = await store.materializeDatabase(result.revision)
        expect(stored).not.toHaveProperty('formatversion')
        expect(stored.characters[0].image).toBe(persistent.characters[0].image)
        expect(stored.characters[0].emotionImages).toEqual(persistent.characters[0].emotionImages)
    })

    it('yields between character catalog pages without delaying every summary', async () => {
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', false)]
        persistent.botPresetsId = -1
        persistent.characters = Array.from({ length: 3 }, (_, index) => ({
            type: 'character', chaId: `char-${index}`, name: `Character ${index}`, chats: [],
        }) as unknown as character)
        const harness = await createPersistentStoreHarness(persistent)
        const events: string[] = []
        scheduling.yieldToMainThread.mockImplementation(async () => {
            events.push('yield')
        })
        harness.observe('queryCharacters', (query) => {
            events.push(`${query.trash ? 'trash' : 'active'}:${query.cursor === undefined ? 'first' : 'next'}`)
        })

        const result = await bootstrapPersistentDatabase({
            store: harness.store,
            catalogPageSize: 2,
            prepareDatabase: async (database) =>
                preparedResult(database, database),
            prepareRoot: async (root) => structuredClone(root),
            projectScalableWorkingSet: (input) => projectCatalogWorkingSet(
                input.root,
                input.characters,
                createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset),
            ),
        })

        expect(events).toEqual([
            'active:first',
            'yield',
            'active:next',
            'yield',
            'trash:first',
        ])
        expect(result.database.characters).toHaveLength(3)
    })

    it('rejects a stale catalog page returned after a cooperative yield', async () => {
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', false)]
        persistent.botPresetsId = -1
        const harness = await createPersistentStoreHarness(persistent)
        scheduling.yieldToMainThread.mockImplementationOnce(async () => {
            const root = (await harness.store.readRoot()).value
            await harness.store.commit({ expectedRevision: harness.revision, root: { ...root, username: 'Concurrent user' } })
        })

        await expect(
            bootstrapPersistentDatabase({
                store: harness.store,
                prepareDatabase: async (database) =>
                    preparedResult(database, database),
                prepareRoot: async (root) => structuredClone(root),
                projectScalableWorkingSet: () =>
                    structuredClone(fixtureDatabase),
            }),
        ).rejects.toBeInstanceOf(RevisionConflictError)

        expect(scheduling.yieldToMainThread).toHaveBeenCalledOnce()
    })

    it('canonicalizes an out-of-range scalable preset selection before hydration', async () => {
        const indexedDB = new IDBFactory()
        const databaseName = `bootstrap-scalable-invalid-preset-${crypto.randomUUID()}`
        const store = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', false)]
        persistent.botPresetsId = 99
        await store.open()
        await store.replaceFromDatabase(persistent)
        const readPreset = vi.spyOn(store, 'readPreset')

        const result = await bootstrapPersistentDatabase({
            store,
            now: () => 500,
            prepareDatabase: async () => {
                throw new Error('Scalable boot must not prepare a complete database')
            },
            prepareRoot: async (root) => structuredClone(root),
            projectScalableWorkingSet: (input) => projectCatalogWorkingSet(
                input.root,
                input.characters,
                createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset),
            ),
        })

        expect(result.revision).toBe(1)
        expect(result.database.botPresetsId).toBe(0)
        expect(result.database.botPresets[0]).toEqual(persistent.botPresets[0])
        expect(result.database.botPresets[1]).toEqual({
            id: persistent.botPresets[1]['id'],
            name: persistent.botPresets[1].name,
            image: persistent.botPresets[1].image,
        })
        expect(readPreset).toHaveBeenCalledOnce()
        expect(readPreset).toHaveBeenCalledWith('preset-beta')
        expect((await store.readRoot()).value.botPresetsId).toBe('')
    })

    it.each([1, 'preset-alpha'] as const)('projects selection %s without rewriting the stored boundary value', async (selection) => {
        const database = structuredClone(fixtureDatabase)
        const harness = await createPersistentStoreHarness(database)
        const {revision} = await harness.store.commit({expectedRevision:harness.revision,root:{...capturePersistentRoot(database),botPresetsId:selection}})
        const result = await bootstrapPersistentDatabase({store:harness.store,prepareDatabase:async(value)=>preparedResult(value,value),prepareRoot:async(root)=>root,
            projectScalableWorkingSet:(input)=>projectCatalogWorkingSet(input.root,input.characters,createCatalogPresetWorkingSet(input.presetCatalog,input.activePreset)),
        })
        expect(result.database.botPresetsId).toBe(1)
        expect(result.database.botPresets[1]['id']).toBe('preset-alpha')
        expect(result.revision).toBe(revision)
        const stored = await harness.store.readRoot()
        expect(stored.revision).toBe(revision)
        expect(stored.value.botPresetsId).toBe(selection)
    })

    it('projects a newly prepared scalable database after committing the complete import', async () => {
        const indexedDB = new IDBFactory()
        const databaseName = `bootstrap-new-scalable-${crypto.randomUUID()}`
        const store = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        const prepared = structuredClone(fixtureDatabase)
        prepared.plugins = [plugin('2.1', false)]
        prepared.botPresetsId = 1
        const replacement = vi.spyOn(store,'replaceFromDatabase')
        const commit = vi.spyOn(store,'commit')

        const result = await bootstrapPersistentDatabase({
            store,
            now: () => 500,
            prepareDatabase: async (database) =>
                preparedResult(database, prepared),
            projectScalableWorkingSet: (input) => projectCatalogWorkingSet(
                input.root,
                input.characters,
                createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset),
            ),
        })

        expect(replacement).toHaveBeenCalledOnce()
        expect(commit).not.toHaveBeenCalled()
        expect(result.revision).toBe(1)
        expect(await store.materializeDatabase(result.revision)).toEqual(prepared)
        expect(result.database.botPresets[0]).toEqual({
            id: prepared.botPresets[0]['id'],
            name: prepared.botPresets[0].name,
            image: prepared.botPresets[0].image,
        })
        expect(result.database.botPresets[1]).toEqual(prepared.botPresets[1])
        expect(result.database.characters.every(isCatalogCharacterStub)).toBe(true)
    })

    it('leaves trash expiry off the scalable startup critical path', async () => {
        const day = 24 * 60 * 60 * 1000
        const now = 10 * day
        const indexedDB = new IDBFactory()
        const store = new IndexedDbPersistentDataStore(
            `bootstrap-trash-expiry-${crypto.randomUUID()}`,
            indexedDB,
            IDBKeyRange,
        )
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', false)]
        persistent.characters[2].trashTime = now - 4 * day
        const recentTrash = structuredClone(persistent.characters[2])
        recentTrash.chaId = 'char-recent-trash'
        recentTrash.name = 'Recent trash'
        recentTrash.trashTime = now - 2 * day
        recentTrash.chats[0].id = 'conv-recent-trash'
        persistent.characters.push(recentTrash)
        persistent.characterOrder = [
            'char-b',
            'char-c',
            { id: 'folder', name: 'Folder', color: '', data: ['char-c', 'char-a'] },
        ]
        await store.open()
        await store.replaceFromDatabase(persistent)
        const queryCharacters = vi.spyOn(store, 'queryCharacters')

        const result = await bootstrapPersistentDatabase({
            store,
            now: () => now,
            prepareDatabase: async () => {
                throw new Error('Scalable boot must not materialize the database')
            },
            prepareRoot: async (root) => structuredClone(root),
            projectScalableWorkingSet: (input) => projectCatalogWorkingSet(
                input.root,
                input.characters,
                createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset),
            ),
        })

        expect(result.database.characters.map((character) => character.chaId))
            .toContain('char-c')
        expect(result.database.characters.map((character) => character.chaId))
            .toContain('char-recent-trash')
        expect(result.database.characterOrder).toEqual([
            'char-b',
            'char-c',
            { id: 'folder', name: 'Folder', color: '', data: ['char-c', 'char-a'] },
        ])
        expect(queryCharacters).toHaveBeenCalledTimes(2)
        const stored = await store.materializeDatabase(result.revision)
        expect(stored.characters.map((character) => character.chaId)).toContain('char-c')
        expect(stored.characters.find((character) => character.chaId === 'char-a')?.chats)
            .toEqual(persistent.characters[1].chats)
    })

    it('reports a conflict when another connection writes the blank store first', async () => {
        const indexedDB = new IDBFactory()
        const databaseName = `bootstrap-blank-conflict-${crypto.randomUUID()}`
        const store = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        const rival = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        await store.open()
        await rival.open()

        await expect(
            bootstrapPersistentDatabase({
                store,
                prepareDatabase: async (database) => {
                    await rival.replaceFromDatabase(structuredClone(fixtureDatabase))
                    return preparedResult(database, fixtureDatabase)
                },
            }),
        ).rejects.toBeInstanceOf(RevisionConflictError)
    })

    it('reports a conflict when another connection writes during normalization', async () => {
        const indexedDB = new IDBFactory()
        const databaseName = `bootstrap-normalize-conflict-${crypto.randomUUID()}`
        const store = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        const rival = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        await store.open()
        await rival.open()
        const persistent = structuredClone(fixtureDatabase)
        persistent.plugins = [plugin('2.1', true)]
        await store.replaceFromDatabase(persistent)

        await expect(
            bootstrapPersistentDatabase({
                store,
                prepareDatabase: async (database) => {
                    const normalized = structuredClone(database)
                    normalized.username = 'Normalized user'
                    await rival.replaceFromDatabase(structuredClone(persistent))
                    return preparedResult(database, normalized)
                },
            }),
        ).rejects.toBeInstanceOf(RevisionConflictError)
    })
})
