import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync } from 'svelte'
import { get } from 'svelte/store'

vi.mock('../parser/parser.svelte', () => ({
    assetRegex: /$^/,
    hasher: vi.fn(async () => 'hash'),
    parseMarkdownSafe: (value: string) => value,
    ParseMarkdown: vi.fn(async (value: string) => value),
    risuChatParser: (value: string) => value,
}))
vi.mock('../process/modules', () => ({ moduleUpdate: vi.fn() }))
vi.mock('../process/scripts', () => ({ resetScriptCache: vi.fn() }))

import { characterFormatUpdate } from '../characters'
import { selectedCharID, selIdState } from '../stores.svelte'
import type { Chat, Database, Message, character } from './database.svelte'
import { getDatabase, setDatabaseLite } from './database.svelte'
import { IndexedDbPersistentDataStore } from './indexedDbPersistentDataStore'
import { createPersistentDataRuntime } from './persistentDataRuntime'
import { createProductionStateAdapter } from './persistentDataRuntime.svelte'
import { workingSetResidency } from './workingSetResidency'
import { observePersistentSaveChanges } from './persistentSaveObserver.svelte'

afterEach(() => {
    workingSetResidency.clear()
    workingSetResidency.setEvictionAllowed(true)
    selectedCharID.set(-1)
})

function makeLargeLegacyDatabase(): {
    database: Database
    messages: Message[]
} {
    const messages = Array.from({ length: 10_000 }, (_, index) => ({
        role: index % 2 === 0 ? 'user' : 'char',
        data: `synthetic-message-${index}`,
        chatId: `synthetic-id-${index}`,
        time: 1_800_000_000_000 + index,
    })) as Message[]
    const conversation = {
        id: 'legacy-chat',
        name: 'Legacy metadata',
        note: 'Original note',
        message: messages,
        scriptstate: { arbitrary: ['metadata', 7] },
        lastDate: 1_800_000_010_000,
    } as unknown as Chat
    const legacyCharacter = {
        type: 'character',
        chaId: 'legacy-character',
        name: 'Legacy character',
        image: '',
        firstMessage: 'Hello',
        desc: 'Synthetic legacy fixture',
        notes: '',
        chats: [conversation],
        chatFolders: [],
        chatPage: 0,
        viewScreen: 'none',
        bias: [],
        emotionImages: [],
        globalLore: [
            {
                key: 'legacy lore',
                content: '<char> @@@end',
                activationPercent: 25,
            },
        ],
        postHistoryInstructions: 'Migrated instruction',
        replaceGlobalNote: '',
        firstMsgIndex: 7,
        lastInteraction: 1,
        newGenData: {},
        syntheticCharacterField: { preserved: true },
    } as unknown as character
    return {
        database: {
            username: 'Synthetic user',
            botPresets: [],
            plugins: [],
            characters: [legacyCharacter],
        } as unknown as Database,
        messages,
    }
}

function syntheticCharacter(id: string, history: Message[]): character {
    return {
        type: 'character',
        chaId: id,
        name: `Synthetic ${id}`,
        image: '',
        firstMessage: '',
        desc: 'Synthetic description',
        notes: '',
        chats: [{ id: `${id}-chat`, name: 'Synthetic chat', note: '', localLore: [], message: history }],
        chatFolders: [],
        chatPage: 0,
        viewScreen: 'none',
        bias: [],
        emotionImages: [],
        globalLore: [],
    } as unknown as character
}

function syntheticLibrary(type: 'character' | 'group'): Database {
    const history: Message[] = [{ role: 'char', data: 'Synthetic existing history' }]
    const characters: Database['characters'] = [syntheticCharacter('second-character', structuredClone(history))]
    if (type === 'group') {
        const member = syntheticCharacter('group-member', [])
        characters.unshift({
            ...syntheticCharacter('edited-owner', history.map((message) => ({ ...message, saying: member.chaId }))),
            type: 'group',
            characters: [member.chaId],
            characterTalks: [1],
            characterActive: [true],
        } as unknown as Database['characters'][number], member)
    } else {
        characters.unshift(syntheticCharacter('edited-owner', history))
    }
    return { streamingDisplayOptimizationMode: 'balanced', characters } as unknown as Database
}

// A fresh module graph gives each boot its own production runtime, stores and coordinator.
async function bootProductionApp(indexedDB: IDBFactory, seed?: Database) {
    vi.resetModules()
    Object.assign(globalThis, { indexedDB, IDBKeyRange })
    const svelte = await import('svelte')
    const stores = await import('../stores.svelte')
    const databaseModule = await import('./database.svelte')
    const runtimeModule = await import('./persistentDataRuntime.svelte')
    const factory = await import('./persistentDataStoreFactory')
    const { bootstrapPersistentDatabase } = await import('./persistentBootstrap')
    const preparation = await import('./databasePreparation')
    const catalog = await import('./workingSetCatalog')
    const { workingSetResidency: residency } = await import('./workingSetResidency')
    const characters = await import('../characters')
    const globalApi = await import('../globalApi.svelte')
    const generation = await import('../process/generationState')
    const { createSelectedConversationOperations } = await import('../selectedConversationOperations')
    const { saveCapturedChatMessage } = await import('../chatMessageUi')
    const mutations = await import('../conversationMutations')
    const { appendDefaultChatInput } = await import('../../lib/ChatScreens/defaultChatInput')
    const { default: Harness } = await import('../../lib/SideBars/BoundCharacterEditorHarness.test.svelte')

    if (seed) {
        const raw = factory.getRawPersistentDataStore()
        await raw.open()
        const { database } = await preparation.prepareDatabaseForBootstrap(seed)
        await raw.replaceFromDatabase(database, 0)
    }
    // Mirrors bootstrap's local-only persistent working-set installation.
    const runtime = runtimeModule.getPersistentDataRuntime()
    const local = await bootstrapPersistentDatabase({
        store: runtime.store,
        prepareDatabase: preparation.prepareDatabaseForBootstrap,
        prepareRoot: preparation.preparePersistentRootForWorkingSet,
        projectScalableWorkingSet: (input) => catalog.projectCatalogWorkingSet(
            input.root,
            input.characters,
            catalog.createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset),
        ),
    })
    runtimeModule.configurePersistentDataRuntime({
        projectWorkingSet(database, selectedCharacterId, selectedConversationId, activeCharacterIds, forceScalableProjection) {
            if (forceScalableProjection === false) return database
            const projected = catalog.isCatalogPresetWorkingSet(database.botPresets)
                ? database
                : catalog.projectCompleteScalableWorkingSet(
                    database, selectedCharacterId, runtime.revision, activeCharacterIds, selectedConversationId,
                )
            for (const character of projected.characters) {
                if (catalog.isWorkingSetCharacterStub(character)) residency.markCharacterReleased(character.chaId)
                else residency.reconcileConversationResidency(character)
            }
            return projected
        },
    })
    residency.clear()
    for (const character of local.database.characters) {
        if (catalog.isWorkingSetCharacterStub(character)) residency.markCharacterReleased(character.chaId)
    }
    databaseModule.setDatabase(local.database)
    await runtimeModule.initializeActiveWorkingSet(databaseModule.getDatabase())
    stores.selectedCharID.set(-1)
    await globalApi.saveDb()
    return {
        svelte, stores, runtime, characters, generation, mutations, residency, Harness,
        getDatabase: databaseModule.getDatabase,
        appendDefaultChatInput, saveCapturedChatMessage, createSelectedConversationOperations,
    }
}

type ProductionApp = Awaited<ReturnType<typeof bootProductionApp>>

function settleWithin<T>(promise: Promise<T>, label: string, trace: unknown[]): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined
    return Promise.race([
        promise,
        new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(new Error(`${label} did not settle: ${JSON.stringify(trace)}`)), 5000)
        }),
    ]).finally(() => clearTimeout(timer))
}

function selectedConversationOperations(app: ProductionApp) {
    const { runtime, stores } = app
    return app.createSelectedConversationOperations({
        captureSelectedConversationTarget: () => runtime.captureSelectedConversationTarget(),
        acquireCompleteConversation: (reason, target) => runtime.acquireCompleteConversation(reason, target),
        captureCurrent: () => {
            const character = stores.DBState.db.characters[get(stores.selectedCharID)]
            const conversation = character?.chats[character.chatPage]
            return character && conversation ? { character, conversation } : null
        },
        getCurrentSession: () => runtime.getActiveConversationSession(),
        getCurrentViewportSource: () => runtime.getActiveConversationViewportSource(),
    })
}

// DefaultChatScreen's send path up to the provider. The provider stand-in returns an
// ordinary failed result, so no response is applied or acknowledged.
async function sendUserTurnWithFailedProvider(app: ProductionApp, input: string): Promise<boolean | null> {
    const { runtime, stores, generation, mutations } = app
    if (get(generation.doingChat)) return null
    return selectedConversationOperations(app).withCompleteSelectedConversation('send-message', async (context) => {
        const requireTarget = () => {
            const authority = context.requireCurrent()
            return mutations.captureConversationMutationTarget(
                authority.character, authority.conversation, authority.session,
            )
        }
        const target = requireTarget()
        const createMessage = (data: string): Message => ({ role: 'user', data, time: Date.now(), name: null })
        if (target.character.type === 'character') {
            const appended = await app.appendDefaultChatInput({
                target,
                recaptureTarget: requireTarget,
                runInputTrigger: async () => null,
                processInput: async () => input,
                isTargetCurrent: (candidate) => {
                    context.requireCurrent()
                    const character = stores.DBState.db.characters[get(stores.selectedCharID)]
                    return mutations.isConversationMutationTargetCurrent(
                        candidate, character, character?.chats[character.chatPage], runtime.getActiveConversationSession(),
                    )
                },
                createMessage,
            })
            context.requireCurrent()
            if (!appended) return false
        } else {
            mutations.appendConversationMessage(target, createMessage(input))
        }
        await new Promise((resolve) => setTimeout(resolve, 10))
        context.requireCurrent()
        const reservation = generation.reserveGeneration()
        if (!reservation) return false
        let lease: Awaited<ReturnType<typeof runtime.acquireCompleteConversation>> | null = null
        try {
            const selection = runtime.captureSelectedConversationTarget()
            if (selection) lease = await runtime.acquireCompleteConversation('generation', selection)
            await Promise.resolve()
            return false
        } finally {
            lease?.release()
            reservation.release({ preserveBusy: false })
            generation.doingChat.set(false)
        }
    })
}

describe('windowed navigation integration', () => {
    it.each([
        ['character', 'immediate'],
        ['character', 'autosave'],
        ['character', 'failed-request'],
        ['character', 'failed-request-closed-editor'],
        ['group', 'immediate'],
        ['group', 'autosave'],
        ['group', 'failed-request'],
        ['group', 'failed-request-closed-editor'],
    ] as const)('keeps %s edits from the bound editor through %s production navigation and reboot', async (type, timing) => {
        const indexedDB = new IDBFactory()
        const previousGlobals = { indexedDB: globalThis.indexedDB, IDBKeyRange: globalThis.IDBKeyRange }
        const ownerId = 'edited-owner'
        const conversationId = `${ownerId}-chat`
        const typedName = '수정 🙂'
        const typedDescription = 'Synthetic edited description'
        const editedHistory = 'Synthetic edited history'
        const userTurn = 'Synthetic user turn'
        const trace: unknown[] = []
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {})
        const target = document.createElement('div')
        document.body.append(target)
        let editor: ReturnType<ProductionApp['svelte']['mount']> | null = null
        let app: ProductionApp | null = null
        const reader = new IndexedDbPersistentDataStore('risuai-persistent-data', indexedDB, IDBKeyRange)
        try {
            app = await bootProductionApp(indexedDB, syntheticLibrary(type))
            const { svelte, stores, runtime, characters } = app
            const commit = runtime.store.commit.bind(runtime.store)
            vi.spyOn(runtime.store, 'commit').mockImplementation(async (request) => {
                const entry: Record<string, unknown> = {
                    stage: 'commit',
                    expected: request.expectedRevision,
                    keys: Object.keys(request).filter((key) => key !== 'expectedRevision').sort(),
                }
                trace.push(entry)
                try {
                    const result = await commit(request)
                    entry.revision = result.revision
                    return result
                } catch (error) {
                    entry.error = error instanceof Error ? error.name : typeof error
                    throw error
                }
            })
            let dirtyMarks = 0
            const markDirty = runtime.markPersistentDataDirty.bind(runtime)
            vi.spyOn(runtime, 'markPersistentDataDirty').mockImplementation((bytes) => {
                dirtyMarks++
                markDirty(bytes)
            })
            const activate = runtime.activateCharacter.bind(runtime)
            vi.spyOn(runtime, 'activateCharacter').mockImplementation(async (id, options) => {
                const result = await activate(id, options)
                trace.push({ stage: 'activate', id, result, revision: runtime.revision, dirtyMarks })
                return result
            })
            const navigate = (id: string) => settleWithin(
                characters.changeChar(stores.DBState.db.characters.findIndex((candidate) => candidate.chaId === id)),
                `changeChar(${id})`,
                trace,
            )
            const selected = () => stores.DBState.db.characters[get(stores.selectedCharID)]
            const readEditor = () => ({
                name: target.querySelector('input'),
                description: target.querySelector('textarea'),
            })

            editor = svelte.mount(app.Harness, { target })
            svelte.flushSync()
            expect(stores.DBState.db).toBe(app.getDatabase())
            expect(await navigate(ownerId), JSON.stringify(trace)).toBe(true)
            const inputs = await vi.waitFor(() => {
                const found = readEditor()
                expect(found.name).not.toBeNull()
                return found
            })
            const beforeEdit = dirtyMarks
            inputs.name!.value = typedName
            inputs.name!.dispatchEvent(new Event('input', { bubbles: true }))
            if (inputs.description) {
                inputs.description.value = typedDescription
                inputs.description.dispatchEvent(new Event('input', { bubbles: true }))
            }
            await svelte.tick()
            trace.push({
                stage: 'edited',
                selected: selected()?.chaId,
                nameEqual: selected()?.name === typedName,
                selectedTargetIsOwner: runtime.captureSelectedConversationTarget()?.characterId === ownerId,
                dirtyMarksSinceEdit: dirtyMarks - beforeEdit,
            })
            expect(selected()?.name).toBe(typedName)
            expect(dirtyMarks).toBeGreaterThan(beforeEdit)

            const operations = selectedConversationOperations(app)
            const acquired = await operations.acquireCompleteMessageTarget(0, 'edit-message')
            expect(acquired).not.toBeNull()
            try {
                const context = {
                    captureCurrent: () => {
                        const character = selected()
                        const conversation = character?.chats[character.chatPage]
                        return character && conversation ? { character, conversation } : null
                    },
                    getCurrentSession: () => runtime.getActiveConversationSession(),
                }
                expect(app.saveCapturedChatMessage(acquired!.target, context, editedHistory).saved).toBe(true)
            } finally {
                acquired!.release()
            }

            if (timing === 'failed-request-closed-editor') {
                await svelte.unmount(editor)
                editor = null
            }
            if (timing.startsWith('failed-request')) {
                expect(await settleWithin(sendUserTurnWithFailedProvider(app, userTurn), 'send', trace)).toBe(false)
                expect(get(app.generation.doingChat)).toBe(false)
                const session = runtime.getActiveConversationSession()
                trace.push({
                    stage: 'request-failed',
                    mode: runtime.getSelectedConversationMode(),
                    messages: session?.totalMessages,
                    version: session?.version,
                    persistedVersion: session?.persistedVersion,
                })
            }
            await reader.open()
            if (timing === 'autosave') {
                await vi.waitFor(async () => {
                    expect((await reader.readCharacter(ownerId))?.value.name).toBe(typedName)
                    expect((await reader.readConversation(ownerId, conversationId))?.value.message[0].data)
                        .toBe(editedHistory)
                }, { timeout: 3000 })
            }

            expect(await navigate('second-character'), JSON.stringify(trace)).toBe(true)
            expect(selected()?.chaId).toBe('second-character')
            expect(await navigate(ownerId), JSON.stringify(trace)).toBe(true)
            expect(selected()?.name).toBe(typedName)
            if (editor) await vi.waitFor(() => expect(readEditor().name?.value).toBe(typedName))
            const expectedMessages = [
                editedHistory,
                ...(timing.startsWith('failed-request') ? [userTurn] : []),
            ]
            if (editor) {
                const session = runtime.getActiveConversationSession()
                expect(session?.readRange(0, 8).messages.map((message) => message.data)).toEqual(expectedMessages)
            }
            // changeChar retries once after an observer invalidation; a second refusal is a lost click.
            const activations = trace.filter((entry): entry is { stage: 'activate', id: string, result: boolean } =>
                (entry as { stage?: string }).stage === 'activate')
            activations.forEach((entry, index) => {
                if (!entry.result) {
                    expect(activations[index + 1], JSON.stringify(trace)).toMatchObject({ id: entry.id, result: true })
                }
            })
            expect(consoleError.mock.calls.map((args) => args.map((value) =>
                value instanceof Error ? `${value.name}: ${value.message}` : String(value).slice(0, 200))),
            JSON.stringify(trace)).toEqual([])

            const persistedCharacter = await reader.readCharacter(ownerId)
            expect(persistedCharacter?.value.name).toBe(typedName)
            if (type === 'character') expect(persistedCharacter?.value).toMatchObject({ desc: typedDescription })
            expect((await reader.readConversation(ownerId, conversationId))?.value.message.map((message) => message.data))
                .toEqual(expectedMessages)
            expect((await reader.readRoot()).revision).toBe(runtime.revision)
            if (editor) await svelte.unmount(editor)
            editor = null

            // Module reset cannot stop this instance's debounce timer as a process exit would,
            // so run the production exit drain before another instance opens the same store.
            await runtime.flushPendingData('exit')
            expect((await reader.readRoot()).revision).toBe(runtime.revision)
            const rebooted = await bootProductionApp(indexedDB)
            app = rebooted
            expect(await settleWithin(rebooted.characters.changeChar(
                rebooted.stores.DBState.db.characters.findIndex((candidate) => candidate.chaId === ownerId),
            ), 'reboot changeChar', trace)).toBe(true)
            const reopened = rebooted.stores.DBState.db.characters[get(rebooted.stores.selectedCharID)]
            expect(reopened?.name).toBe(typedName)
            const lease = await rebooted.runtime.acquireCompleteConversation('reboot-check')
            try {
                expect(lease.session.readRange(0, 8).messages.map((message) => message.data)).toEqual(expectedMessages)
            } finally {
                lease.release()
            }
            await rebooted.runtime.flushPendingData('exit')
            expect(consoleError.mock.calls.length, JSON.stringify(trace)).toBe(0)
        } finally {
            if (editor && app) await app.svelte.unmount(editor)
            target.remove()
            consoleError.mockRestore()
            Object.assign(globalThis, previousGlobals)
        }
    })

    it.each(['immediate', 'autosave'])('preserves cold-selection editor changes and a user-only turn through %s navigation', async (timing) => {
        const indexedDB = new IDBFactory()
        const databaseName = `cold-selection-${timing}`
        const store = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
        await store.open()
        const { database } = makeLargeLegacyDatabase()
        const first = database.characters[0] as character
        first.chats[0].message = [{ role: 'char', data: 'Synthetic existing history' }]
        const second = structuredClone(first)
        second.chaId = 'second-character'
        second.chats[0].id = 'second-chat'
        database.characters.push(second)
        await store.replaceFromDatabase(database)
        setDatabaseLite(structuredClone(database))
        selectedCharID.set(-1)
        const state = createProductionStateAdapter()
        expect(state.canonicalCapture!.character()).toBeNull()
        const runtime = createPersistentDataRuntime({ store, state, prepareDatabase: async (value) => value })
        await runtime.initializeActiveWorkingSet(getDatabase())
        const navigate = async (id: string) => {
            const expectedGeneration = runtime.getNavigationGeneration() + 1
            if (await runtime.activateCharacter(id)) return true
            if (runtime.getNavigationGeneration() !== expectedGeneration) return false
            // Match changeChar's single retry after observer invalidation.
            return runtime.activateCharacter(id)
        }
        const dispose = observePersistentSaveChanges({
            readDatabase: getDatabase,
            readSelectedCharacter: () => getDatabase().characters[selIdState.selId] ?? null,
            markDirty: (bytes) => runtime.markPersistentDataDirty(bytes),
        })
        try {
            flushSync()
            expect(await navigate(first.chaId)).toBe(true)
            const lease = await runtime.acquireCompleteConversation('synthetic-bound-editor')
            try {
                const live = getDatabase().characters[selIdState.selId] as character
                live.name = '수정 🙂'
                live.desc = 'Synthetic edited description'
                live.chats[0].note = 'Synthetic edited note'
                lease.session.append({ role: 'user', data: 'Synthetic user-only turn' })
                flushSync()
                if (timing === 'autosave') {
                    await vi.waitFor(async () => {
                        expect((await store.readCharacter(first.chaId))?.value.name).toBe('수정 🙂')
                        expect((await store.readConversation(first.chaId, 'legacy-chat'))?.value.message).toHaveLength(2)
                    }, { timeout: 3000 })
                }
            } finally {
                lease.release()
            }
            expect(await navigate(second.chaId)).toBe(true)
            expect(await navigate(first.chaId)).toBe(true)
            expect(getDatabase().characters[selIdState.selId].name).toBe('수정 🙂')
            const reopened = new IndexedDbPersistentDataStore(databaseName, indexedDB, IDBKeyRange)
            await reopened.open()
            expect((await reopened.readCharacter(first.chaId))?.value).toMatchObject({
                name: '수정 🙂', desc: 'Synthetic edited description',
            })
            expect((await reopened.readConversation(first.chaId, 'legacy-chat'))?.value).toMatchObject({
                note: 'Synthetic edited note',
                message: [
                    { role: 'char', data: 'Synthetic existing history' },
                    { role: 'user', data: 'Synthetic user-only turn' },
                ],
            })
        } finally {
            dispose()
        }
    })

    it('normalizes through the production Svelte adapter and survives immediate promotion and restart', async () => {
        const indexedDB = new IDBFactory()
        const databaseName = 'windowed-navigation-normalize-promote-restart'
        const store = new IndexedDbPersistentDataStore(
            databaseName,
            indexedDB,
            IDBKeyRange,
        )
        await store.open()
        const { database, messages } = makeLargeLegacyDatabase()
        const imported = await store.replaceFromDatabase(database)

        setDatabaseLite(structuredClone(database))
        selectedCharID.set(0)
        workingSetResidency.setEvictionAllowed(false)
        const runtime = createPersistentDataRuntime({
            store,
            state: createProductionStateAdapter(),
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(getDatabase())
        workingSetResidency.setEvictionAllowed(true)

        const readConversation = vi.spyOn(store, 'readConversation')
        expect(
            await runtime.activateCharacter('legacy-character', {
                normalize: (candidate) =>
                    characterFormatUpdate(candidate, {
                        updateInteraction: true,
                    }),
            }),
        ).toBe(true)

        expect(runtime.getSelectedConversationMode()).toBe('windowed')
        expect(readConversation).not.toHaveBeenCalled()
        const windowedCharacter = getDatabase().characters[0] as character
        const windowedConversation = windowedCharacter.chats[0]
        expect(windowedCharacter).toMatchObject({
            chaId: 'legacy-character',
            postHistoryInstructions: null,
            firstMsgIndex: 7,
            syntheticCharacterField: { preserved: true },
        })
        expect(windowedCharacter.lastInteraction).toBeGreaterThan(1)
        expect(windowedCharacter.globalLore[0]).toMatchObject({
            bookVersion: 2,
            activationPercent: null,
            content: '@@probability 25\n{{char}} @@depth 0',
        })
        expect(windowedConversation).toMatchObject({
            id: 'legacy-chat',
            note: 'Original note\nMigrated instruction',
            fmIndex: 7,
            localLore: [],
            scriptstate: { arbitrary: ['metadata', 7] },
        })

        const target = runtime.captureSelectedConversationTarget()
        expect(target).not.toBeNull()
        const completeLease = await runtime.acquireCompleteConversation(
            'integration-immediate-promotion',
            target,
        )
        try {
            expect(runtime.getSelectedConversationMode()).toBe('complete')
            expect(readConversation).toHaveBeenCalledTimes(1)
            expect(completeLease.session.totalMessages).toBe(messages.length)
            expect(completeLease.session.readRange(0, 128).messages).toEqual(
                messages.slice(0, 128),
            )
            expect(completeLease.session.readLatest(128).messages).toEqual(
                messages.slice(-128),
            )

            const reopened = new IndexedDbPersistentDataStore(
                databaseName,
                indexedDB,
                IDBKeyRange,
            )
            await reopened.open()
            const detail = await reopened.readCharacter('legacy-character')
            const persisted = await reopened.readConversation(
                'legacy-character',
                'legacy-chat',
            )
            expect(detail).toMatchObject({
                revision: imported.revision + 1,
                value: {
                    postHistoryInstructions: null,
                    firstMsgIndex: 7,
                    syntheticCharacterField: { preserved: true },
                },
            })
            expect(persisted).toMatchObject({
                revision: imported.revision + 1,
                value: {
                    note: 'Original note\nMigrated instruction',
                    fmIndex: 7,
                    localLore: [],
                    scriptstate: { arbitrary: ['metadata', 7] },
                },
            })
            expect(persisted?.value.message).toEqual(messages)
            expect(
                persisted?.value.note.match(/Migrated instruction/g),
            ).toHaveLength(1)
        } finally {
            completeLease.release()
        }
    })
})
