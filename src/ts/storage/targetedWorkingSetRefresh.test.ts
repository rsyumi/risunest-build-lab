import 'fake-indexeddb/auto'
import { describe, expect, it, vi } from 'vitest'
import type { Database } from './database.svelte'
import { carryCatalogCharacterMetadata, getCatalogCharacterMetadata } from './workingSetCatalog'
import { isMetadataOnlySelectedConversation } from './selectedConversationLifecycle'
import { IndexedDbPersistentDataStore } from './indexedDbPersistentDataStore'
import type {
    ContentChangeKey,
    ContentChangeWindow,
    DataRevision,
    PersistentDataStore,
} from './persistentDataStore'
import {
    capturePersistentPluginStorage,
    capturePersistentPresets,
    capturePersistentRoot,
    createPersistentDataRuntime,
    type PersistentDataRuntimeStateAdapter,
} from './persistentDataRuntime'

vi.mock('./database.svelte', () => ({
    getDatabase: () => {
        throw new Error('No global database in targeted refresh tests')
    },
    presetTemplate: {},
}))
vi.mock('../globalApi.svelte', () => ({ forageStorage: {} }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))

function makeDatabase(username: string): Database {
    return {
        username,
        botPresetsId: 0,
        characters: [
            {
                type: 'character',
                chaId: 'char-a',
                name: 'Alpha',
                chatPage: 0,
                chats: [{ id: 'chat-a', name: 'First', message: [] }],
            },
            {
                type: 'character',
                chaId: 'char-b',
                name: 'Beta',
                chatPage: 0,
                chats: [{ id: 'chat-b', name: 'Second', message: [] }],
            },
        ],
        botPresets: [{ id: 'preset-a', name: 'Preset' }],
        pluginCustomStorage: {},
    } as unknown as Database
}

/// The change window every native store exposes, scripted for one refresh.
function withScriptedChangeWindow(store: IndexedDbPersistentDataStore): {
    store: PersistentDataStore
    script(window: ContentChangeWindow | null, keys: ContentChangeKey[]): void
    cursors: DataRevision[]
    queryCharacterCalls(): number
    readConversationCalls(): number
    resetCounts(): void
    failTargetedReads(failing: boolean): void
} {
    let scriptedWindow: ContentChangeWindow | null = null
    let scriptedKeys: ContentChangeKey[] = []
    const cursors: DataRevision[] = []
    let queryCharacterCalls = 0
    let readConversationCalls = 0
    let failing = false
    const acquireRevision = store.acquireRevision.bind(store)
    const decorated = Object.create(store) as PersistentDataStore
    Object.assign(decorated, {
        acquireRevision: async (revision: DataRevision) => {
            const lease = await acquireRevision(revision)
            const queryCharacters = lease.queryCharacters.bind(lease)
            const readCharacterSummary = lease.readCharacterSummary.bind(lease)
            const readConversation = lease.readConversation.bind(lease)
            return Object.assign(Object.create(lease), {
                readConversation: (...args: Parameters<typeof readConversation>) => {
                    readConversationCalls += 1
                    return readConversation(...args)
                },
                queryCharacters: (input: Parameters<typeof queryCharacters>[0]) => {
                    queryCharacterCalls += 1
                    return queryCharacters(input)
                },
                readCharacterSummary: async (id: string) => {
                    // Fails the targeted pass once and lets the fallback through.
                    if (failing) {
                        failing = false
                        throw new Error('Synthetic targeted read failure')
                    }
                    return readCharacterSummary(id)
                },
                readWorkingSetChangeWindow: async () =>
                    scriptedWindow ?? { revision, afterRevision: null },
                readWorkingSetChangePage: async (
                    _afterRevision: DataRevision,
                    afterKey: ContentChangeKey | null,
                ) => (afterKey === null ? scriptedKeys : []),
            })
        },
        commitWorkingSetChangeCursor: async (revision: DataRevision) => {
            cursors.push(revision)
        },
    })
    return {
        store: decorated,
        script(window, keys) {
            scriptedWindow = window
            scriptedKeys = keys
        },
        cursors,
        queryCharacterCalls: () => queryCharacterCalls,
        readConversationCalls: () => readConversationCalls,
        resetCounts() {
            queryCharacterCalls = 0
            readConversationCalls = 0
        },
        failTargetedReads(next: boolean) {
            failing = next
        },
    }
}

function makeState(database: Database): PersistentDataRuntimeStateAdapter & {
    current(): Database
    generating: { characterId: string; conversationId: string } | null
    operationActive: boolean
    operationCheck?: () => void
    windowedAllowed: boolean
    endGeneration(): void
} {
    let current = structuredClone(database)
    const listeners = new Set<(active: boolean) => void>()
    const state = {
        current: () => current,
        generating: null as { characterId: string; conversationId: string } | null,
        operationActive: false,
        operationCheck: undefined as (() => void) | undefined,
        windowedAllowed: false,
        canUseWindowedSelectedConversation: () => state.windowedAllowed,
        canReleaseConversation: () => true,
        endGeneration() {
            state.operationActive = false
            for (const listener of listeners) listener(false)
        },
        subscribeConversationOperationActive: (listener: (active: boolean) => void) => {
            listeners.add(listener)
            listener(state.operationActive)
            return () => listeners.delete(listener)
        },
        captureRoot: () => capturePersistentRoot(current),
        capturePluginStorage: () => capturePersistentPluginStorage(current),
        capturePresets: () => capturePersistentPresets(current),
        captureSelectedCharacter: () => current.characters[0] ?? null,
        captureCharacter: (id: string) =>
            current.characters.find((character) => character.chaId === id) ?? null,
        getSelectedCharacterId: () => current.characters[0]?.chaId ?? null,
        getSelectedConversationId: () => current.characters[0]?.chats[0]?.id ?? null,
        captureWorkingSetDatabase: () => current,
        isConversationOperationActive: () => {
            state.operationCheck?.()
            return state.operationActive
        },
        getGeneratingConversation: () => state.generating,
        replaceDatabase: (database: Database) => {
            current = database
        },
        publishCharacter: (character: Database['characters'][number]) => {
            const index = current.characters.findIndex((item) => item.chaId === character.chaId)
            if (index >= 0) current.characters[index] = carryCatalogCharacterMetadata(current.characters[index], character)
        },
        publishConversation: (characterId: string, conversation: Database['characters'][number]['chats'][number], nextCharacter?: Database['characters'][number]) => {
            const character = current.characters.find((item) => item.chaId === characterId)!
            if (nextCharacter) state.publishCharacter(nextCharacter)
            else character.chats[character.chatPage] = conversation
        },
    }
    return state as unknown as PersistentDataRuntimeStateAdapter & {
        current(): Database
        generating: { characterId: string; conversationId: string } | null
        operationActive: boolean
        operationCheck?: () => void
        windowedAllowed: boolean
        endGeneration(): void
    }
}

async function makeRuntime(name: string) {
    const raw = new IndexedDbPersistentDataStore(name, indexedDB, IDBKeyRange)
    await raw.open()
    const database = makeDatabase('Initial')
    await raw.replaceFromDatabase(database)
    const scripted = withScriptedChangeWindow(raw)
    const state = makeState(database)
    const errors = vi.fn()
    const runtime = createPersistentDataRuntime({
        store: scripted.store,
        state,
        onBackgroundError: errors,
        prepareDatabase: async (candidate) => candidate,
    })
    await runtime.initializeActiveWorkingSet(database)
    return { runtime, state, store: raw, scripted, errors }
}

async function refresh(
    runtime: Awaited<ReturnType<typeof makeRuntime>>['runtime'],
    revision: number,
): Promise<void> {
    const fence = await runtime.acquireCommittedWorkingSetRefreshFence()
    try {
        await fence.refreshCommittedWorkingSet(revision)
    } finally {
        fence.release()
    }
}

describe('the working-set refresh drives the content change cursor', () => {
    it('keeps windowed authority through root and conversation refreshes without loading full history', async () => {
        const { runtime, state, store, scripted } = await makeRuntime(`targeted-windowed-${crypto.randomUUID()}`)
        const database = makeDatabase('Projected')
        database.characters[0].chats[0].message = Array.from({ length: 1500 }, (_, index) => ({
            role: index % 2 ? 'char' as const : 'user' as const, data: `Synthetic ${index}`, chatId: `row-${index}`,
        }))
        await store.replaceFromDatabase(database, 1)
        scripted.script(null, [])
        await refresh(runtime, 2)
        state.windowedAllowed = true
        expect(await runtime.tryDemoteSelectedConversation()).toBe(true)
        expect(getCatalogCharacterMetadata(state.current().characters[0])).toBeDefined()
        const oldAuthority = runtime.captureSelectedConversationAuthority()!
        const root = await store.readRoot()
        await store.commit({ expectedRevision: 2, root: { ...root.value, username: 'Updated' } })
        scripted.script({ revision: 3, afterRevision: 2 }, [{ kind: 'root', key1: '', key2: '' }])
        scripted.resetCounts()
        await refresh(runtime, 3)
        expect(runtime.captureSelectedConversationAuthority()).toMatchObject({ storeRevision: 3, totalMessages: 1500 })
        expect(isMetadataOnlySelectedConversation(state.current().characters[0].chats[0])).toBe(true)
        expect(scripted.readConversationCalls()).toBe(0)
        expect(scripted.queryCharacterCalls()).toBe(0)
        expect(runtime.captureSelectedConversationAuthority()!.sessionToken).not.toBe(oldAuthority.sessionToken)
        await store.commit({ expectedRevision: 3, conversations: [{ type: 'replace-range', characterId: 'char-a',
            conversationId: 'chat-a', start: 1500, deleteCount: 0,
            messages: [{ role: 'char', data: 'Remote appended', chatId: 'remote-appended' }],
        }] })
        scripted.script({ revision: 4, afterRevision: 3 }, [
            { kind: 'character', key1: 'char-a', key2: '' },
            { kind: 'conversation', key1: 'char-a', key2: 'chat-a' },
        ])
        await refresh(runtime, 4)
        expect(runtime.captureSelectedConversationAuthority()).toMatchObject({ storeRevision: 4, totalMessages: 1501 })
        expect(scripted.readConversationCalls()).toBe(0)
        const controller = await runtime.captureWindowedMessageMutation(runtime.captureSelectedConversationTarget()!, 1498,
            { role: 'user', data: 'Synthetic 1498', chatId: 'row-1498' })
        expect(controller).not.toBeNull()
        expect(controller!.applyRange(0, 1, [{ role: 'user', data: 'Edited after refresh', chatId: 'row-1498' }], 'edit')).toBe(true)
        controller!.release()
        await runtime.flushPendingData('test-windowed-refresh-edit')
        const saved = await store.readConversationWindow({ characterId: 'char-a', conversationId: 'chat-a', startIndex: 1498, limit: 1 })
        expect(saved!.value.messages[0].data).toBe('Edited after refresh')
    })

    it('realigns the cursor with the initial projection of a recreated WebView', async () => {
        const { scripted } = await makeRuntime(`targeted-boot-${crypto.randomUUID()}`)
        expect(scripted.cursors).toEqual([1])
    })

    it('reprojects and advances the cursor when the window asks for a rebuild', async () => {
        const { runtime, state, store, scripted } = await makeRuntime(
            `targeted-rebuild-${crypto.randomUUID()}`,
        )
        await store.replaceFromDatabase(makeDatabase('Remote winner'), 1)
        scripted.script(null, [])
        scripted.resetCounts()
        await refresh(runtime, 2)

        expect(state.current().username).toBe('Remote winner')
        expect(scripted.queryCharacterCalls()).toBeGreaterThan(0)
        expect(scripted.cursors).toEqual([1, 2])
    })

    it('applies a bounded window without walking the character catalog', async () => {
        const { runtime, state, store, scripted } = await makeRuntime(
            `targeted-window-${crypto.randomUUID()}`,
        )
        await store.replaceFromDatabase(makeDatabase('Projected'), 1)
        scripted.script(null, [])
        await refresh(runtime, 2)

        const root = await store.readRoot()
        await store.commit({
            expectedRevision: 2,
            root: { ...root.value, username: 'Targeted' },
        })
        scripted.script({ revision: 3, afterRevision: 2 }, [
            { kind: 'root', key1: '', key2: '' },
        ])
        scripted.resetCounts()
        await refresh(runtime, 3)

        expect(state.current().username).toBe('Targeted')
        expect(scripted.queryCharacterCalls()).toBe(0)
        expect(scripted.cursors).toEqual([1, 2, 3])
    })

    it('holds the cursor while a change lands on a generating conversation', async () => {
        const { runtime, state, store, scripted } = await makeRuntime(
            `targeted-deferred-${crypto.randomUUID()}`,
        )
        await store.replaceFromDatabase(makeDatabase('Projected'), 1)
        scripted.script(null, [])
        await refresh(runtime, 2)

        await store.commit({
            expectedRevision: 2,
            conversations: [
                {
                    type: 'replace-range',
                    characterId: 'char-a',
                    conversationId: 'chat-a',
                    start: 0,
                    deleteCount: 0,
                    messages: [{ role: 'char', data: 'remote', chatId: 'remote-1' }],
                } as never,
            ],
        })
        state.operationActive = true
        state.generating = { characterId: 'char-a', conversationId: 'chat-a' }
        scripted.script({ revision: 3, afterRevision: 2 }, [
            { kind: 'character', key1: 'char-a', key2: '' },
            { kind: 'conversation', key1: 'char-a', key2: 'chat-a' },
        ])
        await refresh(runtime, 3)

        expect(scripted.cursors).toEqual([1, 2])
        const selected = state
            .current()
            .characters.find((character) => character.chaId === 'char-a')!
        expect(selected.chats[0].message).toEqual([])
    })

    it('persists the generated reply before it applies the held change', async () => {
        const { runtime, state, store, scripted } = await makeRuntime(
            `targeted-resume-${crypto.randomUUID()}`,
        )
        await store.replaceFromDatabase(makeDatabase('Projected'), 1)
        scripted.script(null, [])
        await refresh(runtime, 2)

        await store.commit({
            expectedRevision: 2,
            conversations: [
                {
                    type: 'replace-range',
                    characterId: 'char-a',
                    conversationId: 'chat-a',
                    start: 0,
                    deleteCount: 0,
                    messages: [{ role: 'char', data: 'remote', chatId: 'remote-1' }],
                } as never,
            ],
        })
        state.operationActive = true
        state.generating = { characterId: 'char-a', conversationId: 'chat-a' }
        scripted.script({ revision: 3, afterRevision: 2 }, [
            { kind: 'character', key1: 'char-a', key2: '' },
            { kind: 'conversation', key1: 'char-a', key2: 'chat-a' },
        ])
        await refresh(runtime, 3)
        expect(scripted.cursors).toEqual([1, 2])

        // The reply the generation produced is still only in the working set.
        state.current().username = 'Generated locally'
        runtime.markPersistentDataDirty(10)
        scripted.script({ revision: 4, afterRevision: 2 }, [
            { kind: 'character', key1: 'char-a', key2: '' },
            { kind: 'conversation', key1: 'char-a', key2: 'chat-a' },
        ])
        state.endGeneration()
        await vi.waitFor(() => expect(scripted.cursors.length).toBe(3))

        expect((await store.readRoot()).value.username).toBe('Generated locally')
        const selected = state
            .current()
            .characters.find((character) => character.chaId === 'char-a')!
        expect(selected.chats[0].message).toEqual([
            { role: 'char', data: 'remote', chatId: 'remote-1' },
        ])
    })

    it('retains held content when a second generation races deferred refresh and retries after it ends', async () => {
        const { runtime, state, store, scripted, errors } = await makeRuntime(`targeted-contention-${crypto.randomUUID()}`)
        await store.replaceFromDatabase(makeDatabase('Projected'), 1)
        scripted.script(null, [])
        await refresh(runtime, 2)
        await store.commit({ expectedRevision: 2, conversations: [{
            type: 'replace-range', characterId: 'char-a', conversationId: 'chat-a',
            start: 0, deleteCount: 0, messages: [{ role: 'char', data: 'held', chatId: 'held-1' }],
        } as never] })
        state.operationActive = true
        state.generating = { characterId: 'char-a', conversationId: 'chat-a' }
        scripted.script({ revision: 3, afterRevision: 2 }, [
            { kind: 'conversation', key1: 'char-a', key2: 'chat-a' },
        ])
        await refresh(runtime, 3)
        expect(scripted.cursors).toEqual([1, 2])
        let observedBlock!: () => void
        const blocked = new Promise<void>((resolve) => { observedBlock = resolve })
        state.operationCheck = () => {
            if (state.operationActive) observedBlock()
        }
        state.endGeneration()
        state.operationActive = true
        await blocked
        await runtime.flushPendingDataLocally('settle-contention')
        expect(errors).not.toHaveBeenCalled()
        expect(scripted.cursors).toEqual([1, 2])
        state.generating = null
        state.endGeneration()
        await vi.waitFor(() => expect(scripted.cursors).toEqual([1, 2, 3]))
        expect(state.current().characters[0].chats[0].message).toEqual([
            { role: 'char', data: 'held', chatId: 'held-1' },
        ])
        expect(errors).not.toHaveBeenCalled()
    })

    it('reprojects and advances the cursor when a targeted pass fails', async () => {
        const { runtime, state, store, scripted } = await makeRuntime(
            `targeted-failure-${crypto.randomUUID()}`,
        )
        await store.replaceFromDatabase(makeDatabase('Projected'), 1)
        scripted.script(null, [])
        await refresh(runtime, 2)

        const root = await store.readRoot()
        await store.commit({
            expectedRevision: 2,
            root: { ...root.value, username: 'Recovered' },
        })
        scripted.script({ revision: 3, afterRevision: 2 }, [
            { kind: 'character', key1: 'char-a', key2: '' },
            { kind: 'root', key1: '', key2: '' },
        ])
        scripted.resetCounts()
        scripted.failTargetedReads(true)
        await refresh(runtime, 3)

        expect(state.current().username).toBe('Recovered')
        expect(scripted.queryCharacterCalls()).toBeGreaterThan(0)
        expect(scripted.cursors).toEqual([1, 2, 3])
    })
})
