import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { afterEach, expect, it, vi } from 'vitest'
import { IndexedDbPersistentDataStore } from '../indexedDbPersistentDataStore'
import { capturePersistentRoot, createPersistentDataRuntime } from '../persistentDataRuntime'
import type { Database, character } from '../database.svelte'

afterEach(() => vi.restoreAllMocks())

async function fixture() {
    const store = new IndexedDbPersistentDataStore('synthetic-css-promotion-race', new IDBFactory(), IDBKeyRange)
    await store.open()
    let database = {
        username: 'Synthetic', botPresets: [], characters: [{
            type: 'character', chaId: 'synthetic-character', name: 'Synthetic', chatPage: 0,
            chats: [{id:'synthetic-chat', name:'Synthetic', note:'', localLore:[], message:[{role:'char', data:'Synthetic message'}]}],
        }],
    } as unknown as Database
    let selected: string | null = null
    const imported = await store.replaceFromDatabase(database)
    const runtime = createPersistentDataRuntime({
        store,
        state: {
            captureRoot: () => capturePersistentRoot(database),
            capturePresets: () => database.botPresets,
            captureSelectedCharacter: () => selected ? database.characters[0] as character : null,
            captureCharacter: id => database.characters.find(c => c.chaId === id) as character ?? null,
            getSelectedCharacterId: () => selected,
            getSelectedConversationId: () => selected ? database.characters[0].chats[0].id : null,
            replaceDatabase: value => {database = value},
            publishCharacter: value => {database.characters[0] = value; selected = value.chaId},
            publishConversation: (_id, conversation, nextCharacter) => {
                if (nextCharacter) database.characters[0] = nextCharacter
                else database.characters[0].chats[0] = conversation
            },
            canUseWindowedSelectedConversation: () => true,
            shouldHydrateFullCharacter: () => false,
            canReleaseConversation: () => true,
        },
        prepareDatabase: async value => value,
    })
    await runtime.initializeActiveWorkingSet(database)
    expect(await runtime.activateCharacter('synthetic-character')).toBe(true)
    expect(runtime.getSelectedConversationMode()).toBe('windowed')
    return {store, runtime, imported, database: () => database}
}

it.each(['none', 'pending', 'committed'])('finishes one acquisition when a save is %s during its read', async (save) => {
    const {store, runtime, imported, database} = await fixture()
    const target = runtime.captureSelectedConversationTarget()!
    const read = store.readConversation.bind(store)
    vi.spyOn(store, 'readConversation').mockImplementationOnce(async (...args) => {
        const value = await read(...args)
        if (save !== 'none') {
            database().username = 'Changed synthetic root'
            runtime.markPersistentDataDirty(1)
            if (save === 'committed') await runtime.flushPendingData('synthetic-concurrent-save')
        }
        return value
    })
    const lease = await runtime.acquireCompleteConversation('live-display-parser', target)
    expect(runtime.getSelectedConversationMode()).toBe('complete')
    expect(database().characters[0].chats[0].message).toEqual([{role:'char',data:'Synthetic message'}])
    expect(runtime.revision).toBe(imported.revision + (save === 'none' ? 0 : 1))
    expect(lease.target.storeRevision).toBe(runtime.revision)
    expect((await store.readRoot()).value.username).toBe(save === 'none' ? 'Synthetic' : 'Changed synthetic root')
    lease.release()
})

it('settles a pending save without replacing the shared promotion or rereading unchanged data', async () => {
    const {store, runtime, imported, database} = await fixture()
    const target = runtime.captureSelectedConversationTarget()!
    const read = store.readConversation.bind(store)
    const readSpy = vi.spyOn(store, 'readConversation').mockImplementationOnce(async (...args) => {
        const value = await read(...args)
        runtime.markPersistentDataDirty(1)
        return value
    })
    const [first, second] = await Promise.all([
        runtime.acquireCompleteConversation('background', target),
        runtime.acquireCompleteConversation('plugin', target),
    ])
    expect(readSpy).toHaveBeenCalledOnce()
    expect(first.session).toBe(second.session)
    expect(first.session.pinCount('compatibility')).toBe(2)
    expect(runtime.revision).toBe(imported.revision)
    expect(database().characters[0].chats[0].message).toHaveLength(1)
    first.release()
    second.release()
})

it('does not publish a stale body when a message edit is saved during the read', async () => {
    const {store, runtime, database} = await fixture()
    const target = runtime.captureSelectedConversationTarget()!
    const read = store.readConversation.bind(store)
    vi.spyOn(store, 'readConversation').mockImplementationOnce(async (...args) => {
        const value = await read(...args)
        const chat = structuredClone(value!.value)
        const edit = runtime.captureWindowedConversationMutationController(target, chat, 0)!
        expect(edit.applyRange(0, 1, [{role:'char', data:'Edited synthetic message'}], 'edit')).toBe(true)
        edit.release()
        return value
    })
    const lease = await runtime.acquireCompleteConversation('background', target)
    expect(database().characters[0].chats[0].message[0].data).toBe('Edited synthetic message')
    expect((await read('synthetic-character', 'synthetic-chat'))!.value.message[0].data).toBe('Edited synthetic message')
    lease.release()
})

it('propagates a real save failure and leaves the selected conversation windowed', async () => {
    const {store, runtime, database} = await fixture()
    const target = runtime.captureSelectedConversationTarget()!
    const failure = new Error('Synthetic persistence unavailable')
    const read = store.readConversation.bind(store)
    vi.spyOn(store, 'readConversation').mockImplementationOnce(async (...args) => {
        const value = await read(...args)
        database().username = 'Synthetic pending root'
        runtime.markPersistentDataDirty(1)
        return value
    })
    const commit = vi.spyOn(store, 'commit').mockRejectedValueOnce(failure)
    await expect(runtime.acquireCompleteConversation('background', target)).rejects.toBe(failure)
    expect(runtime.getSelectedConversationMode()).toBe('windowed')
    expect(runtime.captureSelectedConversationTarget()).toEqual(target)
    commit.mockRestore()
    await runtime.flushPendingData('synthetic-cleanup')
})

it('stops preparation after navigation even if a save became pending during the read', async () => {
    const {store, runtime} = await fixture()
    const target = runtime.captureSelectedConversationTarget()!
    const read = store.readConversation.bind(store)
    vi.spyOn(store, 'readConversation').mockImplementationOnce(async (...args) => {
        const value = await read(...args)
        runtime.markPersistentDataDirty(1)
        runtime.fenceNavigation()
        return value
    })
    await expect(runtime.acquireCompleteConversation('background', target)).rejects.toThrow('changed during complete promotion')
    expect(runtime.getSelectedConversationMode()).toBe('windowed')
    expect(runtime.getActiveConversationSession()).toBeNull()
    await runtime.flushPendingData('synthetic-cleanup')
})

it('stops preparation if navigation changes while the follow-up save is committing', async () => {
    const {store, runtime, database} = await fixture()
    const target = runtime.captureSelectedConversationTarget()!
    const read = store.readConversation.bind(store)
    vi.spyOn(store, 'readConversation').mockImplementationOnce(async (...args) => {
        const value = await read(...args)
        database().username = 'Synthetic concurrent save'
        runtime.markPersistentDataDirty(1)
        return value
    })
    let started!: () => void
    let finish!: () => void
    const committing = new Promise<void>(resolve => {started = resolve})
    const gate = new Promise<void>(resolve => {finish = resolve})
    const commit = store.commit.bind(store)
    vi.spyOn(store, 'commit').mockImplementationOnce(async (...args) => {
        started()
        await gate
        return commit(...args)
    })
    const result = expect(runtime.acquireCompleteConversation('background', target))
        .rejects.toThrow('changed during complete promotion')
    await committing
    runtime.fenceNavigation()
    finish()
    await result
    expect(runtime.getActiveConversationSession()).toBeNull()
    expect((await store.readRoot()).value.username).toBe('Synthetic concurrent save')
})
