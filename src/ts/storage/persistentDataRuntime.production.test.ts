import { UNOWNED_PLUGIN_OWNER } from '../plugins/pluginOwner'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { flushSync } from 'svelte'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'

vi.mock('../parser/parser.svelte', () => ({
    assetRegex: /$^/,
    hasher: vi.fn(async () => 'hash'),
    parseMarkdownSafe: (value: string) => value,
    ParseMarkdown: vi.fn(async (value: string) => value),
    risuChatParser: (value: string) => value,
}))
import { selectedCharID } from '../stores.svelte'
import { doingChat } from '../process/generationState'
import { getRuntimePerformanceBudgets } from '../runtimePerformanceProfile'
import {
    createPluginStorageStore,
    registerPluginStorageLifecycle,
} from '../plugins/pluginStorageStore'
import { getV2PluginAPIs } from '../plugins/plugins.svelte'
import type { Database } from './database.svelte'
import type { PersistentDataStore } from './persistentDataStore'
import { getDatabase, getEffectivePresetId, normalizeDatabaseDefaults, setDatabase, setDatabaseLite, setEffectivePresetOverride } from './database.svelte'
import {
    configurePersistentDataRuntime,
    createProductionStateAdapter,
    hydrateCurrentGroupMemberDetail,
} from './persistentDataRuntime.svelte'
import {
    createCatalogCharacterStub,
    isCatalogCharacterStub,
    isCatalogPresetWorkingSet,
    projectCompleteScalableWorkingSet,
} from './workingSetCatalog'
import { workingSetResidency } from './workingSetResidency'
import { canonicalJson, SaveCoordinator } from './saveCoordinator'
import { capturePersistentRoot, createPersistentDataRuntime } from './persistentDataRuntime'
import { IndexedDbPersistentDataStore } from './indexedDbPersistentDataStore'
import { observePersistentSaveChanges } from './persistentSaveObserver.svelte'

afterEach(() => {
    configurePersistentDataRuntime({ projectWorkingSet: undefined })
    workingSetResidency.clear()
    workingSetResidency.setEvictionAllowed(true)
    doingChat.set(false)
})

describe('production persistent working-set publication', () => {
    it.each([true, false])('adopts a complete added preset without a follow-up publication (catalog=%s)', async (catalog) => {
        const initial = normalizeDatabaseDefaults({} as Database)
        initial.botPresets[0].id = 'existing-preset'
        initial.personas[0].id = 'existing-persona'
        const added = {...structuredClone(initial.botPresets[0]), id: 'added-preset', name: 'Added complete preset'}
        const store = new IndexedDbPersistentDataStore(`complete-added-preset-${catalog}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        const {revision} = await store.replaceFromDatabase(initial)
        setDatabaseLite(catalog ? projectCompleteScalableWorkingSet(initial, null, revision, new Set()) : initial)
        selectedCharID.set(-1)
        const runtime = createPersistentDataRuntime({store, state: createProductionStateAdapter(), prepareDatabase: async (value) => value})
        await runtime.initializeActiveWorkingSet(getDatabase())
        const commit = vi.spyOn(store, 'commit')
        const projectionReads: string[] = []
        const acquire = store.acquireRevision.bind(store)
        vi.spyOn(store, 'acquireRevision').mockImplementation(async (revision) => {
            const lease = await acquire(revision)
            const read = lease.readPreset.bind(lease)
            vi.spyOn(lease, 'readPreset').mockImplementation((id: string) => { projectionReads.push(id); return read(id) })
            return lease
        })
        await runtime.mutatePersistentPresets('add-complete-preset', (state) => { state.presets = [...state.presets, added] })
        expect((await store.readPreset('added-preset'))!.value).toEqual(added)
        if (!catalog) expect(getDatabase().botPresets.find((value) => value.id === added.id)).toEqual(added)
        await runtime.flushPendingDataLocally('added-preset-no-echo')
        expect(commit).toHaveBeenCalledOnce()
        expect(projectionReads).toEqual(catalog ? [] : ['added-preset'])
        expect(isCatalogPresetWorkingSet(getDatabase().botPresets)).toBe(catalog)
        const liveExisting = getDatabase().botPresets.find((value) => value.id === 'existing-preset')!
        liveExisting.mainPrompt = 'Retained loaded preset edit'
        await runtime.flushPendingDataLocally('loaded-preset-after-membership')
        expect(commit).toHaveBeenCalledTimes(2)
        expect(commit.mock.calls[1][0].unitMutations).toEqual([{key: '["preset","existing-preset","mainPrompt"]', type: 'set', value: 'Retained loaded preset edit'}])
        expect((await store.readPreset('existing-preset'))!.value.mainPrompt).toBe('Retained loaded preset edit')
    })

    it.each(['metadata', 'presets', 'delete', 'upsert', 'module'] as const)('keeps sparse activated root defaults local during the first targeted %s edit', async (scope) => {
        const initial = {username: 'Before', botPresets: [{id: 'initial-preset', name: 'Initial preset'}], botPresetsId: 0, characters: [], plugins: []} as unknown as Database
        const store = new IndexedDbPersistentDataStore(`sparse-activated-root-defaults-${scope}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        await store.replaceFromDatabase(initial)
        setDatabase(structuredClone(initial))
        selectedCharID.set(-1)
        const runtime = createPersistentDataRuntime({store, state: createProductionStateAdapter(), prepareDatabase: async (value) => value})
        await runtime.initializeActiveWorkingSet(getDatabase())
        await runtime.withPausedPersistentWrites('sparse-activation', async (token) => {
            const guard = runtime.beginActivatedLibraryGuard(token)
            await store.replaceFromDatabase({...initial, characters: [{type: 'character', chaId: 'sparse-owner', name: 'Owner', chatPage: 0,
                chatFolders: [], chats: [{id: 'sparse-chat', message: []}]}]} as unknown as Database, token.revision)
            await runtime.refreshActivatedLibraryUnderPause(token)
            guard.complete()
        })
        const commit = vi.spyOn(store, 'commit')
        await runtime.flushPendingDataLocally('sparse-activated-no-op')
        expect(commit).not.toHaveBeenCalled()
        if (scope === 'metadata') await runtime.mutatePersistentCharacterDetail('sparse-owner', 'first-targeted-edit', ({character}) => { if (character.type === 'group') throw new Error('Expected character'); character.desc = 'Real targeted edit' })
        if (scope === 'presets') await runtime.mutatePersistentPresets('first-targeted-preset', (state) => { state.root.username = 'Explicit root edit' })
        if (scope === 'delete') await runtime.deletePersistentCharacterWithGroupReferences('sparse-owner', 'first-targeted-delete')
        if (scope === 'upsert') await runtime.upsertPersistentCompleteCharacter('new-owner', 'first-targeted-upsert', () => ({type: 'character', chaId: 'new-owner', name: 'New', chatPage: 0, chatFolders: [], chats: []}) as Database['characters'][number])
        if (scope === 'module') await runtime.appendPersistentRootModule('first-targeted-module', {module: {id: 'new-module', name: 'New module', description: ''}, assetAliases: [], ownerHead: {present: false, manifestHash: null, entryCount: 0}})
        await runtime.flushPendingDataLocally('after-targeted-edit')
        expect(commit).toHaveBeenCalledOnce()
        if (scope === 'metadata') expect(commit.mock.calls[0][0]).toMatchObject({unitMutations: [{key: '["character","sparse-owner","desc"]', type: 'set', value: 'Real targeted edit'}]})
        expect(commit.mock.calls.flatMap(([input]) => input.rootMutations ?? []).filter((value) => value.key !== 'characterOrder')).toEqual(scope === 'presets' ? [{type: 'set', key: 'username', value: 'Explicit root edit'}] : [])
        expect((await store.readRoot()).value).not.toHaveProperty('translatorMaxResponse')
    })

    it.each([false, true])('retains explicit and concurrent root deltas without copying defaults (failure=%s)', async (fail) => {
        const initial = {username: 'Before', translator: 'Before', botPresets: [{id: 'preset', name: 'Preset'}], botPresetsId: 0, personas: [{id: 'persona', name: 'Persona', prompt: ''}], selectedPersona: 0, plugins: [],
            opaqueRoot: {before: true}, removedRoot: 'Remove', characters: [{type: 'character', chaId: 'owner', name: 'Owner', chats: []}]} as unknown as Database
        const store = new IndexedDbPersistentDataStore(`sparse-concurrent-root-${fail}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        await store.replaceFromDatabase(initial)
        setDatabase(structuredClone(initial))
        selectedCharID.set(-1)
        const runtime = createPersistentDataRuntime({store, state: createProductionStateAdapter(), prepareDatabase: async (value) => value})
        await runtime.initializeActiveWorkingSet(getDatabase())
        const commit = vi.spyOn(store, 'commit')
        if (fail) commit.mockRejectedValueOnce(new Error('Synthetic root commit failure'))
        let attempts = 0
        const mutate = () => runtime.mutatePersistentCharacterDetail('owner', 'explicit-concurrent-root', async ({root, character}) => {
            const concurrent = ++attempts === 1 ? 'Concurrent edit' : 'Concurrent retry edit'
            root.translator = 'Callback edit'
            ;(root as unknown as Record<string, unknown>).opaqueRoot = {explicit: true}
            ;(root as unknown as Record<string, unknown>).nullableRoot = null
            if (character.type === 'group') throw new Error('Expected character')
            character.desc = 'Explicit detail'
            await Promise.resolve()
            getDatabase().translator = concurrent
            const live = getDatabase() as unknown as Record<string, unknown>
            delete live.removedRoot
            live.addedRoot = {concurrent: true}
        })
        if (fail) {
            await expect(mutate()).rejects.toThrow('Synthetic root commit failure')
            expect((await store.readRoot()).value).toMatchObject({username: 'Before', translator: 'Before', opaqueRoot: {before: true}, removedRoot: 'Remove'})
            expect((await store.readCharacter('owner'))!.value).not.toHaveProperty('desc')
            await runtime.flushPendingDataLocally('retry-live-root-deltas')
            expect((await store.readRoot()).value.translator).toBe('Concurrent edit')
        }
        await mutate()
        const accepted = (await store.readRoot()).value
        expect(accepted).toMatchObject({translator: 'Callback edit', opaqueRoot: {explicit: true}, addedRoot: {concurrent: true}, nullableRoot: null})
        expect(accepted).not.toHaveProperty('removedRoot')
        expect(accepted).not.toHaveProperty('translatorMaxResponse')
        expect((await store.readCharacter('owner'))!.value).toMatchObject({desc: 'Explicit detail'})
        await runtime.flushPendingDataLocally('remaining-concurrent-view')
        expect((await store.readRoot()).value.translator).toBe(fail ? 'Concurrent retry edit' : 'Concurrent edit')
        const roots = commit.mock.calls.flatMap(([input]) => input.rootMutations ?? [])
        expect(roots.map((value) => value.key).filter((key) => !['translator', 'opaqueRoot', 'removedRoot', 'addedRoot', 'nullableRoot'].includes(key))).toEqual([])
    })

    it('observes and persists edits through a retained module after a preset operation', async () => {
        const initial = {
            username: 'Before', botPresets: [], botPresetsId: 0, characters: [], plugins: [],
            modules: [{ id: 'module', name: 'Synthetic module', description: '' }],
        } as unknown as Database
        const indexedDB = new IDBFactory()
        const store = new IndexedDbPersistentDataStore('root-publication-edit', indexedDB, IDBKeyRange)
        await store.open()
        const { revision } = await store.replaceFromDatabase(initial)
        setDatabaseLite(initial)
        selectedCharID.set(-1)
        const adapter = createProductionStateAdapter()
        const coordinator = new SaveCoordinator({ store, ...adapter })
        coordinator.initialize(revision)
        const markDirty = vi.fn((bytes: number) => coordinator.markPersistentDataDirty(bytes))
        const dispose = observePersistentSaveChanges({
            readDatabase: getDatabase, readSelectedCharacter: () => null, markDirty,
        })
        try {
            flushSync()
            const module = getDatabase().modules[0]
            await coordinator.mutatePersistentPresets('synthetic-preset-operation', ({ root }) => {
                root.username = 'After'
            })
            flushSync()
            markDirty.mockClear()
            module.description = 'Edited after publication'
            flushSync()
            expect(markDirty).toHaveBeenCalled()
            await coordinator.flushPendingDataLocally('synthetic-retained-editor')
            const reopened = new IndexedDbPersistentDataStore('root-publication-edit', indexedDB, IDBKeyRange)
            await reopened.open()
            expect((await reopened.readRoot()).value).toMatchObject({
                username: 'After', modules: [{ description: 'Edited after publication' }],
            })
        } finally {
            dispose()
        }
    })

    it.each(['root', 'preset', 'character'] as const)(
        'keeps unchanged editor data attached during %s publication', (kind) => {
            setDatabaseLite({
                username: 'Before', botPresets: [], characters: [], plugins: [],
                promptTemplate: [{ type: 'plain', text: 'Synthetic prompt', role: 'system' }],
                modules: [{ id: 'synthetic-module', name: 'Synthetic module', description: '' }],
                personas: [{ id: 'synthetic-persona', name: 'Synthetic persona', prompt: '' }],
                globalscript: [{ comment: 'Synthetic regex', in: '', out: '', type: 'editinput' }],
            } as unknown as Database)
            const database = getDatabase()
            const references = [database.promptTemplate, database.modules, database.personas, database.globalscript]
            const root = JSON.parse(JSON.stringify(capturePersistentRoot(database)))
            root.username = 'After'
            const adapter = createProductionStateAdapter()
            if (kind === 'root') adapter.publishRootWorkingSet!(root)
            if (kind === 'preset') adapter.publishPresetWorkingSet!({ revision: 2, root, presets: [] })
            if (kind === 'character') adapter.publishCharacterMutation!({
                revision: 2, root, kind: 'detail', characterId: 'absent', character: null,
            })
            expect(database.username).toBe('After')
            for (const [index, current] of [database.promptTemplate, database.modules, database.personas, database.globalscript].entries()) {
                expect(current).toBe(references[index])
            }
            const module = references[1][0] as Database['modules'][number]
            module.description = 'Edited after publication'
            expect(adapter.captureRoot().modules[0].description).toBe('Edited after publication')
        },
    )

    it.each(['character', 'group'])('tracks cold selection and subsequent %s edits in the same canonical capture', (type) => {
        setDatabaseLite({
            botPresets: [], plugins: [], pluginCustomStorage: {},
            characters: ['a', 'b'].map((id) => ({
                type, chaId: id, name: id, chatPage: 0,
                ...(type === 'group' ? { characters: [], characterTalks: [] } : {}),
                chats: [{ id: `chat-${id}`, note: '', message: [] }],
            })),
        } as unknown as Database)
        selectedCharID.set(-1)
        const adapter = createProductionStateAdapter()
        const capture = adapter.canonicalCapture!
        expect(capture.character()).toBeNull()
        for (const index of [0, 1, 0]) {
            selectedCharID.set(index)
            expect(capture.character()).toBe(canonicalJson(adapter.captureSelectedCharacter()))
            const character = getDatabase().characters[index]
            character.name = '수정 🙂'
            character.chats[0].note = 'Synthetic nested edit'
            expect(capture.character()).toBe(canonicalJson(adapter.captureSelectedCharacter()))
            character.name = ''
            expect(capture.character()).toBe(canonicalJson(adapter.captureSelectedCharacter()))
        }
    })

    it.each(['modules', 'loadouts', 'customModels', 'personas', 'protectedPresetValues', 'explicitGlobalChatVariables'] as const)(
        'converges an empty %s projection omitted from the accepted root without a durable write', async (field) => {
            const initial = {username: 'Before', botPresets: [], botPresetsId: 0, characters: [], plugins: [],
                globalChatVariables: {}, explicitGlobalChatVariables: {}} as unknown as Database
            delete initial[field]
            setDatabaseLite(initial)
            selectedCharID.set(-1)
            const adapter = createProductionStateAdapter()
            const commit = vi.fn(async ({expectedRevision}) => ({revision: expectedRevision + 1}))
            const coordinator = new SaveCoordinator({store: {commit} as unknown as PersistentDataStore, ...adapter})
            coordinator.initialize(1)
            const accepted = adapter.captureRoot()
            delete accepted[field]
            coordinator.adoptAppliedUnitState(1, accepted, null, [])
            const current = getDatabase() as unknown as Record<string, unknown>
            current[field] = field.endsWith('Values') || field === 'explicitGlobalChatVariables' ? {} : []
            const capture = adapter.canonicalCapture!
            const root = capture.root.bind(capture)
            let captures = 0
            vi.spyOn(capture, 'root').mockImplementation(() => {
                if (++captures > 12) throw new Error('Non-converging empty split-root projection')
                return root()
            })
            coordinator.markPersistentDataDirty(1)
            await coordinator.flushPendingDataLocally('empty-split-root-projection')
            expect(commit).not.toHaveBeenCalled()
            expect(coordinator.revision).toBe(1)
            expect(coordinator.hasPendingPersistenceWork).toBe(false)
            expect(captures).toBeLessThanOrEqual(2)
            getDatabase().username = 'Real edit'
            coordinator.markPersistentDataDirty(1)
            await coordinator.flushPendingDataLocally('after-empty-projection')
            expect(commit).toHaveBeenCalledExactlyOnceWith({expectedRevision: 1,
                rootMutations: [{key: 'username', type: 'set', value: 'Real edit'}]})
        },
    )

    it.each(['explicitGlobalChatVariables', 'protectedPresetValues'] as const)(
        'persists real %s additions and deletions after empty projection convergence', async (field) => {
            setDatabaseLite({username: 'Before', botPresets: [], botPresetsId: 0, characters: [], plugins: [],
                globalChatVariables: {}, explicitGlobalChatVariables: {}, protectedPresetValues: {}} as unknown as Database)
            selectedCharID.set(-1)
            const adapter = createProductionStateAdapter()
            const commit = vi.fn(async ({expectedRevision}) => ({revision: expectedRevision + 1}))
            const coordinator = new SaveCoordinator({store: {commit} as unknown as PersistentDataStore, ...adapter})
            coordinator.initialize(1)
            const accepted = adapter.captureRoot()
            delete accepted[field]
            coordinator.adoptAppliedUnitState(1, accepted, null, [])
            coordinator.markPersistentDataDirty(1)
            await coordinator.flushPendingDataLocally('empty-map-projection')
            expect(commit).not.toHaveBeenCalled()
            const map = getDatabase()[field] as Record<string, unknown>
            const key = field === 'explicitGlobalChatVariables' ? 'ordinary' : 'seperateModels'
            const unitKey = JSON.stringify([field === 'explicitGlobalChatVariables' ? 'variable' : 'preset-protected', key])
            const value = field === 'explicitGlobalChatVariables' ? 'Real value' : {memory: 'Real value'}
            map[key] = value
            coordinator.markPersistentDataDirty(1)
            await coordinator.flushPendingDataLocally('real-map-add')
            expect(commit).toHaveBeenCalledExactlyOnceWith({expectedRevision: 1, rootMutations: [],
                unitMutations: [{key: unitKey, type: 'set', value}]})
            delete map[key]
            coordinator.markPersistentDataDirty(1)
            await coordinator.flushPendingDataLocally('real-map-delete')
            expect(commit).toHaveBeenCalledTimes(2)
            expect(commit.mock.calls[1][0]).toEqual({expectedRevision: 2, rootMutations: [],
                unitMutations: [{key: unitKey, type: 'delete'}]})
            await coordinator.flushPendingDataLocally('empty-map-again')
            expect(commit).toHaveBeenCalledTimes(2)
            expect(coordinator.hasPendingPersistenceWork).toBe(false)
        },
    )

    it('retains genuine split-root units when an empty projection accompanies a failed commit', async () => {
        setDatabaseLite({username: 'Before', botPresets: [], botPresetsId: 0, characters: [], plugins: [],
            globalChatVariables: {}, explicitGlobalChatVariables: {},
            modules: [{id: 'module', name: 'Module', description: 'Before'}]} as unknown as Database)
        selectedCharID.set(-1)
        const adapter = createProductionStateAdapter()
        const commit = vi.fn().mockRejectedValueOnce(new Error('Synthetic failure'))
            .mockImplementation(async ({expectedRevision}) => ({revision: expectedRevision + 1}))
        const coordinator = new SaveCoordinator({store: {commit} as unknown as PersistentDataStore, ...adapter})
        coordinator.initialize(1)
        const accepted = adapter.captureRoot()
        delete accepted.explicitGlobalChatVariables
        coordinator.adoptAppliedUnitState(1, accepted, null, [])
        getDatabase().modules[0].description = 'Real record edit'
        coordinator.markPersistentDataDirty(1)
        await expect(coordinator.flushPendingDataLocally('mixed-empty-real-unit')).rejects.toThrow('Synthetic failure')
        expect(coordinator.hasPendingPersistenceWork).toBe(true)
        await coordinator.flushPendingDataLocally('retry-mixed-empty-real-unit')
        expect(commit).toHaveBeenCalledTimes(2)
        for (const [input] of commit.mock.calls) expect(input).toEqual({expectedRevision: 1, rootMutations: [], unitMutations: [
            {key: '["record","modules","module"]', type: 'set', value: {id: 'module', name: 'Module', description: 'Real record edit'}},
        ]})
        expect(coordinator.hasPendingPersistenceWork).toBe(false)
    })

    it('uses canonical captures for immediate production edits and emits a small delta', async () => {
        setDatabaseLite({
            username: 'Before',
            customBackground: 'x'.repeat(6 * 1024 * 1024),
            botPresets: [],
            plugins: [],
            characters: [
                {
                    type: 'character',
                    chaId: 'synthetic-large',
                    name: 'Synthetic',
                    chatPage: 0,
                    chats: [
                        {
                            id: 'synthetic-chat',
                            message: [{ role: 'char', data: 'x'.repeat(2 * 1024 * 1024) }],
                        },
                    ],
                },
            ],
            pluginCustomStorage: {},
        } as unknown as Database)
        selectedCharID.set(0)
        const adapter = createProductionStateAdapter()
        expect(adapter.canonicalCapture).toBeDefined()
        const commit = vi.fn(async () => ({ revision: 2 }))
        const coordinator = new SaveCoordinator({
            ...adapter,
            captureRoot: () => {
                throw new Error('Must use the production canonical capture')
            },
            store: { commit } as unknown as PersistentDataStore,
        })
        coordinator.initialize(1)
        getDatabase().username = 'Immediately changed'
        coordinator.markPersistentDataDirty(1)
        const parse = vi.spyOn(JSON, 'parse')
        try {
            await coordinator.flushPendingData('immediate-production-mutation')
            expect(parse.mock.calls.some(([value]) => value.length > 1024 * 1024)).toBe(false)
        } finally {
            parse.mockRestore()
        }
        expect(commit).toHaveBeenCalledExactlyOnceWith({
            expectedRevision: 1,
            rootMutations: [{ type: 'set', key: 'username', value: 'Immediately changed' }],
        })
        expect(JSON.stringify(commit.mock.calls[0]).length).toBeLessThan(256)
    })

    it('adopts a small production root edit without decoding an unchanged large background in the renderer', async () => {
        const initial = {
            username: 'Before', customBackground: 'x'.repeat(6 * 1024 * 1024),
            botPresets: [], plugins: [], characters: [], pluginCustomStorage: {},
            modules: [{ id: 'module', name: 'Before', description: '' }],
        } as unknown as Database
        const store = new IndexedDbPersistentDataStore('cached-root-adoption', new IDBFactory(), IDBKeyRange)
        await store.open()
        await store.replaceFromDatabase(initial)
        setDatabaseLite(initial)
        selectedCharID.set(-1)
        const adapter = createProductionStateAdapter()
        const runtime = createPersistentDataRuntime({ store, state: adapter, prepareDatabase: async (value) => value })
        await runtime.initializeActiveWorkingSet(getDatabase())
        const coordinator = new SaveCoordinator({ store, ...adapter })
        coordinator.initialize(runtime.revision)
        const originalParse = JSON.parse
        let inBackendCommit = false
        const rendererParseLengths: number[] = []
        const parse = vi.spyOn(JSON, 'parse').mockImplementation((value, reviver) => {
            if (!inBackendCommit) rendererParseLengths.push(value.length)
            return originalParse(value, reviver)
        })
        const originalCommit = store.commit.bind(store)
        // IndexedDB's backend root clone is outside the renderer capture/adoption path.
        const commit = vi.spyOn(store, 'commit').mockImplementation(async (request) => {
            inBackendCommit = true
            try { return await originalCommit(request) }
            finally { inBackendCommit = false }
        })
        try {
            const baseline = coordinator.capturePersistentBaselineRoot()
            baseline.modules[0].name = 'Detached baseline only'
            expect(coordinator.capturePersistentBaselineRoot().modules[0].name).toBe('Before')
            getDatabase().modules[0].name = 'After'
            runtime.markPersistentDataDirty(1)
            await runtime.flushPendingData('cached-root-adoption')
            runtime.markPersistentDataDirty(1)
            await runtime.flushPendingData('cached-root-no-echo')
            expect(rendererParseLengths.some((length) => length > 1024 * 1024)).toBe(false)
        } finally {
            parse.mockRestore()
        }
        expect(commit).toHaveBeenCalledTimes(1)
        const persisted = (await store.readRoot()).value
        expect(persisted.modules[0].name).toBe('After')
        expect(persisted.customBackground).toBe(initial.customBackground)
    })

    it.each(['root', 'preset', 'mirror', 'selection'] as const)('retains concurrent catalog preset edits during async %s unit projection', async (scope) => {
        const initial = {
            language: 'en', mainPrompt: 'Initial prompt', botPresetsId: 0,
            botPresets: [{ id: 'resident-preset', name: 'Initial preset', mainPrompt: 'Initial prompt' },
                { id: 'other-preset', name: 'Other preset', mainPrompt: 'Other prompt' }],
            plugins: [], characters: [], pluginCustomStorage: {},
        } as unknown as Database
        const store = new IndexedDbPersistentDataStore(`catalog-preset-race-${scope}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        const { revision } = await store.replaceFromDatabase(initial)
        setDatabaseLite(projectCompleteScalableWorkingSet(initial, null, revision))
        selectedCharID.set(-1)
        const adapter = createProductionStateAdapter()
        const runtime = createPersistentDataRuntime({store, state: adapter, prepareDatabase: async (value) => value})
        await runtime.initializeActiveWorkingSet(getDatabase())
        expect(adapter.capturePresets?.()).toBeNull()
        expect(adapter.capturePresetRecords?.()).toHaveLength(1)
        let entered!: () => void
        const reading = new Promise<void>((resolve) => { entered = resolve })
        let release!: () => void
        const blocked = new Promise<void>((resolve) => { release = resolve })
        const acquire = store.acquireRevision.bind(store)
        vi.spyOn(store, 'acquireRevision').mockImplementationOnce(async (requested) => {
            const lease = await acquire(requested)
            if (scope === 'preset' || scope === 'selection') {
                const read = lease.readPreset.bind(lease)
                vi.spyOn(lease, 'readPreset').mockImplementationOnce(async (id) => {
                    const value = await read(id)
                    entered()
                    await blocked
                    return value
                })
            } else {
                const read = lease.readRoot.bind(lease)
                vi.spyOn(lease, 'readRoot').mockImplementationOnce(async () => {
                    const value = await read()
                    entered()
                    await blocked
                    return value
                })
            }
            return lease
        })
        const commit = vi.spyOn(store, 'commit')
        const mirrorEdit = scope === 'mirror' || scope === 'selection'
        const projected = runtime.commitPersistentUnitIntent('catalog-preset-race', [{
            key: scope === 'selection' ? '["root","botPresetsId"]' : scope === 'preset' ? '["preset","resident-preset","name"]' : '["root","language"]',
            type: 'set', value: scope === 'selection' ? 'other-preset' : scope === 'preset' ? 'Received preset' : 'ko',
        }])
        await reading
        if (mirrorEdit) getDatabase().mainPrompt = 'Concurrent prompt'
        else getDatabase().botPresets[0].name = 'Concurrent preset'
        runtime.markPersistentDataDirty(1)
        release()
        await projected
        if (mirrorEdit) {
            expect(getDatabase().mainPrompt).toBe(scope === 'selection' ? 'Other prompt' : 'Concurrent prompt')
            expect(getDatabase().botPresets.find((value) => value['id'] === 'resident-preset')?.mainPrompt).toBe('Concurrent prompt')
            expect((await store.readPreset('resident-preset'))?.value.mainPrompt).toBe('Initial prompt')
        } else {
            expect(getDatabase().botPresets[0].name).toBe('Concurrent preset')
            expect((await store.readPreset('resident-preset'))?.value.name).toBe(scope === 'root' ? 'Initial preset' : 'Received preset')
        }
        await runtime.flushPendingDataLocally('persist-concurrent-catalog-preset')
        if (mirrorEdit) {
            expect((await store.readPreset('resident-preset'))?.value.mainPrompt).toBe('Concurrent prompt')
            expect((await store.readPreset('other-preset'))?.value.mainPrompt).toBe('Other prompt')
        } else expect((await store.readPreset('resident-preset'))?.value.name).toBe('Concurrent preset')
        expect(commit).toHaveBeenCalledTimes(2)
        await runtime.flushPendingDataLocally('catalog-preset-no-echo')
        expect(commit).toHaveBeenCalledTimes(2)
    })

    it('adopts received protected flags and explicit toggles without creating preset or toggle echoes', async () => {
        const initial = {
            botPresetsId: 0, doNotChangeSeperateModels: false, seperateModels: {memory: 'initial'},
            protectedPresetValues: {}, explicitGlobalChatVariables: {toggle_mode: 'initial'},
            globalChatVariables: {toggle_mode: 'initial'},
            botPresets: [{id: 'resident-preset', name: 'Preset', seperateModels: {memory: 'initial'}}],
            plugins: [], characters: [], pluginCustomStorage: {},
        } as unknown as Database
        const store = new IndexedDbPersistentDataStore('catalog-protected-no-echo', new IDBFactory(), IDBKeyRange) as PersistentDataStore
        await store.open()
        const {revision} = await store.replaceFromDatabase(initial)
        setDatabaseLite(projectCompleteScalableWorkingSet(initial, null, revision))
        selectedCharID.set(-1)
        const adapter = createProductionStateAdapter()
        const runtime = createPersistentDataRuntime({store, state: adapter, prepareDatabase: async (value) => value})
        await runtime.initializeActiveWorkingSet(getDatabase())
        const mutations = [
            {key: '["root","doNotChangeSeperateModels"]', type: 'set' as const, value: true},
            {key: '["preset-protected","seperateModels"]', type: 'set' as const, value: {memory: 'received'}},
            {key: '["toggle","toggle_mode"]', type: 'set' as const, value: 'received-toggle'},
        ]
        store.lwwStageReceive = async () => undefined
        store.lwwApplyReceive = async () => {
            const result = await store.commit({expectedRevision: runtime.revision, unitMutations: mutations})
            return {...result, affectedKeys: mutations.map((value) => value.key), heldKeys: [], deferredKeys: []}
        }
        store.lwwFinishReceive = async () => undefined
        const commit = vi.spyOn(store, 'commit')
        await runtime.applyLwwReceive({bindingAuthority: '1', requestId: 'protected-receive', changes: [], progress: {kind: 'server', cursor: '1'}, admittedTimeUpperMs: '100'})
        expect(getDatabase().doNotChangeSeperateModels).toBe(true)
        expect(getDatabase().protectedPresetValues?.seperateModels).toEqual({memory: 'received'})
        expect(getDatabase().seperateModels).toEqual({memory: 'received'})
        expect(getDatabase().globalChatVariables.toggle_mode).toBe('received-toggle')
        expect(getDatabase().explicitGlobalChatVariables?.toggle_mode).toBe('received-toggle')
        expect((await store.readPreset('resident-preset'))?.value.seperateModels).toEqual({memory: 'initial'})
        commit.mockClear()
        await runtime.flushPendingDataLocally('protected-receive-no-echo')
        expect(commit).not.toHaveBeenCalled()
    })

    it('restores only affected activation entries, selection and residency', () => {
        const selected = {
            type: 'character',
            chaId: 'char-a',
            name: 'Selected',
            chats: [
                {
                    id: 'chat-a',
                    name: 'Chat',
                    note: '',
                    localLore: [],
                    message: [],
                },
            ],
            chatPage: 0,
        }
        const related = createCatalogCharacterStub({
            id: 'char-b',
            configuredIndex: 1,
            conversationCount: 2,
            name: 'Related',
            type: 'character',
            recentAt: 0,
            trashed: false,
        })
        const unaffected = {
            type: 'character',
            chaId: 'char-c',
            name: 'Unaffected',
            chats: [],
        }
        setDatabaseLite({
            botPresets: [],
            plugins: [],
            characters: [selected, related, unaffected],
        } as unknown as Database)
        selectedCharID.set(0)
        const residentSelected = getDatabase().characters[0]
        const residentRelated = getDatabase().characters[1]
        workingSetResidency.markCharacterHydrated('char-a')
        workingSetResidency.reconcileConversationResidency(residentSelected)
        workingSetResidency.markCharacterReleased('char-b')
        const restore = createProductionStateAdapter()
            .captureActivationRollback!(['char-a', 'char-b'])

        getDatabase().characters[0] = {
            type: 'character',
            chaId: 'char-a',
            name: 'Tentative selected',
            chats: [],
        } as any
        getDatabase().characters[1] = {
            type: 'character',
            chaId: 'char-b',
            name: 'Tentative related',
            chats: [],
        } as any
        getDatabase().characters[2].name = 'Concurrent unaffected edit'
        selectedCharID.set(1)
        workingSetResidency.markCharacterReleased('char-a')
        workingSetResidency.markCharacterHydrated('char-b')

        restore()

        expect(getDatabase().characters[0]).toBe(residentSelected)
        expect(getDatabase().characters[1]).toBe(residentRelated)
        expect(getDatabase().characters[2].name).toBe(
            'Concurrent unaffected edit',
        )
        expect(get(selectedCharID)).toBe(0)
        expect(workingSetResidency.isCharacterReleased('char-a')).toBe(false)
        expect(
            workingSetResidency.canReleaseConversation(
                residentSelected,
                'chat-a',
            ),
        ).toBe(false)
        expect(workingSetResidency.isCharacterReleased('char-b')).toBe(true)
    })

    it('hydrates only the restored catalog member while preserving the selected group', () => {
        const group = {
            type: 'group',
            chaId: 'group-a',
            name: 'Group',
            characters: ['member-a'],
            characterTalks: [1],
            characterActive: [true],
            chats: [{ id: 'group-chat', message: [] }],
            chatPage: 0,
        }
        const memberStub = createCatalogCharacterStub({
            id: 'member-b',
            configuredIndex: 1,
            conversationCount: 0,
            name: 'Beta',
            type: 'character',
            recentAt: 0,
            trashed: false,
        })
        const database = {
            botPresets: [],
            plugins: [],
            characters: [group, memberStub],
        } as unknown as Database
        setDatabaseLite(database)
        selectedCharID.set(0)
        const residentGroup = getDatabase().characters[0]
        const residentMemberStub = getDatabase().characters[1]
        const detail = {
            type: 'character',
            chaId: 'member-b',
            name: 'Beta',
            personality: 'Persistent personality',
            scenario: 'Persistent scenario',
        } as any

        expect(hydrateCurrentGroupMemberDetail('group-a', detail)).toBe(true)

        expect(getDatabase().characters[0]).toBe(residentGroup)
        expect(getDatabase().characters[1]).toMatchObject({
            personality: 'Persistent personality',
            scenario: 'Persistent scenario',
        })
        expect(isCatalogCharacterStub(getDatabase().characters[1])).toBe(false)
        expect(getDatabase().characters[1].chats).toBe(residentMemberStub.chats)
        expect(getDatabase().characters[get(selectedCharID)]).toBe(residentGroup)
    })

    it('leaves an already complete maximum-compatibility member unchanged', () => {
        const group = {
            type: 'group',
            chaId: 'group-a',
            characters: [],
            characterTalks: [],
            characterActive: [],
            chats: [],
        }
        const member = {
            type: 'character',
            chaId: 'member-b',
            personality: 'Complete personality',
            chats: [],
        }
        setDatabaseLite({
            botPresets: [],
            plugins: [{ enabled: true, version: '2.1' }],
            characters: [group, member],
        } as unknown as Database)
        selectedCharID.set(0)
        const residentMember = getDatabase().characters[1]

        expect(hydrateCurrentGroupMemberDetail('group-a', {
            type: 'character',
            chaId: 'member-b',
            personality: 'Replacement personality',
        } as any)).toBe(true)

        expect(getDatabase().characters[1]).toBe(residentMember)
        expect(getDatabase().characters[1].personality).toBe('Complete personality')
    })

    it('reports selected lifecycle policy, operation and viewport budget', () => {
        setDatabaseLite({
            botPresets: [],
            characters: [],
            plugins: [],
        } as unknown as Database)
        workingSetResidency.setEvictionAllowed(true)
        doingChat.set(false)
        const adapter = createProductionStateAdapter()
        const operationTransitions: boolean[] = []
        const unsubscribeOperation = adapter.subscribeConversationOperationActive?.(
            (active) => operationTransitions.push(active),
        )

        expect(adapter.canUseWindowedSelectedConversation?.()).toBe(true)
        expect(adapter.isConversationOperationActive?.()).toBe(false)
        expect(adapter.conversationViewportRowBudget).toBe(
            getRuntimePerformanceBudgets().chatMountedMessageBudget,
        )

        workingSetResidency.setEvictionAllowed(false)
        doingChat.set(true)

        expect(adapter.canUseWindowedSelectedConversation?.()).toBe(false)
        expect(adapter.isConversationOperationActive?.()).toBe(true)
        doingChat.set(false)
        expect(operationTransitions).toEqual([false, true, false])
        unsubscribeOperation?.()
    })

    it('clears old residency before the synchronous projector records the replacement', () => {
        const initial = {
            botPresetsId: 0,
            botPresets: [{ name: 'Active' }],
            characters: [{
                type: 'group',
                chaId: 'group-a',
                name: 'Group',
                characters: ['member-a'],
                characterTalks: [1],
                characterActive: [true],
                chats: [],
            }],
        } as unknown as Database
        const replacement = {
            ...initial,
            characters: [{
                type: 'character',
                chaId: 'member-a',
                name: 'Member',
                personality: 'resident detail',
                chats: [],
            }, initial.characters[0], {
                type: 'character',
                chaId: 'inactive',
                name: 'Inactive',
                personality: 'must be released',
                chats: [],
            }],
        } as unknown as Database
        setDatabaseLite(initial)
        selectedCharID.set(0)
        workingSetResidency.markCharacterReleased('stale')
        let oldResidencyClearedBeforeProjection = false
        configurePersistentDataRuntime({
            projectWorkingSet(database, selectedCharacterId, selectedConversationId, activeIds) {
                oldResidencyClearedBeforeProjection =
                    !workingSetResidency.isCharacterReleased('stale')
                const projected = projectCompleteScalableWorkingSet(
                    database,
                    selectedCharacterId,
                    2,
                    activeIds,
                    selectedConversationId,
                )
                for (const character of projected.characters) {
                    if (isCatalogCharacterStub(character)) {
                        workingSetResidency.markCharacterReleased(character.chaId)
                    }
                }
                return projected
            },
        })

        createProductionStateAdapter().replaceDatabase(
            replacement,
            new Set(['group-a', 'member-a']),
            true,
        )

        expect(oldResidencyClearedBeforeProjection).toBe(true)
        expect(workingSetResidency.isCharacterReleased('inactive')).toBe(true)
        expect(getDatabase().characters[0].personality).toBe('resident detail')
        expect(isCatalogCharacterStub(getDatabase().characters[2])).toBe(true)
    })

    it('preloads nested maximum plugin values from the installed plain database', async () => {
        const nestedValue = {
            list: [{ enabled: true }],
            settings: { mode: 'maximum' },
        }
        const storage = createPluginStorageStore({
            getStorageAuthorityEpoch: () => 0,
            assertPersistentMutationAllowed: vi.fn(),
            store: {
                open: async () => undefined,
                queryPluginStorage: async () => ({
                    revision: 1,
                    items: [{ owner: UNOWNED_PLUGIN_OWNER, key: 'nested', byteSize: 1 }],
                }),
                readPluginStorage: async () => ({ revision: 1, value: nestedValue }),
            } as unknown as PersistentDataStore,
            mutate: async () => undefined,
        })
        const unregister = registerPluginStorageLifecycle(storage)
        const complete = {
            botPresets: [],
            pluginCustomStorage: { nested: nestedValue },
            characters: [],
        } as unknown as Database
        try {
            createProductionStateAdapter().installCompleteDatabase!(complete)

            await expect(storage.forOwner(UNOWNED_PLUGIN_OWNER).keys()).resolves.toEqual(['nested'])
            await expect(storage.forOwner(UNOWNED_PLUGIN_OWNER).getItem('nested')).resolves.toEqual(nestedValue)
            expect(await storage.forOwner(UNOWNED_PLUGIN_OWNER).getItem('nested')).not.toBe(nestedValue)
        } finally {
            unregister()
        }
    })
})

describe('preset chain override across a whole working-set replacement', () => {
    function presetDatabase(): Database {
        const database = normalizeDatabaseDefaults({} as Database)
        database.botPresets[0].id = 'preset-a'
        database.botPresets[0].mainPrompt = 'Prompt A'
        database.botPresets.push({ ...structuredClone(database.botPresets[0]), id: 'preset-b', name: 'Preset B', mainPrompt: 'Prompt B' })
        database.botPresetsId = 0
        database.mainPrompt = 'Prompt A'
        database.personas[0].id = 'persona'
        return database
    }

    it.each([true, false])('keeps the override when its preset is still present (resident=%s)', async (resident) => {
        const initial = presetDatabase()
        const store = new IndexedDbPersistentDataStore(`override-carry-${resident}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        const { revision } = await store.replaceFromDatabase(initial)
        setDatabase(structuredClone(initial))
        selectedCharID.set(-1)
        setEffectivePresetOverride('preset-b')
        expect(getDatabase().mainPrompt).toBe('Prompt B')
        const replacement = resident ? structuredClone(initial) : projectCompleteScalableWorkingSet(initial, null, revision, new Set())
        createProductionStateAdapter({ readPreset: (id) => store.readPreset(id) }).replaceDatabase(replacement, new Set(), false)
        await vi.waitFor(() => expect(getEffectivePresetId()).toBe('preset-b'))
        expect(getDatabase().mainPrompt).toBe('Prompt B')
        expect(getDatabase().botPresets.find((preset) => preset.id === 'preset-a')!.mainPrompt).toBe('Prompt A')
    })

    it('clears the override when its preset is gone', () => {
        const initial = presetDatabase()
        setDatabase(structuredClone(initial))
        selectedCharID.set(-1)
        setEffectivePresetOverride('preset-b')
        const replacement = structuredClone(initial)
        replacement.botPresets.pop()
        createProductionStateAdapter().replaceDatabase(replacement, new Set(), false)
        expect(getEffectivePresetId()).toBe('preset-a')
    })
})
