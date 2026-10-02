import { afterEach, describe, expect, it, vi } from 'vitest'
import { IDBFactory, IDBKeyRange, IDBObjectStore } from 'fake-indexeddb'
import type { Database, groupChat } from './database.svelte'
import { IndexedDbPersistentDataStore } from './indexedDbPersistentDataStore'
import { PersistentStorageQuotaError } from './persistentDataStore'
import { prepareNativePersistenceValue } from './nativePersistenceValue'
import type { WindowedConversationPersistenceAuthority } from './saveCoordinator'
import { createPersistentDataRuntime, publishPersistentCharacterMutationToWorkingSet } from './persistentDataRuntime'
import { WorkingSetResidencyRegistry } from './workingSetResidency'
import { captureRoot, deferred, makeDatabase, makeStore, SaveCoordinator } from './saveCoordinator.testSupport'

vi.mock('../platform', () => ({ isTauri: false }))
afterEach(() => vi.useRealTimers())

async function durableHarness(database: Database, selectedCharacterId?: string) {
    const store = new IndexedDbPersistentDataStore(`coordinator-review-${crypto.randomUUID()}`, new IDBFactory(), IDBKeyRange)
    await store.open()
    const { revision } = await store.replaceFromDatabase(database)
    const residency = new WorkingSetResidencyRegistry()
    const coordinator = new SaveCoordinator({
        store,
        captureRoot: () => captureRoot(database),
        captureCharacter: (id) => database.characters.find((value) => value.chaId === id) ?? null,
        captureSelectedCharacter: () => database.characters.find((value) => value.chaId === selectedCharacterId) ?? null,
        replaceDatabase: () => undefined,
        publishCharacterMutation: (result) => publishPersistentCharacterMutationToWorkingSet(
            database, result, residency, database.characters.findIndex((value) => value.chaId === selectedCharacterId), () => undefined,
        ),
    })
    coordinator.initialize(revision)
    return { coordinator, store }
}

describe('coordinator review regressions', () => {
    it('keeps zero-byte character and chat edits pending after a failure and retries before reporting idle', async () => {
        vi.useFakeTimers()
        const database = makeDatabase()
        database.characters[0].chats = [{ id: 'chat', name: 'Before', message: [] } as any]
        const commit = vi.fn().mockRejectedValueOnce(new Error('synthetic transient write failure'))
            .mockImplementation(async ({ expectedRevision }) => ({ revision: expectedRevision + 1 }))
        const idle = vi.fn()
        const errors = vi.fn()
        const failures = vi.fn()
        const coordinator = new SaveCoordinator({
            store: makeStore(commit), captureRoot: () => captureRoot(database),
            captureSelectedCharacter: () => database.characters[0], replaceDatabase: () => undefined,
            onPersistenceIdle: idle, onBackgroundError: errors, onLocalSaveFailure: failures,
        })
        coordinator.initialize(1)
        database.characters[0].name = 'Edited character'
        database.characters[0].chats[0].name = 'Edited chat'
        coordinator.markPersistentDataDirty(0)
        await vi.advanceTimersByTimeAsync(500)
        expect(commit).toHaveBeenCalledTimes(1)
        expect(coordinator.pendingBytes).toBe(0)
        expect(coordinator.hasPendingPersistenceWork).toBe(true)
        expect(idle).not.toHaveBeenCalled()
        expect(failures).toHaveBeenLastCalledWith(expect.objectContaining({ message: 'synthetic transient write failure' }))
        await vi.advanceTimersByTimeAsync(2_000)
        expect(commit).toHaveBeenCalledTimes(2)
        expect(commit.mock.calls[1][0].replaceCharacter).toMatchObject({
            name: 'Edited character', chats: [{ name: 'Edited chat' }],
        })
        expect(coordinator.hasPendingPersistenceWork).toBe(false)
        expect(idle).toHaveBeenCalledOnce()
        expect(errors).toHaveBeenCalledOnce()
        expect(failures).toHaveBeenCalledTimes(2)
        expect(failures).toHaveBeenLastCalledWith(null)
    })

    it('keeps quota failures pending without scheduling an automatic retry', async () => {
        vi.useFakeTimers()
        const database = makeDatabase()
        const commit = vi.fn().mockRejectedValue(new PersistentStorageQuotaError())
        const failures = vi.fn()
        const coordinator = new SaveCoordinator({
            store: makeStore(commit), captureRoot: () => captureRoot(database),
            captureSelectedCharacter: () => database.characters[0], replaceDatabase: () => undefined,
            onLocalSaveFailure: failures,
        })
        coordinator.initialize(1)
        database.characters[0].name = 'Pending'
        coordinator.markPersistentDataDirty(0)
        await vi.advanceTimersByTimeAsync(65_000)
        expect(commit).toHaveBeenCalledOnce()
        expect(coordinator.hasPendingPersistenceWork).toBe(true)
        expect(failures).toHaveBeenCalledOnce()
        expect(failures).toHaveBeenLastCalledWith(expect.any(PersistentStorageQuotaError))
        commit.mockImplementation(async ({ expectedRevision }) => ({ revision: expectedRevision + 1 }))
        await coordinator.flushPendingDataLocally('manual-save-retry')
        expect(failures).toHaveBeenLastCalledWith(null)
        expect(coordinator.hasPendingPersistenceWork).toBe(false)
    })

    it('skips identity and cloned no-op replacements but retains in-place edits', async () => {
        const database = makeDatabase()
        const { coordinator, store } = await durableHarness(database)
        const original = database.characters[0]
        const commit = vi.spyOn(store, 'commit')
        expect(await coordinator.replacePersistentCompleteCharacter('char-a', 'no-op', (value) => value)).toBe(true)
        expect(await coordinator.upsertPersistentCompleteCharacter('char-a', 'clone-no-op', (value) => structuredClone(value!))).toBe(true)
        expect(commit).not.toHaveBeenCalled()
        expect(database.characters[0]).toBe(original)
        await coordinator.replacePersistentCompleteCharacter('char-a', 'changed', (value) => {
            value.name = 'Changed'
            return value
        })
        expect(commit).toHaveBeenCalledOnce()
        expect((await store.readCharacter('char-a'))?.value.name).toBe('Changed')
        expect(coordinator.hasPendingPersistenceWork).toBe(false)
    })

    it('expires 200 trashed characters in bounded batches and repairs live and trashed groups atomically', async () => {
        const now = 10 * 24 * 60 * 60 * 1000
        const database = makeDatabase()
        const expired = Array.from({ length: 200 }, (_, index) => ({
            ...structuredClone(database.characters[0]), chaId: `expired-${index}`, trashTime: 1,
        }))
        const group = { type: 'group', chaId: 'group', name: 'Group', chats: [],
            characters: ['char-a', 'expired-0', 'expired-199'], characterTalks: [1, 2, 3], characterActive: [true, false, false] } as unknown as groupChat
        database.characters.push(...expired, group, { ...structuredClone(group), chaId: 'trashed-group', trashTime: now - 1 })
        database.characterOrder = ['char-a', ...expired.map((value) => value.chaId), 'group', 'trashed-group']
        const { coordinator, store } = await durableHarness(database)
        const commit = vi.spyOn(store, 'commit')
        expect(await coordinator.expirePersistentTrash(now)).toBe(200)
        expect(commit).toHaveBeenCalledTimes(2)
        const deletionBatches = commit.mock.calls.map(([input]) => input.unitMutations?.filter((mutation) =>
            mutation.type === 'delete').map((mutation) => JSON.parse(mutation.key)))
        expect(deletionBatches.map((batch) => batch?.length)).toEqual([128, 72])
        expect(deletionBatches.flat()).toEqual(expired.map((value) => ['exists', 'character', value.chaId]))
        const restored = await store.materializeDatabase(coordinator.revision)
        expect(restored.characters.map((value) => value.chaId)).toEqual(['char-a', 'group', 'trashed-group'])
        for (const id of ['group', 'trashed-group']) {
            expect(restored.characters.find((value) => value.chaId === id)).toMatchObject({
                characters: ['char-a'], characterTalks: [1], characterActive: [true],
            })
        }
        expect(restored.characterOrder).toEqual(['char-a', 'group', 'trashed-group'])
    })

    it('leaves trash and group membership intact when an expiry batch fails', async () => {
        const database = makeDatabase()
        database.characters[0].trashTime = 1
        database.characters.push({ ...structuredClone(database.characters[0]), chaId: 'char-b' })
        database.characters.push({ type: 'group', chaId: 'group', name: 'Group', chats: [],
            characters: ['char-a', 'char-b'], characterTalks: [0.25, 0.75], characterActive: [true, false],
        } as unknown as groupChat)
        database.characterOrder = ['group', 'char-a', 'char-b']
        const { coordinator, store } = await durableHarness(database)
        const before = await store.materializeDatabase(coordinator.revision)
        const commit = vi.spyOn(store, 'commit').mockRejectedValueOnce(new Error('synthetic expiry failure'))
        await expect(coordinator.expirePersistentTrash(10 * 24 * 60 * 60 * 1000)).rejects.toThrow('synthetic expiry failure')
        expect(commit.mock.calls[0][0].unitMutations?.filter((mutation) => mutation.type === 'delete')
            .map((mutation) => JSON.parse(mutation.key))).toEqual([
                ['exists', 'character', 'char-a'], ['exists', 'character', 'char-b'],
            ])
        expect(await store.materializeDatabase(coordinator.revision)).toEqual(before)
        expect(database.characters).toHaveLength(3)
        expect(database.characters.find((value) => value.chaId === 'group')).toMatchObject({
            characters: ['char-a', 'char-b'], characterTalks: [0.25, 0.75], characterActive: [true, false],
        })
    })
    it('mutates unrelated targets while windowed and obtains a complete lease for the selected target', async () => {
        let database = makeDatabase()
        database.characters[0].chatPage = 0
        database.characters[0].chats = [{ id: 'chat', name: 'Chat', message: [
            { role: 'user', data: 'Persisted message' },
        ] } as any]
        database.characters.push({ ...structuredClone(database.characters[0]), chaId: 'other' })
        const store = new IndexedDbPersistentDataStore(`windowed-explicit-${crypto.randomUUID()}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        await store.replaceFromDatabase(database)
        const residency = new WorkingSetResidencyRegistry()
        const publishCharacter = (character: Database['characters'][number]) => {
            const index = database.characters.findIndex((value) => value.chaId === character.chaId)
            if (index >= 0) database.characters[index] = character
        }
        const errors = vi.fn()
        const runtime = createPersistentDataRuntime({ store, onBackgroundError: errors, prepareDatabase: async (value) => value,
            state: {
                captureRoot: () => captureRoot(database),
                captureSelectedCharacter: () => database.characters[0],
                captureCharacter: (id) => database.characters.find((value) => value.chaId === id) ?? null,
                getSelectedCharacterId: () => 'char-a', getSelectedConversationId: () => 'chat',
                replaceDatabase: (value) => { database = value },
                publishCharacter,
                publishConversation: (characterId, conversation, nextCharacter) => {
                    if (nextCharacter) publishCharacter(nextCharacter)
                    const character = database.characters.find((value) => value.chaId === characterId)!
                    const index = character.chats.findIndex((value) => value.id === conversation.id)
                    if (index < 0) character.chats.push(conversation)
                    else character.chats[index] = conversation
                },
                publishCharacterMutation: (result) => publishPersistentCharacterMutationToWorkingSet(
                    database, result, residency, 0, () => undefined,
                ),
                canUseWindowedSelectedConversation: () => true,
                shouldHydrateFullCharacter: () => false,
            },
        })
        await runtime.initializeActiveWorkingSet(database)
        await runtime.activateCharacter('char-a')
        await runtime.tryDemoteSelectedConversation()
        expect(runtime.getSelectedConversationMode()).toBe('windowed')
        await expect(runtime.mutatePersistentCharacterDetail('other', 'unrelated-detail', ({ character }) => {
            character.name = 'Changed other'
        })).resolves.toBe(true)
        expect(runtime.getSelectedConversationMode()).toBe('windowed')
        await expect(runtime.upsertPersistentCompleteCharacter('new-id', 'new-character', () => ({
            ...makeDatabase().characters[0], chaId: 'new-id', name: 'New',
        }))).resolves.toBe(true)
        await expect(runtime.deletePersistentCharacterWithGroupReferences('other', 'unrelated-delete')).resolves.toBe(true)
        expect(runtime.getSelectedConversationMode()).toBe('windowed')
        expect(errors.mock.calls).toEqual([])
        await expect(runtime.mutatePersistentCharacterDetail('char-a', 'selected-detail', ({ character }) => {
            expect(runtime.getSelectedConversationMode()).toBe('complete')
            character.name = 'Selected edit'
        })).resolves.toBe(true)
        await runtime.tryDemoteSelectedConversation()
        expect(runtime.getSelectedConversationMode()).toBe('windowed')
        expect((await store.readCharacter('char-a'))?.value.name).toBe('Selected edit')
        expect((await store.readConversation('char-a', 'chat'))?.value.message[0].data).toBe('Persisted message')
        expect(await store.readCharacter('other')).toBeNull()
        expect((await store.readCharacter('new-id'))?.value.name).toBe('New')
    })

    it('preserves root and selected chat edits made while deletion waits on a catalog page', async () => {
        const database = makeDatabase()
        database.characters[0].chats = [{ id: 'chat', name: 'Chat', message: [{ role: 'user', data: 'Before' }] } as any]
        database.characters.push({ ...structuredClone(database.characters[0]), chaId: 'delete-me' })
        database.characterOrder = ['char-a', 'delete-me']
        const { coordinator, store } = await durableHarness(database, 'char-a')
        const commit = vi.spyOn(store, 'commit')
        const entered = deferred<void>()
        const resume = deferred<void>()
        const acquire = store.acquireRevision.bind(store)
        vi.spyOn(store, 'acquireRevision').mockImplementation(async (revision) => {
            const lease = await acquire(revision)
            const query = lease.queryCharacters.bind(lease)
            vi.spyOn(lease, 'queryCharacters').mockImplementationOnce(async (input) => {
                const result = await query(input)
                entered.resolve()
                await resume.promise
                return result
            })
            return lease
        })
        const deleting = coordinator.deletePersistentCharacterWithGroupReferences('delete-me', 'concurrent-delete')
        await entered.promise
        database.username = 'Concurrent root edit'
        database.characters[0].chats[0].message[0].data = 'Concurrent chat edit'
        coordinator.markPersistentDataDirty(0)
        resume.resolve()
        await expect(deleting).resolves.toBe(true)
        expect((await store.readRoot()).value.username).toBe('Fixture')
        expect(database.username).toBe('Concurrent root edit')
        expect(commit.mock.calls[0][0].rootMutations).toBeUndefined()
        expect(commit.mock.calls[0][0].root).toBeUndefined()
        await coordinator.flushPendingDataLocally('preserve-concurrent-edits')
        expect((await store.readRoot()).value.username).toBe('Concurrent root edit')
        expect((await store.readConversation('char-a', 'chat'))?.value.message[0].data).toBe('Concurrent chat edit')
        expect(await store.readCharacter('delete-me')).toBeNull()
    })

    it('rejects deletion when the selected referenced group changes during the catalog scan', async () => {
        const database = makeDatabase()
        const group = { type: 'group', chaId: 'group', name: 'Group', chats: [],
            characters: ['char-a'], characterTalks: [1], characterActive: [true] } as unknown as groupChat
        database.characters.push(group)
        const { coordinator, store } = await durableHarness(database, 'group')
        const entered = deferred<void>()
        const resume = deferred<void>()
        const acquire = store.acquireRevision.bind(store)
        vi.spyOn(store, 'acquireRevision').mockImplementation(async (revision) => {
            const lease = await acquire(revision)
            const query = lease.queryCharacters.bind(lease)
            vi.spyOn(lease, 'queryCharacters').mockImplementationOnce(async (input) => {
                const result = await query(input)
                entered.resolve()
                await resume.promise
                return result
            })
            return lease
        })
        const deleting = coordinator.deletePersistentCharacterWithGroupReferences('char-a', 'conflicting-delete')
        const outcome = expect(deleting).rejects.toThrow()
        await entered.promise
        group.name = 'Concurrent group edit'
        coordinator.markPersistentDataDirty(0)
        resume.resolve()
        await outcome
        expect(await store.readCharacter('char-a')).not.toBeNull()
        expect((await store.readCharacter('group'))?.value).toMatchObject({ characters: ['char-a'] })
        await coordinator.flushPendingDataLocally('save-group-edit')
    })

    it('maps an IndexedDB quota failure to a typed save error without changing durable content', async () => {
        const database = makeDatabase()
        const { coordinator, store } = await durableHarness(database)
        const put = vi.spyOn(IDBObjectStore.prototype, 'put').mockImplementationOnce(() => {
            throw new DOMException('synthetic quota', 'QuotaExceededError')
        })
        try {
            await expect(store.commit({ expectedRevision: coordinator.revision,
                rootMutations: [{ type: 'set', key: 'username', value: 'must not persist' }],
            })).rejects.toBeInstanceOf(PersistentStorageQuotaError)
        } finally { put.mockRestore() }
        expect((await store.readRoot()).value.username).toBe('Fixture')
    })

    it('pages searched Unicode names by stable recent keys without repeating the matching prefix', async () => {
        const database = makeDatabase()
        database.characters = Array.from({ length: 37 }, (_, index) => ({
            ...structuredClone(database.characters[0]), chaId: `id-${index}`,
            name: index % 2 ? `한글 ÄLPHA ${index}` : `Other ${index}`, lastInteraction: Math.floor(index / 4),
        }))
        const { store } = await durableHarness(database)
        const ids: string[] = []
        let cursor: string | undefined
        do {
            const page = await store.queryCharacters({ order: 'recent', trash: false, search: 'älpha', limit: 3, cursor })
            ids.push(...page.items.map((item) => item.id))
            if (page.nextCursor) {
                const last = page.items.at(-1)!
                expect(JSON.parse(page.nextCursor)).toEqual([last.recentAt, last.configuredIndex, last.id])
            }
            cursor = page.nextCursor
        } while (cursor)
        const expected = database.characters.map((value, index) => ({ value, index }))
            .filter(({ index }) => index % 2).sort((a, b) => b.value.lastInteraction! - a.value.lastInteraction! || a.index - b.index)
            .map(({ value }) => value.chaId)
        expect(ids).toEqual(expected)
        expect(new Set(ids).size).toBe(ids.length)
    })

    it('does not resave native Unicode replacements and accepts a windowed edit after reload', async () => {
        const database = makeDatabase()
        database.characters[0].chats = [{ id: 'chat', name: 'Chat', message: [
            { role: 'user', data: 'Before' },
        ] } as any]
        const store = new IndexedDbPersistentDataStore(`native-unicode-${crypto.randomUUID()}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        const initial = await store.replaceFromDatabase(database)
        const commitToStore = store.commit.bind(store)
        const commit = vi.spyOn(store, 'commit').mockImplementation((input) =>
            commitToStore(prepareNativePersistenceValue(input)),
        )
        let selected = database.characters[0]
        let authority: WindowedConversationPersistenceAuthority | null = null
        const coordinator = new SaveCoordinator({
            store, captureRoot: () => captureRoot(database),
            captureSelectedCharacter: () => selected, captureSelectedConversationAuthority: () => authority,
            replaceDatabase: () => undefined,
            onWindowedSelectedConversationRevision: (revision) => {
                if (authority) authority = { ...authority, storeRevision: revision }
            },
            onConversationMutationPersisted: (event) => {
                if (authority) authority = { ...authority, storeRevision: event.revision,
                    persistedSessionVersion: event.sessionVersion }
            },
        })
        coordinator.initialize(initial.revision)
        selected.chats[0].name = 'Chat \uD800'
        selected.chats[0].message[0].data = 'Text \uDC00'
        coordinator.markPersistentDataDirty(0)
        await coordinator.flushPendingDataLocally('native-unicode-save')
        await coordinator.flushPendingDataLocally('unchanged-native-unicode-save')
        expect(commit).toHaveBeenCalledOnce()
        expect(coordinator.hasPendingPersistenceWork).toBe(false)
        expect(selected.chats[0].message[0].data).toBe('Text \uDC00')

        const stored = await store.readConversation('char-a', 'chat')
        const window = await store.readConversationWindow({ characterId: 'char-a', conversationId: 'chat', limit: 1 })
        expect(stored?.value.name).toBe('Chat \uFFFD')
        expect(window?.value.messages[0].data).toBe('Text \uFFFD')
        const { message: _message, ...metadata } = stored!.value
        const projectedChat = { ...metadata }
        Object.defineProperty(projectedChat, 'message', { enumerable: true, get: () => {
            throw new Error('windowed metadata capture must not read message bodies')
        } })
        selected = { ...selected, chats: [projectedChat] } as any
        authority = { kind: 'windowed', characterId: 'char-a', conversationId: 'chat',
            sessionToken: 'unicode-reload' as any, storeRevision: coordinator.revision,
            persistedSessionVersion: 0, sessionVersion: 0, totalMessages: 1 }
        expect(coordinator.adoptWindowedSelectedConversation(coordinator.revision,
            coordinator.mutationGeneration, selected, authority)).toBe(true)
        authority = { ...authority, sessionVersion: 1 }
        coordinator.recordActiveConversationMutation({ characterId: 'char-a', conversationId: 'chat',
            sessionToken: authority.sessionToken, previousVersion: 0, sessionVersion: 1,
            commands: ['replace-range'], conversation: metadata,
            mutations: [{ start: 0, deleteCount: 1, sessionVersion: 1,
                messages: [{ ...window!.value.messages[0], data: 'Text \uFFFD edited' }] }],
        })
        await coordinator.flushPendingDataLocally('windowed-native-unicode-edit')
        await coordinator.flushPendingDataLocally('unchanged-windowed-native-unicode-edit')
        expect(commit).toHaveBeenCalledTimes(2)
        expect(commit.mock.calls[1][0].replaceCharacter).toBeUndefined()
        expect((await store.readConversation('char-a', 'chat'))?.value.message[0].data).toBe('Text \uFFFD edited')
        expect(coordinator.hasPendingPersistenceWork).toBe(false)
    })

    it('does not report successful local data as unsaved when official publication fails', async () => {
        vi.useFakeTimers()
        const database = makeDatabase()
        const failures = vi.fn()
        const coordinator = new SaveCoordinator({
            store: makeStore(vi.fn(async ({ expectedRevision }) => ({ revision: expectedRevision + 1 }))),
            captureRoot: () => captureRoot(database), captureSelectedCharacter: () => database.characters[0],
            replaceDatabase: () => undefined, onLocalSaveFailure: failures,
            officialPublisher: { pin: async () => ({
                publish: async () => { throw new Error('synthetic publication failure') }, dispose: async () => {},
            }) },
        })
        coordinator.initialize(1)
        database.username = 'Saved locally'
        coordinator.markPersistentDataDirty(0)
        await expect(coordinator.flushPendingData('save-and-publish')).rejects.toThrow('synthetic publication failure')
        expect(failures).not.toHaveBeenCalled()
    })

    it('holds scheduled official publication until the replacement fence releases', async () => {
        vi.useFakeTimers()
        const database = makeDatabase()
        const pin = vi.fn(async () => ({ publish: vi.fn(async () => {}), dispose: vi.fn(async () => {}) }))
        const errors = vi.fn()
        const coordinator = new SaveCoordinator({
            store: makeStore(vi.fn(async ({ expectedRevision }) => ({ revision: expectedRevision + 1 }))),
            captureRoot: () => captureRoot(database), captureSelectedCharacter: () => database.characters[0],
            replaceDatabase: () => undefined, officialPublisher: { pin }, onBackgroundError: errors,
        })
        coordinator.initialize(1)
        database.username = 'Local revision'
        coordinator.markPersistentDataDirty(0)
        await coordinator.flushPendingDataLocally('local-only')
        const token = await coordinator.capturePersistentMutationToken('replacement', { publishOfficial: false })
        const fence = await coordinator.acquireDestructiveReplacementFence(token)
        await vi.advanceTimersByTimeAsync(15_000)
        expect(pin).not.toHaveBeenCalled()
        expect(errors).not.toHaveBeenCalled()
        coordinator.releaseDestructiveReplacementFence(fence)
        await vi.advanceTimersByTimeAsync(3_000)
        expect(pin).toHaveBeenCalledOnce()
        expect(errors).not.toHaveBeenCalled()
    })

})
