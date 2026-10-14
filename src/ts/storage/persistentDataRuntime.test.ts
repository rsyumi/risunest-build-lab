import { runNativeDataHealthRepair } from './nativeDataHealthRepair'
import { notifyLocalPersistentRevision, subscribeLocalPersistentRevision } from './persistentRevisionEvents'
import { describe, expect, it, vi } from 'vitest'
import { createPersistentStoreHarness } from '../../../tests/support/persistentStoreHarness'
import type { Database } from './database.svelte'
import { capturePersistentPluginStorage, capturePersistentPresets, capturePersistentRoot, createPersistentDataRuntime, publishPersistentCharacterMutationToWorkingSet, publishPersistentConversationReplacementToWorkingSet, restoreStableWorkingSetSelection } from './persistentDataRuntime'
import type { PersistentDataStore } from './persistentDataStore'
import { RevisionConflictError } from './persistentDataStore'
import {
    createCatalogCharacterStub,
    createCatalogPresetWorkingSet,
    isCatalogCharacterStub,
    isCatalogPresetWorkingSet,
    projectCompleteScalableWorkingSet,
} from './workingSetCatalog'
import { WorkingSetResidencyRegistry } from './workingSetResidency'
import {
    createConversationSummaryStubFromChat,
    isConversationSummaryStub,
} from './conversationResidency'

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((resolvePromise) => {
        resolve = resolvePromise
    })
    return { promise, resolve }
}

function makeDatabase(username: string): Database {
    return {
        username,
        botPresets: [],
        characters: [{
            type: 'character',
            chaId: 'char-a',
            name: 'Alpha',
            chats: [],
        }],
    } as unknown as Database
}

function makeConversationDatabase(username: string): Database {
    const database = makeDatabase(username)
    database.characters[0].chatPage = 0
    database.characters[0].chats = [{
        id: 'chat-a',
        name: 'Chat A',
        note: '',
        localLore: [],
        message: [{ role: 'user', data: `${username} message`, chatId: 'message-a' }],
    }]
    return database
}

async function createActiveSessionRuntimeHarness(
    prepareDatabase: (value: Database) => Promise<Database> = async (value) => value,
    initial = makeConversationDatabase('Initial'),
) {
    let database = initial
    const storage = await createPersistentStoreHarness(structuredClone(database))
    const runtime = createPersistentDataRuntime({
        store: storage.store,
        state: {
            captureRoot: () => capturePersistentRoot(database),
            capturePresets: () => database.botPresets,
            captureSelectedCharacter: () => database.characters[0] ?? null,
            captureCharacter: (id) =>
                database.characters.find((character) => character.chaId === id) ?? null,
            getSelectedCharacterId: () => database.characters[0]?.chaId,
            getSelectedConversationId: () => database.characters[0]?.chats[0]?.id,
            replaceDatabase: (replacement) => {
                database = structuredClone(replacement)
            },
            installCompleteDatabase: (replacement) => {
                database = structuredClone(replacement)
            },
            restoreSelection: vi.fn(),
            publishCharacter: vi.fn(),
            publishConversation: vi.fn(),
            publishConversationReplacement: (result) => {
                publishPersistentConversationReplacementToWorkingSet(database, result)
            },
        },
        prepareDatabase,
    })
    await runtime.initializeActiveWorkingSet(database)
    return {
        runtime,
        storage,
        get database() {
            return database
        },
    }
}

async function createFenceRuntimeHarness(
    initial: Database,
    restored: Database = initial,
    onLocalRevision: (revision: number) => void = (revision) => notifyLocalPersistentRevision(revision),
) {
    let database = structuredClone(initial)
    const storage = await createPersistentStoreHarness(structuredClone(initial))
    const runtime = createPersistentDataRuntime({
        store: storage.store,
        state: {
            captureRoot: () => capturePersistentRoot(database),
            capturePluginStorage: () => database.pluginCustomStorage ?? {},
            capturePresets: () => database.botPresets,
            captureSelectedCharacter: () => database.characters[0] ?? null,
            captureCharacter: (id) => database.characters.find(
                (character) => character.chaId === id,
            ) ?? null,
            getSelectedCharacterId: () => database.characters[0]?.chaId,
            getSelectedConversationId: () => database.characters[0]?.chats[0]?.id,
            replaceDatabase: (replacement) => {
                database = structuredClone(replacement)
            },
            publishCharacter: vi.fn(),
            publishConversation: vi.fn(),
        },
        prepareDatabase: async (value) => value,
        onLocalRevision,
    })
    return {
        runtime,
        storage,
        get database() { return database },
        set username(value: string) { database.username = value },
        /** Writes the restored database as the next revision, as a native restore does outside the runtime. */
        async commitRestored() {
            const { revision } = await storage.store.readRoot()
            return (await storage.store.replaceFromDatabase(structuredClone(restored), revision)).revision
        },
        /** The username and revision the store holds now. */
        async stored() {
            const { revision, value } = await storage.store.readRoot()
            return { revision, username: value.username }
        },
    }
}

describe('native data health repair coordination', () => {
    it.each(['repair', 'undo'])('refreshes the loaded root after %s and saves a later edit at the new revision', async () => {
        const repaired = makeConversationDatabase('Corrected root')
        const harness = await createFenceRuntimeHarness(makeConversationDatabase('Broken root'), repaired)
        await harness.runtime.initializeActiveWorkingSet(harness.database)
        const mutate = vi.fn(async (revision: number) => {
            expect(revision).toBe(1)
            expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow()
            return { revision: await harness.commitRestored() }
        })
        const notified = vi.fn()
        const unsubscribe = subscribeLocalPersistentRevision(notified)
        try {
            await runNativeDataHealthRepair(1, mutate, harness.runtime)
        } finally {
            unsubscribe()
        }
        expect(notified).toHaveBeenCalledExactlyOnceWith(2, 'edit')
        expect(harness.database.username).toBe('Corrected root')
        expect(harness.runtime.revision).toBe(2)
        harness.username = 'Later edit'
        harness.runtime.markPersistentDataDirty(1)
        await harness.runtime.flushPendingDataLocally('after-repair')
        expect(await harness.stored()).toEqual({ revision: 3, username: 'Later edit' })
    })

    it('flushes pending edits and rejects a diagnosis that became stale before native mutation', async () => {
        const harness = await createFenceRuntimeHarness(makeConversationDatabase('Initial'))
        await harness.runtime.initializeActiveWorkingSet(harness.database)
        harness.username = 'Pending edit'
        harness.runtime.markPersistentDataDirty(1)
        const mutate = vi.fn()
        await expect(runNativeDataHealthRepair(1, mutate, harness.runtime)).rejects.toBeInstanceOf(RevisionConflictError)
        expect(mutate).not.toHaveBeenCalled()
        expect(harness.runtime.revision).toBe(2)
        expect(() => harness.runtime.assertPersistentMutationAllowed()).not.toThrow()
    })

    it('keeps the committed repair read-only when projection fails', async () => {
        const harness = await createFenceRuntimeHarness(makeConversationDatabase('Initial'), makeConversationDatabase('Repaired'))
        await harness.runtime.initializeActiveWorkingSet(harness.database)
        harness.storage.failNext('acquireRevision', new Error('projection failed'))
        const notified = vi.fn()
        const unsubscribe = subscribeLocalPersistentRevision(notified)
        try {
            await expect(runNativeDataHealthRepair(1, async () => ({ revision: await harness.commitRestored() }), harness.runtime))
                .rejects.toMatchObject({ committedRevision: 2, code: 'activation-committed-refresh-failed' })
        } finally {
            unsubscribe()
        }
        expect(notified).toHaveBeenCalledExactlyOnceWith(2, 'edit')
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow()
        expect(harness.database.username).toBe('Initial')
    })

    it('does not replay a native repair whose journal or diagnosis failed after commit', async () => {
        const harness = await createFenceRuntimeHarness(makeConversationDatabase('Initial'), makeConversationDatabase('Repaired'))
        await harness.runtime.initializeActiveWorkingSet(harness.database)
        const mutate = vi.fn(async () => {
            const revision = await harness.commitRestored()
            throw { code: 'committed', revision, message: 'diagnosis failed' }
        })
        const notified = vi.fn()
        const unsubscribe = subscribeLocalPersistentRevision(notified)
        try {
            await expect(runNativeDataHealthRepair(1, mutate, harness.runtime))
                .rejects.toMatchObject({ committedRevision: 2, recoveryRequired: true })
        } finally {
            unsubscribe()
        }
        expect(notified).toHaveBeenCalledExactlyOnceWith(2, 'edit')
        expect(mutate).toHaveBeenCalledExactlyOnceWith(1)
        expect(harness.runtime.revision).toBe(2)
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow()
        expect(harness.database.username).toBe('Initial')
    })
})

describe('persistent preset capture', () => {
    it('returns null for a selected-only scalable preset working set', () => {
        const botPresets = createCatalogPresetWorkingSet(
            {
                revision: 3,
                items: [
                    { id: '0', configuredIndex: 0, name: 'Inactive' },
                    { id: '1', configuredIndex: 1, name: 'Active' },
                ],
            },
            {
                summary: { id: '1', configuredIndex: 1, name: 'Active' },
                value: { name: 'Active', mainPrompt: 'full body' } as Database['botPresets'][number],
            },
        )
        const database = { botPresets } as Database

        expect(capturePersistentPresets(database)).toBeNull()
    })

    it('returns the full array for maximum compatibility state', () => {
        const botPresets = [
            { name: 'First', mainPrompt: 'one' },
            { name: 'Second', mainPrompt: 'two' },
        ] as Database['botPresets']

        expect(capturePersistentPresets({ botPresets } as Database)).toBe(botPresets)
    })
})

describe('persistent plugin storage capture', () => {
    it('does not capture the empty compatibility object from a scalable working set', () => {
        const database = makeDatabase('Scalable')
        database.botPresets = createCatalogPresetWorkingSet(
            { revision: 3, items: [] },
            null,
        )
        database.pluginCustomStorage = {}

        expect(capturePersistentPluginStorage(database)).toBeNull()
    })

    it('captures all values from a maximum-compatibility working set', () => {
        const database = makeDatabase('Maximum')
        database.pluginCustomStorage = { memory: { entries: [1, 2, 3] } }

        expect(capturePersistentPluginStorage(database)).toBe(database.pluginCustomStorage)
    })
})

describe('received unit projection into the selected conversation', () => {
    async function selectedConversationReceiveHarness(receive: (persisted: Database) => string[]) {
        const database = makeConversationDatabase('Initial')
        const persisted = structuredClone(database)
        const storage = await createPersistentStoreHarness(structuredClone(persisted))
        // The IndexedDB store has no LWW receive, so the received change lands as one replacement revision.
        const store: PersistentDataStore = Object.assign(storage.store, {
            lwwStageReceive: async () => undefined,
            lwwApplyReceive: async () => {
                const affectedKeys = receive(persisted)
                let { revision } = await storage.store.readRoot()
                if (affectedKeys.length) revision = (await storage.store.replaceFromDatabase(structuredClone(persisted), revision)).revision
                return { revision, affectedKeys, heldKeys: [], deferredKeys: [] }
            },
            lwwFinishReceive: async () => undefined,
        })
        const runtime = createPersistentDataRuntime({
            store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePresets: () => database.botPresets,
                captureCharacters: () => database.characters,
                captureWorkingSetDatabase: () => database,
                captureSelectedCharacter: () => database.characters[0] ?? null,
                captureCharacter: (id) => database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => database.characters[0]?.chaId,
                getSelectedConversationId: () => database.characters[0]?.chats[0]?.id,
                replaceDatabase: vi.fn(),
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)
        const session = runtime.getActiveConversationSession()!
        const chat = database.characters[0].chats[0]
        expect(session.matchesConversation('char-a', chat)).toBe(true)
        const version = session.version
        const events: unknown[] = []
        session.subscribe((event) => { events.push(event) })
        const applyReceive = () => runtime.applyLwwReceive({ bindingAuthority: '1', requestId: 'selected-receive', changes: [], progress: { kind: 'server', cursor: '1' }, admittedTimeUpperMs: '100' })
        return { database, runtime, session, chat, version, events, applyReceive }
    }

    it('leaves the selected conversation alone for a receive that affected nothing', async () => {
        const { database, runtime, session, chat, version, events, applyReceive } = await selectedConversationReceiveHarness(() => [])
        const revision = runtime.revision
        await applyReceive()
        expect(runtime.revision).toBe(revision)
        expect(database.characters[0].chats[0]).toBe(chat)
        expect(runtime.getActiveConversationSession()).toBe(session)
        expect(session.version).toBe(version)
        expect(events).toEqual([])
    })

    it('signals the selected session for a received conversation field without replacing its object', async () => {
        const { database, runtime, session, chat, version, events, applyReceive } = await selectedConversationReceiveHarness((persisted) => {
            persisted.characters[0].chats[0].note = 'Remote note'
            return ['["conversation","char-a","chat-a","note"]']
        })
        await applyReceive()
        expect(database.characters[0].chats[0]).toBe(chat)
        expect(chat.note).toBe('Remote note')
        expect(runtime.getActiveConversationSession()).toBe(session)
        expect(session.version).toBe(version + 1)
        expect(events).toEqual([expect.objectContaining({ commands: ['update-metadata'] })])
    })

    it('republishes the selected conversation as a new object when its session refuses the received change', async () => {
        const { database, runtime, session, chat, version, events, applyReceive } = await selectedConversationReceiveHarness((persisted) => {
            persisted.characters[0].chats[0].message.push({ role: 'char', data: 'Remote reply', chatId: 'message-remote' })
            return ['["messages","char-a","chat-a"]']
        })
        // Resident segments track the message count, so an appended message cannot be adopted in place.
        expect(session.residencyFallbackActive).toBe(false)
        await applyReceive()
        const republished = database.characters[0].chats[0]
        expect(republished).not.toBe(chat)
        expect(republished.message.at(-1)).toEqual({ role: 'char', data: 'Remote reply', chatId: 'message-remote' })
        expect(session.version).toBe(version)
        expect(session.isActive).toBe(false)
        expect(events).toEqual([null])
        const current = runtime.getActiveConversationSession()!
        expect(current).not.toBe(session)
        expect(current.matchesConversation('char-a', republished)).toBe(true)
    })
})

describe('persistent conversation replacement publication', () => {
    it('forwards a lease-owned replacement refresh without changing selection', async () => {
        const harness = await createActiveSessionRuntimeHarness()
        const target = harness.runtime.captureSelectedConversationTarget()!
        const lease = await harness.runtime.acquireCompleteConversation(
            'plugin-full-object-setter',
            target,
        )
        const replacement = {
            ...structuredClone(harness.database.characters[0].chats[0]),
            note: 'published through runtime',
        }

        await expect(harness.runtime.replacePersistentConversation(
            target.characterId,
            target.conversationId,
            'plugin-chat-set',
            replacement,
            { expectedRevision: target.storeRevision },
        )).resolves.toBe(true)
        expect(lease.session.matchesConversation(target.characterId, replacement)).toBe(false)

        lease.release()
        expect(harness.runtime.refreshSelectedConversationAfterReplacement(
            lease.target,
            lease.session,
        )).toBe(true)
        expect(harness.runtime.getActiveConversationSession()?.matchesConversation(
            target.characterId,
            harness.database.characters[0].chats[0],
        )).toBe(true)
        expect(harness.runtime.captureSelectedConversationTarget()).toMatchObject({
            characterId: target.characterId,
            conversationId: target.conversationId,
        })
    })

    it('forwards inactive replacement options without changing active selection', async () => {
        const initial = makeConversationDatabase('Initial')
        initial.characters.push({
            type: 'character',
            chaId: 'char-b',
            name: 'Inactive',
            chatPage: 0,
            chats: [{
                id: 'chat-b',
                name: 'Inactive chat',
                note: '',
                localLore: [],
                message: [],
            }],
        } as any)
        const harness = await createActiveSessionRuntimeHarness(undefined, initial)
        const selectedCharacterId = harness.database.characters[0].chaId
        const selectedConversationId = harness.database.characters[0].chats[0].id
        expect(harness.runtime.captureSelectedConversationTarget()).toMatchObject({
            characterId: selectedCharacterId,
            conversationId: selectedConversationId,
        })
        const replacement = {
            ...structuredClone(harness.database.characters[1].chats[0]),
            name: 'Runtime replacement',
        }

        await expect(harness.runtime.replacePersistentConversation(
            'char-b',
            'chat-b',
            'plugin-chat-set',
            replacement,
            { expectedRevision: 1 },
        )).resolves.toBe(true)

        expect(harness.database.characters[0].chaId).toBe(selectedCharacterId)
        expect(harness.database.characters[0].chats[0].id).toBe(selectedConversationId)
        expect(harness.runtime.captureSelectedConversationTarget()).toMatchObject({
            characterId: selectedCharacterId,
            conversationId: selectedConversationId,
        })
        expect(harness.database.characters[1].chats[0]).toEqual(replacement)
        expect((await harness.storage.store.readConversation('char-b', 'chat-b'))?.value).toEqual(replacement)
        await expect(harness.runtime.replacePersistentConversation(
            'char-b',
            'chat-b',
            'plugin-chat-set',
            replacement,
            { expectedRevision: 1 },
        )).rejects.toBeInstanceOf(RevisionConflictError)
    })

    it('updates complete targets while keeping inactive catalog and summary targets bounded', () => {
        const selected = makeConversationDatabase('Selected').characters[0]
        selected.chaId = 'selected-char'
        selected.chats[0].id = 'selected-chat'
        const inactive = createCatalogCharacterStub({
            id: 'inactive-char',
            configuredIndex: 1,
            name: 'Inactive',
            recentAt: 0,
            trashed: false,
            conversationCount: 1,
            type: 'character',
        })
        const database = {
            username: 'Fixture',
            botPresets: [],
            characters: [selected, inactive],
        } as unknown as Database
        const replacement = {
            ...structuredClone(selected.chats[0]),
            name: 'Updated selected chat',
        }

        publishPersistentConversationReplacementToWorkingSet(database, {
            revision: 2,
            characterId: 'inactive-char',
            conversationId: 'inactive-chat',
            conversation: {
                id: 'inactive-chat',
                name: 'Updated inactive',
                note: '',
                localLore: [],
                message: [],
            },
        })
        expect(isCatalogCharacterStub(database.characters[1])).toBe(true)
        expect(database.characters[1].chats).toEqual([])

        publishPersistentConversationReplacementToWorkingSet(database, {
            revision: 3,
            characterId: 'selected-char',
            conversationId: 'selected-chat',
            conversation: replacement,
        })
        expect(database.characters[0].chats[0]).toEqual(replacement)

        database.characters[0].chats[0] = createConversationSummaryStubFromChat(
            'selected-char',
            replacement,
            0,
        )
        publishPersistentConversationReplacementToWorkingSet(database, {
            revision: 4,
            characterId: 'selected-char',
            conversationId: 'selected-chat',
            conversation: { ...replacement, name: 'Summary metadata' },
        })
        expect(isConversationSummaryStub(database.characters[0].chats[0])).toBe(true)
        expect(database.characters[0].chats[0].name).toBe('Summary metadata')
        expect(database.characters[0].chats[0].message).toEqual([])
        expect(database.characters[0].chaId).toBe('selected-char')
    })
})

describe('persistent character mutation publication', () => {

    it('keeps scalable add, released replace, and detail mutations bounded', () => {
        const residency = new WorkingSetResidencyRegistry()
        const database = makeDatabase('Catalog')
        database.characters = [createCatalogCharacterStub({
            id: 'char-a',
            name: 'Alpha',
            configuredIndex: 0,
            recentAt: 0,
            trashed: false,
            conversationCount: 2,
            type: 'character',
        })]
        const select = vi.fn()
        const complete = {
            type: 'character',
            chaId: 'char-a',
            name: 'Replaced',
            personality: 'full body',
            chats: [{ id: 'chat-a', message: [{ role: 'user', data: 'hidden' }] }],
        } as any

        publishPersistentCharacterMutationToWorkingSet(
            database,
            {
                revision: 2,
                root: capturePersistentRoot(database),
                characterId: 'char-a',
                kind: 'replace',
                character: complete,
            },
            residency,
            -1,
            select,
        )
        publishPersistentCharacterMutationToWorkingSet(
            database,
            {
                revision: 3,
                root: capturePersistentRoot(database),
                characterId: '§playground',
                kind: 'add',
                character: { ...complete, chaId: '§playground', name: 'Temporary' },
            },
            residency,
            -1,
            select,
        )
        publishPersistentCharacterMutationToWorkingSet(
            database,
            {
                revision: 4,
                root: capturePersistentRoot(database),
                characterId: '§playground',
                kind: 'detail',
                character: {
                    type: 'character',
                    chaId: '§playground',
                    name: 'Renamed temporary',
                    personality: 'must remain absent',
                } as any,
            },
            residency,
            -1,
            select,
        )

        expect(database.characters).toHaveLength(2)
        expect(database.characters.every(isCatalogCharacterStub)).toBe(true)
        expect(database.characters[0]).not.toHaveProperty('personality')
        expect(database.characters[1]).toMatchObject({
            chaId: '§playground',
            name: 'Renamed temporary',
            chats: [],
        })
        expect(database.characters[1]).not.toHaveProperty('personality')
        expect(residency.isCharacterReleased('char-a')).toBe(true)
        expect(residency.isCharacterReleased('§playground')).toBe(true)
        expect(select).not.toHaveBeenCalled()
    })

    it('keeps maximum-compatibility character additions complete', () => {
        const residency = new WorkingSetResidencyRegistry()
        residency.setEvictionAllowed(false)
        const database = makeDatabase('Maximum')
        const complete = {
            type: 'character',
            chaId: 'char-b',
            name: 'Complete',
            personality: 'resident body',
            chats: [{ id: 'chat-b', message: [{ role: 'user', data: 'resident' }] }],
        } as any

        publishPersistentCharacterMutationToWorkingSet(
            database,
            {
                revision: 2,
                root: capturePersistentRoot(database),
                characterId: 'char-b',
                kind: 'add',
                character: complete,
            },
            residency,
            -1,
            vi.fn(),
        )

        expect(database.characters[1]).toEqual(complete)
        expect(isCatalogCharacterStub(database.characters[1])).toBe(false)
        expect(residency.isCharacterReleased('char-b')).toBe(false)
    })
})

describe('stable working-set selection', () => {
    it('restores a reordered character and its selected conversation by stable ID', () => {
        const database = {
            botPresets: [],
            characters: [
                {
                    type: 'character',
                    chaId: 'char-b',
                    name: 'Beta',
                    chatPage: 0,
                    chats: [
                        { id: 'chat-b2', message: [] },
                        { id: 'chat-b1', message: [] },
                    ],
                },
                {
                    type: 'character',
                    chaId: 'char-a',
                    name: 'Alpha',
                    chats: [],
                },
            ],
        } as unknown as Database
        const selectCharacterIndex = vi.fn()

        restoreStableWorkingSetSelection(
            database,
            'char-b',
            'chat-b1',
            selectCharacterIndex,
        )

        expect(selectCharacterIndex).toHaveBeenCalledWith(0)
        expect(database.characters[0].chatPage).toBe(1)
    })

    it('clears selection when the stable character no longer exists', () => {
        const database = makeDatabase('Replacement')
        const selectCharacterIndex = vi.fn()

        restoreStableWorkingSetSelection(
            database,
            'missing-character',
            'missing-chat',
            selectCharacterIndex,
        )

        expect(selectCharacterIndex).toHaveBeenCalledWith(-1)
    })
})

describe('prepared persistent replacement', () => {
    it('starts detached preparation before the caller can change the replacement input', async () => {
        const ready = deferred<void>()
        const prepareDatabase = vi.fn(async (value: Database) => {
            const captured = structuredClone(value)
            await ready.promise
            return captured
        })
        const harness = await createActiveSessionRuntimeHarness(prepareDatabase)
        const input = makeConversationDatabase('Requested replacement')
        const captured = structuredClone(input)
        const replacing = harness.runtime.replacePersistentDatabase(input, 'capture-before-prepare')
        input.username = 'Changed after request'
        input.characters[0].chats[0].message[0].data = 'Changed nested input'
        ready.resolve()
        await replacing

        expect(prepareDatabase).toHaveBeenCalledOnce()
        expect(harness.runtime.revision).toBe(harness.storage.revision + 1)
        expect(await harness.storage.store.materializeDatabase(harness.runtime.revision)).toEqual({ ...captured, pluginCustomStorage: {} })
        expect(harness.database).toEqual(captured)
    })

    it('invalidates a resident session before publishing a cloned conversation', async () => {
        const harness = await createActiveSessionRuntimeHarness()
        const session = harness.runtime.getActiveConversationSession()!

        harness.runtime.invalidateActiveConversationSession()

        expect(session.isActive).toBe(false)
        expect(harness.runtime.getActiveConversationSession()).toBeNull()
        expect(() => session.append({ role: 'char', data: 'detached clone write' })).toThrow(
            /inactive/,
        )
    })

    it('invalidates the resident session after explicit database replacement', async () => {
        const harness = await createActiveSessionRuntimeHarness()
        const session = harness.runtime.getActiveConversationSession()!

        await harness.runtime.replacePersistentDatabase(
            makeConversationDatabase('Replacement'),
            'replace-active-session',
        )

        expect(session.isActive).toBe(false)
        expect(harness.runtime.getActiveConversationSession()).toBeNull()
        expect(() => session.append({ role: 'char', data: 'detached replacement write' })).toThrow(
            /inactive/,
        )
    })

    it('invalidates the resident session after scalable working-set projection', async () => {
        const harness = await createActiveSessionRuntimeHarness()
        const session = harness.runtime.getActiveConversationSession()!

        await expect(harness.runtime.releaseInactiveWorkingSet()).resolves.toBe(true)

        expect(session.isActive).toBe(false)
        expect(harness.runtime.getActiveConversationSession()).toBeNull()
        expect(() => session.append({ role: 'char', data: 'detached projection write' })).toThrow(
            /inactive/,
        )
    })

    it('rejects a catalog working set before database preparation', async () => {
        let database = makeDatabase('Initial')
        const prepareDatabase = vi.fn(async (value: Database) => value)
        const storage = await createPersistentStoreHarness(structuredClone(database))
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[0],
                captureCharacter: (id) =>
                    database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => database.characters[0]?.chaId,
                replaceDatabase: (replacement) => {
                    database = replacement
                },
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase,
        })
        await runtime.initializeActiveWorkingSet(database)
        const catalog = makeDatabase('Catalog')
        catalog.characters = [createCatalogCharacterStub({
            id: 'char-a',
            name: 'Alpha',
            configuredIndex: 0,
            recentAt: 0,
            trashed: false,
            conversationCount: 3,
            type: 'character',
        })]

        await expect(runtime.replacePersistentDatabase(catalog, 'unsafe-catalog'))
            .rejects.toThrow('incomplete persistent working set')

        expect(prepareDatabase).not.toHaveBeenCalled()
        expect((await storage.store.readRoot()).revision).toBe(storage.revision)
    })

    it('preserves newer local edits and rejects a stale prepared replacement', async () => {
        let database = makeDatabase('Initial')
        const preparation = deferred<Database>()
        const prepareDatabase = vi.fn(() => preparation.promise)
        const storage = await createPersistentStoreHarness(structuredClone(database))
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => {
                    const { characters: _characters, botPresets: _botPresets, ...root } = database
                    return root
                },
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[0],
                captureCharacter: (id) =>
                    database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => database.characters[0]?.chaId,
                replaceDatabase: (replacement) => {
                    database = structuredClone(replacement)
                },
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase,
        })
        await runtime.initializeActiveWorkingSet(database)
        const replacement = runtime.replacePersistentDatabase(
            makeDatabase('Replacement'),
            'plugin-profile-change',
        )
        await vi.waitFor(() => expect(prepareDatabase).toHaveBeenCalledOnce())

        database.username = 'Edited during preparation'
        runtime.markPersistentDataDirty(1)
        preparation.resolve(makeDatabase('Prepared replacement'))
        await expect(replacement).rejects.toBeInstanceOf(RevisionConflictError)

        expect(database.username).toBe('Edited during preparation')
        expect(runtime.revision).toBe(2)
        const stored = await storage.store.materializeDatabase(2)
        expect(stored.username).toBe('Edited during preparation')
        expect(stored.characters.map((character) => character.chaId)).toEqual(['char-a'])
    })

    it('returns a detached materialized snapshot without installing it', async () => {
        let database = makeDatabase('Initial')
        const snapshot = makeDatabase('Snapshot')
        const storage = await createPersistentStoreHarness(structuredClone(snapshot))
        // A store that hands out its own object is the case the runtime must detach from.
        storage.spy('materializeDatabase').mockResolvedValue(snapshot)
        const replaceDatabase = vi.fn((replacement: Database) => {
            database = replacement
        })
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[0],
                captureCharacter: (id) =>
                    database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => database.characters[0]?.chaId,
                replaceDatabase,
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)

        const materialized = await runtime.materializePersistentDatabaseSnapshot('dataset-export')

        expect(materialized).toEqual(snapshot)
        expect(materialized).not.toBe(snapshot)
        expect(replaceDatabase).not.toHaveBeenCalled()
    })

    it('reprojects an authoritative complete snapshot into the scalable working set', async () => {
        const complete = {
            username: 'Authoritative',
            botPresetsId: 1,
            botPresets: [
                { id: 'preset-inactive', name: 'Inactive', mainPrompt: 'inactive body' },
                { id: 'preset-active', name: 'Active', mainPrompt: 'active body' },
            ],
            characters: [
                {
                    type: 'character',
                    chaId: 'char-a',
                    name: 'Alpha',
                    personality: 'inactive body',
                    chats: [{ id: 'chat-a', message: [{ role: 'user', data: 'hidden' }] }],
                },
                {
                    type: 'character',
                    chaId: 'char-b',
                    name: 'Beta',
                    personality: 'active body',
                    chatPage: 0,
                    chats: [{ id: 'chat-b', message: [{ role: 'user', data: 'visible' }] }],
                },
            ],
        } as unknown as Database
        let database = structuredClone(complete)
        let projectedActiveIds: string[] = []
        const storage = await createPersistentStoreHarness(structuredClone(complete))
        const leases = storage.leases()
        const materializeDatabase = storage.spy('materializeDatabase')
        const restoreSelection = vi.fn()
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[1],
                captureCharacter: (id) =>
                    database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => 'char-b',
                getSelectedConversationId: () => 'chat-b',
                replaceDatabase: (replacement, activeCharacterIds, forceScalableProjection) => {
                    expect(forceScalableProjection).toBe(true)
                    projectedActiveIds = [...(activeCharacterIds ?? [])]
                    database = replacement
                },
                restoreSelection,
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)

        await runtime.releaseInactiveWorkingSet()

        expect(materializeDatabase).not.toHaveBeenCalled()
        expect(leases.map((lease) => lease.revision)).toEqual([storage.revision])
        expect(database.username).toBe('Authoritative')
        expect(isCatalogCharacterStub(database.characters[0])).toBe(true)
        expect(database.characters[0]).not.toHaveProperty('personality')
        expect(isCatalogCharacterStub(database.characters[1])).toBe(false)
        expect(database.characters[1].personality).toBe('active body')
        expect(isCatalogPresetWorkingSet(database.botPresets)).toBe(true)
        expect(database.botPresets[0]).not.toHaveProperty('mainPrompt')
        expect(database.botPresets[1].mainPrompt).toBe('active body')
        expect(projectedActiveIds).toEqual(['char-b'])
        expect(restoreSelection).toHaveBeenCalledWith('char-b', 'chat-b')
    })

    it('does not publish a scalable snapshot when the final release guard becomes false', async () => {
        const database = makeDatabase('Initial')
        const storage = await createPersistentStoreHarness(makeDatabase('Snapshot'))
        const replaceDatabase = vi.fn()
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[0],
                captureCharacter: (id) =>
                    database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => database.characters[0]?.chaId,
                replaceDatabase,
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)

        let allowed = true
        const materialization = storage.gate('acquireRevision')
        const release = runtime.releaseInactiveWorkingSet(() => allowed)
        await materialization.entered
        allowed = false
        materialization.release()

        await expect(release).resolves.toBe(false)
        expect(replaceDatabase).not.toHaveBeenCalled()
    })

    it('rechecks transition currency synchronously after an async release guard', async () => {
        const database = makeDatabase('Initial')
        const releasePermission = deferred<boolean>()
        let transitionCurrent = true
        const replaceDatabase = vi.fn()
        const storage = await createPersistentStoreHarness(makeDatabase('Snapshot'))
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[0],
                captureCharacter: (id) =>
                    database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => database.characters[0]?.chaId,
                replaceDatabase,
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)

        const release = runtime.releaseInactiveWorkingSet(
            () => releasePermission.promise,
            () => transitionCurrent,
        )
        transitionCurrent = false
        releasePermission.resolve(true)

        await expect(release).resolves.toBe(false)
        expect(replaceDatabase).not.toHaveBeenCalled()
    })

    it('blocks a stale UI edit and persists fresh edits after bounded replacement publication', async () => {
        const complete = {
            username: 'Initial',
            customBackground: '',
            botPresetsId: 0,
            botPresets: [{ id: 'preset-active', name: 'Active', mainPrompt: 'body' }],
            pluginCustomStorage: { cached: true },
            characters: [{
                type: 'character',
                chaId: 'member-a',
                name: 'Member',
                personality: 'member detail',
                chats: [],
            }, {
                type: 'character',
                chaId: 'selected-a',
                name: 'Selected',
                chats: [],
            }, {
                type: 'character',
                chaId: 'inactive',
                name: 'Inactive',
                personality: 'release me',
                chats: [],
            }],
        } as unknown as Database
        let database = structuredClone(complete)
        const storage = await createPersistentStoreHarness(structuredClone(complete))
        const replacementWrite = storage.gate('replaceFromDatabase')
        const acquireRevision = storage.spy('acquireRevision')
        const materializeDatabase = storage.spy('materializeDatabase')
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePluginStorage: () => database.pluginCustomStorage,
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters.find(
                    (character) => character.chaId === 'selected-a',
                ) ?? null,
                captureCharacter: (id) => database.characters.find(
                    (character) => character.chaId === id,
                ) ?? null,
                getSelectedCharacterId: () => 'selected-a',
                replaceDatabase: (replacement, activeCharacterIds) => {
                    database = projectCompleteScalableWorkingSet(
                        replacement,
                        'selected-a',
                        8,
                        activeCharacterIds,
                    )
                },
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)
        const replacement = structuredClone(complete)
        replacement.username = 'Replacement'

        const replacing = runtime.replacePersistentDatabase(replacement, 'delayed-replacement')
        await replacementWrite.entered
        expect(() => {
            runtime.assertPersistentMutationAllowed()
            database.customBackground = 'blocked edit during replacement'
        }).toThrow(/replacement is active/i)
        replacementWrite.release()
        await replacing

        expect(database.username).toBe('Replacement')
        expect(database.customBackground).toBe('')
        expect(database.pluginCustomStorage).toEqual({})
        expect(isCatalogCharacterStub(database.characters[0])).toBe(true)
        expect(isCatalogCharacterStub(database.characters[2])).toBe(true)
        expect(acquireRevision).not.toHaveBeenCalled()
        expect(materializeDatabase).not.toHaveBeenCalled()

        const replaced = storage.revision + 1
        expect(runtime.revision).toBe(replaced)
        database.customBackground = 'live edit during replacement'
        runtime.markPersistentDataDirty(1)
        await runtime.flushPendingData('persist-new-live-edit')
        const stored = await storage.store.readRoot()
        expect(stored.revision).toBe(replaced + 1)
        expect(stored.value).toMatchObject({ username: 'Replacement', customBackground: 'live edit during replacement' })
    })

    it('cannot split committed replacement publication on revision read or release failure', async () => {
        const complete = {
            username: 'Initial',
            botPresetsId: 0,
            botPresets: [{ id: 'preset-active', name: 'Active', mainPrompt: 'body' }],
            pluginCustomStorage: { cached: true },
            characters: [{
                type: 'character',
                chaId: 'char-a',
                name: 'Selected',
                personality: 'selected detail',
                chats: [],
            }, {
                type: 'character',
                chaId: 'inactive',
                name: 'Inactive',
                personality: 'release me',
                chats: [],
            }],
        } as unknown as Database
        let database = structuredClone(complete)
        const storage = await createPersistentStoreHarness(structuredClone(complete))
        const acquireRevision = storage.spy('acquireRevision').mockRejectedValue(new Error('revision reads are unavailable'))
        const materializeDatabase = storage.spy('materializeDatabase').mockRejectedValue(new Error('full materialization is unavailable'))
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePluginStorage: () => database.pluginCustomStorage,
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[0],
                captureCharacter: (id) => database.characters.find(
                    (character) => character.chaId === id,
                ) ?? null,
                getSelectedCharacterId: () => 'char-a',
                replaceDatabase: (replacement, activeCharacterIds) => {
                    database = projectCompleteScalableWorkingSet(
                        replacement,
                        'char-a',
                        8,
                        activeCharacterIds,
                    )
                },
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)
        const replacement = structuredClone(complete)
        replacement.username = 'Committed replacement'

        await runtime.replacePersistentDatabase(replacement, 'nonthrowing-publication')

        expect(runtime.revision).toBe(storage.revision + 1)
        expect((await storage.store.readRoot()).value.username).toBe('Committed replacement')
        expect(database.username).toBe('Committed replacement')
        expect(database.pluginCustomStorage).toEqual({})
        expect(database.characters[0].personality).toBe('selected detail')
        expect(isCatalogCharacterStub(database.characters[1])).toBe(true)
        expect(acquireRevision).not.toHaveBeenCalled()
        expect(materializeDatabase).not.toHaveBeenCalled()
    })
})

describe('native replacement working-set refresh', () => {
    it('rebuilds only the scalable selected working set from the committed revision', async () => {
        let database = makeConversationDatabase('Before native restore')
        const restored = makeConversationDatabase('After native restore')
        restored.characters[0].chats[0].message[0].data = 'restored selected message'
        restored.characters.push({
            type: 'character',
            chaId: 'char-b',
            name: 'Inactive',
            personality: 'must stay outside the JS working set',
            chats: [{
                id: 'chat-b',
                name: 'Inactive chat',
                note: '',
                localLore: [],
                message: [{ role: 'user', data: 'inactive payload', chatId: 'message-b' }],
            }],
        } as Database['characters'][number])
        const storage = await createPersistentStoreHarness(structuredClone(database))
        const materializeDatabase = storage.spy('materializeDatabase')
        const runtime = createPersistentDataRuntime({
            store: storage.store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePluginStorage: () => database.pluginCustomStorage ?? null,
                capturePresets: () => database.botPresets,
                captureSelectedCharacter: () => database.characters[0] ?? null,
                captureCharacter: (id) => database.characters.find(
                    (character) => character.chaId === id,
                ) ?? null,
                getSelectedCharacterId: () => 'char-a',
                getSelectedConversationId: () => 'chat-a',
                replaceDatabase: (replacement) => {
                    database = replacement
                },
                publishCharacter: vi.fn(),
                publishConversation: vi.fn(),
            },
            prepareDatabase: async (value) => value,
        })
        await runtime.initializeActiveWorkingSet(database)
        await storage.store.replaceFromDatabase(restored, storage.revision)
        const leases = storage.leases()

        await expect(runtime.refreshActiveWorkingSetFromStore(2)).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'applied',
        })

        expect(runtime.revision).toBe(2)
        expect(database.username).toBe('After native restore')
        expect(database.characters[0].chats[0].message[0].data).toBe(
            'restored selected message',
        )
        expect(isCatalogCharacterStub(database.characters[1])).toBe(true)
        expect(database.characters[1]).not.toHaveProperty('personality')
        expect(materializeDatabase).not.toHaveBeenCalled()
        expect(leases.map((lease) => lease.revision)).toEqual([2])
        expect(leases[0].release).toHaveBeenCalledOnce()
    })

    it('edit_during_prepare_invalidates_auto_apply without discarding the local edit', async () => {
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')

        harness.username = 'Live edit during native parse'
        runtime.markPersistentDataDirty(1)

        const apply = vi.fn()
        await expect(runtime.acquireDestructiveReplacementFence(token).then(apply))
            .rejects.toThrow(/revision/i)
        expect(apply).not.toHaveBeenCalled()
        expect(runtime.revision).toBe(token.revision + 1)
        expect(harness.database.username).toBe('Live edit during native parse')
        expect(await harness.stored()).toEqual({ revision: token.revision + 1, username: 'Live edit during native parse' })
        expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
    })

    it('requires a fresh explicit-choice token after even a no-op dirty notice', async () => {
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')

        runtime.markPersistentDataDirty(0)

        await expect(runtime.acquireDestructiveReplacementFence(token)).rejects.toThrow(/replacement is active/i)
        expect(runtime.revision).toBe(token.revision)
        expect((await harness.stored()).revision).toBe(token.revision)
        const chosenAgain = await runtime.capturePersistentMutationToken('user-selected-restore-again', {
            publishOfficial: false,
        })
        const fence = await runtime.acquireDestructiveReplacementFence(chosenAgain)
        expect(fence.revision).toBe(token.revision)
        fence.release()
    })

    it('persists outstanding edits but never promotes a stale token during local drain', async () => {
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')

        harness.username = 'Live edit during native parse'
        runtime.markPersistentDataDirty(1)
        const flush = harness.storage.gate('commit')

        const fencing = runtime.acquireDestructiveReplacementFence(token)
        await flush.entered
        harness.username = 'Edit landed while the fence was flushing'
        flush.release()
        await expect(fencing).rejects.toThrow(/revision/i)
        expect(runtime.revision).toBeGreaterThan(token.revision)
        expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
        expect(await harness.stored()).toEqual({ revision: runtime.revision, username: 'Edit landed while the fence was flushing' })
    })

    it('rejects a destructive fence when a live edit happened after the captured token', async () => {
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')

        harness.username = 'Live edit during native parse'
        runtime.markPersistentDataDirty(1)

        await expect(runtime.acquireDestructiveReplacementFence(token)).rejects.toThrow(
            /revision|mutation generation/i,
        )
        expect(harness.database.username).toBe('Live edit during native parse')
        expect(runtime.revision).toBeGreaterThan(token.revision)
    })

    it('keeps edits fenced through committed refresh and releases only after acknowledgement', async () => {
        const restored = makeConversationDatabase('After native restore')
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
            restored,
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')
        const fence = await runtime.acquireDestructiveReplacementFence(token)

        expect(() => runtime.markPersistentDataDirty(1)).toThrow(/replacement is active/i)
        await fence.refreshCommittedWorkingSet(await harness.commitRestored())
        expect(harness.database.username).toBe('After native restore')
        expect(runtime.revision).toBe(2)
        expect(() => runtime.markPersistentDataDirty(1)).toThrow(/replacement is active/i)

        harness.username = 'Edit after native commit'
        expect(() => runtime.markPersistentDataDirty(1)).toThrow(/replacement is active/i)

        fence.release()
        runtime.markPersistentDataDirty(1)
        await runtime.flushPendingData('post-native-commit-edit')
        expect(harness.database.username).toBe('Edit after native commit')
        expect(await harness.stored()).toEqual({ revision: 3, username: 'Edit after native commit' })
    })

    it('keeps a stale working set read-only when native acknowledgement fails after commit', async () => {
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
            makeConversationDatabase('After native restore'),
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')
        const fence = await runtime.acquireDestructiveReplacementFence(token)

        runtime.markCommittedWorkingSetRefreshRequired(
            await harness.commitRestored(),
            new Error('synthetic native acknowledgement failure'),
        )
        fence.release()

        expect(runtime.revision).toBe(2)
        expect(runtime.pendingWorkingSetRefreshRevision).toBe(2)
        expect(harness.database.username).toBe('Before native restore')
        expect(() => runtime.assertPersistentMutationAllowed()).toThrow(/replacement is active/i)
        expect(() => runtime.markPersistentDataDirty(1)).toThrow(/replacement is active/i)
        expect(await harness.stored()).toEqual({ revision: 2, username: 'After native restore' })
    })

    it('does not overwrite an already-applied edit during committed working-set projection', async () => {
        const restored = makeConversationDatabase('After native restore')
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
            restored,
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')
        const fence = await runtime.acquireDestructiveReplacementFence(token)
        const projection = harness.storage.gate('acquireRevision')

        const refreshing = fence.refreshCommittedWorkingSet(await harness.commitRestored())
        await projection.entered
        harness.username = 'Edit resumed after native commit'
        projection.release()

        await expect(refreshing).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'refresh-required',
        })
        expect(harness.database.username).toBe('Edit resumed after native commit')
        expect(() => runtime.markPersistentDataDirty(1)).toThrow(/replacement is active/i)
        fence.release()
        expect(runtime.pendingWorkingSetRefreshRevision).toBe(2)
        expect(() => runtime.assertPersistentMutationAllowed()).toThrow(/replacement is active/i)
        expect(await harness.stored()).toEqual({ revision: 2, username: 'After native restore' })
    })

    it('keeps a large post-publication edit blocked until fence release', async () => {
        const restored = makeConversationDatabase('After native restore')
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native restore'),
            restored,
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-restore-start')
        const fence = await runtime.acquireDestructiveReplacementFence(token)
        await fence.refreshCommittedWorkingSet(await harness.commitRestored())

        harness.username = 'Large post-publication edit'
        expect(() => runtime.markPersistentDataDirty(2 * 1024 * 1024)).toThrow(
            /replacement is active/i,
        )
        fence.release()
        runtime.markPersistentDataDirty(2 * 1024 * 1024)
        await runtime.flushPendingData('large-post-publication-edit')

        expect(await harness.stored()).toEqual({ revision: 3, username: 'Large post-publication edit' })
    })

    it('reports a revision committed under the fence once, after release', async () => {
        const onLocalRevision = vi.fn()
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native commit'),
            makeConversationDatabase('After native commit'),
            onLocalRevision,
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-commit')
        const fence = await runtime.acquireDestructiveReplacementFence(token)
        await fence.refreshCommittedWorkingSet(await harness.commitRestored())
        expect(onLocalRevision).not.toHaveBeenCalled()
        fence.release()
        fence.release()
        expect(onLocalRevision).toHaveBeenCalledExactlyOnceWith(2)
    })

    it('reports a revision committed under the fence whose refresh is still pending', async () => {
        const onLocalRevision = vi.fn()
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before native commit'),
            makeConversationDatabase('After native commit'),
            onLocalRevision,
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const token = await runtime.capturePersistentMutationToken('native-commit')
        const fence = await runtime.acquireDestructiveReplacementFence(token)
        runtime.markCommittedWorkingSetRefreshRequired(await harness.commitRestored(), new Error('synthetic projection failure'))
        fence.release()
        expect(onLocalRevision).toHaveBeenCalledExactlyOnceWith(2)
    })

    it('reports nothing for a fence released at the revision it held', async () => {
        const onLocalRevision = vi.fn()
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Unchanged'),
            makeConversationDatabase('Unchanged'),
            onLocalRevision,
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        const fence = await runtime.acquireDestructiveReplacementFence(
            await runtime.capturePersistentMutationToken('native-commit-cancelled'),
        )
        fence.release()
        expect(onLocalRevision).not.toHaveBeenCalled()
    })

    it('does not report a revision committed elsewhere and projected through a refresh', async () => {
        const onLocalRevision = vi.fn()
        const harness = await createFenceRuntimeHarness(
            makeConversationDatabase('Before received change'),
            makeConversationDatabase('After received change'),
            onLocalRevision,
        )
        const { runtime } = harness
        await runtime.initializeActiveWorkingSet(harness.database)
        await harness.commitRestored()
        await expect(runtime.refreshActiveWorkingSetFromStore(2)).resolves.toMatchObject({ revision: 2, projection: 'applied' })
        expect(harness.database.username).toBe('After received change')
        expect(onLocalRevision).not.toHaveBeenCalled()
    })
})
