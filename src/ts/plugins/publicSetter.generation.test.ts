import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest'

const fixture = vi.hoisted(() => ({
    store: null as any,
    replacers: new Set<(formated: unknown[], model: string) => Promise<unknown[]>>(),
    modelRequests: 0,
    historyLimit: false,
    replyGate: Promise.resolve(),
    onRequest: () => {},
    providerReturned: false,
    outputs: new Set<(event: any) => Promise<void>>(),
}))

// The synthetic database already supplies the generation fields; import migration is outside this fixture.
vi.mock('src/ts/storage/databasePreparation', async (importOriginal) => ({
    ...await importOriginal<typeof import('../storage/databasePreparation')>(),
    prepareDatabaseForPersistence: async (input: unknown) => structuredClone(input),
}))
vi.mock('src/ts/storage/deviceSettings', async (importOriginal) => {
    const original = await importOriginal<typeof import('../storage/deviceSettings')>()
    return { ...original, getDeviceSettings: () => ({
        ...original.getDeviceSettings(), generationHistoryLimitEnabled: fixture.historyLimit,
        generationHistoryLimitMultiplier: 2,
    }) }
})
vi.mock('src/ts/storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: () => fixture.store }))
vi.mock('src/ts/parser/parser.svelte', () => ({
    assetRegex: /$^/,
    hasher: vi.fn(async () => 'hash'),
    parseMarkdownSafe: (value: string) => value,
    ParseMarkdown: vi.fn(async (value: string) => value),
    risuChatParser: (value: string) => value,
}))
vi.mock('src/ts/tokenizer', async () => (await import('../process/tests/sendChatTestHarness')).tokenizerModule({
    tokenize: vi.fn(async () => 10),
    encodeWithTokenizer: vi.fn(async () => new Array(10).fill(0)),
}))
vi.mock('src/lang', async () => (await import('../process/tests/sendChatTestHarness')).langModule())
vi.mock('src/ts/alert', async () => (await import('../process/tests/sendChatTestHarness')).alertModule())
vi.mock('src/ts/parser/chatML', async () => (await import('../process/tests/sendChatTestHarness')).chatMLModule())
vi.mock('src/ts/process/lorebook.svelte', async () => (await import('../process/tests/sendChatTestHarness')).lorebookModule())
// The provider request: `beforeRequest` replacers run where `requestChatData` runs them, then the reply.
vi.mock('src/ts/process/request/request', () => ({
    requestChatData: vi.fn(async (request: { formated: unknown[] }, purpose: string) => {
        if (purpose === 'emotion') return '|igp'
        fixture.modelRequests += 1
        fixture.onRequest()
        for (const replacer of fixture.replacers) request.formated = await replacer(request.formated, purpose)
        await fixture.replyGate
        fixture.providerReturned = true
        return {
            type: 'streaming',
            result: new ReadableStream<Record<string, string>>({
                start(controller) {
                    controller.enqueue({ response: 'answer' })
                    controller.close()
                },
            }),
        }
    }),
}))
vi.mock('src/ts/process/stableDiff', async () => (await import('../process/tests/sendChatTestHarness')).stableDiffModule())
vi.mock('src/ts/process/scripts', async () => (await import('../process/tests/sendChatTestHarness')).scriptsModule())
vi.mock('src/ts/process/templates/templates', async () => (await import('../process/tests/sendChatTestHarness')).templatesModule())
vi.mock('src/ts/process/exampleMessages', async () => (await import('../process/tests/sendChatTestHarness')).exampleMessagesModule())
vi.mock('src/ts/process/tts', async () => (await import('../process/tests/sendChatTestHarness')).ttsModule())
vi.mock('src/ts/process/memory/supaMemory', async () => (await import('../process/tests/sendChatTestHarness')).supaMemoryModule())
// A character without trigger scripts: the real `runTrigger` returns null for every mode.
vi.mock('src/ts/process/triggers', () => ({ runTrigger: vi.fn(async () => null) }))
vi.mock('src/ts/process/memory/hypamemory', async () => (await import('../process/tests/sendChatTestHarness')).hypamemoryModule())
vi.mock('src/ts/process/embedding/addinfo', async () => (await import('../process/tests/sendChatTestHarness')).addinfoModule())
vi.mock('src/ts/process/files/inlays', async () => (await import('../process/tests/sendChatTestHarness')).inlaysModule())
vi.mock('src/ts/process/models/modelString', async () => (await import('../process/tests/sendChatTestHarness')).modelStringModule())
vi.mock('src/ts/process/inlayScreen', () => ({ runInlayScreen: (_char: unknown, data: string) => ({ text: data }) }))
vi.mock('src/ts/process/transformers', async () => (await import('../process/tests/sendChatTestHarness')).transformersModule())
vi.mock('src/ts/process/memory/hanuraiMemory', () => ({
    hanuraiMemory: vi.fn(async (chats: unknown, { currentTokens }: { currentTokens: number }) => ({ chats, tokens: currentTokens })),
}))
vi.mock('src/ts/process/memory/hypav2', () => ({
    hypaMemoryV2: vi.fn(async (chats: unknown, currentTokens: number) => ({ chats, currentTokens })),
}))
vi.mock('src/ts/process/memory/hypav3', () => ({
    getCurrentHypaV3Preset: () => ({ settings: {
        preserveOrphanedMemory: false, useExperimentalImpl: false,
        recentMemoryRatio: 0, similarMemoryRatio: 0, queryChatCount: 1,
    } }),
    hypaMemoryV3: vi.fn(async (chats: unknown[], currentTokens: number, _max: number, room: any) => ({
        chats, currentTokens, memory: room.hypaV3Data,
    })),
}))
vi.mock('src/ts/process/scriptings', async () => (await import('../process/tests/sendChatTestHarness')).scriptingsModule())
vi.mock('src/ts/model/modellist', async (importOriginal) => ({
    ...await importOriginal<typeof import('../model/modellist')>(),
    ...(await import('../process/tests/sendChatTestHarness')).modellistModule(),
}))
vi.mock('src/ts/process/modules', async () => (await import('../process/tests/sendChatTestHarness')).modulesModule())
vi.mock('src/ts/globalApi.svelte', async () => (await import('../process/tests/sendChatTestHarness')).globalApiModule({ forageStorage: {} }))
vi.mock('src/ts/plugins/plugins.svelte', () => ({
    pluginV2: { chatOutput: fixture.outputs, editprocess: new Set(), replacerbeforeRequest: fixture.replacers, replacerafterRequest: new Set() },
}))
vi.mock('src/ts/process/presetChain', async () => (await import('../process/tests/sendChatTestHarness')).presetChainModule())

import type { Chat, Database, Message } from '../storage/database.svelte'
import { getDatabase, setDatabaseLite } from '../storage/database.svelte'
import { IndexedDbPersistentDataStore } from '../storage/indexedDbPersistentDataStore'
import {
    acquireCompleteConversation,
    activateCharacter,
    assertPersistentMutationAllowed,
    captureSelectedConversationAuthority,
    captureSelectedConversationTarget,
    commitPersistentUnitIntent,
    flushPendingDataLocally,
    getActiveConversationSession,
    getPersistentNavigationGeneration,
    getPersistentRevision,
    getPersistentStorageAuthorityEpoch,
    initializeActiveWorkingSet,
    invalidateActiveConversationSession,
    refreshSelectedConversationAfterReplacement,
    replacePersistentDatabase,
} from '../storage/persistentDataRuntime.svelte'
import { selectedCharID } from '../stores.svelte'
import { doingChat, getSelectedBoundedGenerationFallbackReason, sendChat } from '../process/index.svelte'
import { createProductionPluginDatabaseAccess, type PluginDatabaseAccess } from './pluginDatabaseAccess'
import { findActiveHistoryWindow } from '../process/historyWindowIndex'
import { get } from 'svelte/store'

const target = { characterId: 'character-a', conversationId: 'chat-a' }
const factory = new IDBFactory()
const storeName = `public-setter-generation-${crypto.randomUUID()}`
function database(): Database {
    const chat = {
        id: target.conversationId,
        name: 'Chat A',
        note: '',
        localLore: [],
        fmIndex: -1,
        message: [{ role: 'user', data: 'hello', chatId: 'user-message' }] as Message[],
        scriptstate: {},
    }
    return {
        characters: [{
            type: 'character', chaId: target.characterId, name: 'Character A', chatPage: 0, chats: [chat],
            firstMessage: '', alternateGreetings: [''], desc: '', personality: '', scenario: '', bias: [],
            additionalAssets: [], emotionImages: [], triggerscript: [], defaultVariables: '', reloadKeys: 0,
            viewScreen: 'none', inlayViewScreen: false, supaMemory: false,
        }],
        statics: { messages: 0 },
        botPresets: [],
        botPresetsId: 0,
        aiModel: 'test-model',
        maxContext: 8_192,
        maxResponse: 128,
        promptTemplate: [{ type: 'chat', rangeStart: 0, rangeEnd: 'end' }],
        promptSettings: { trimStartNewChat: true, sendName: false, sendChatAsSystem: false, postEndInnerFormat: '' },
        promptInfoInsideChat: false,
        promptTextInfoInsideChat: false,
        customPromptTemplateToggle: '',
        globalChatVariables: {},
        mainPrompt: '',
        additionalPrompt: '',
        globalNote: '',
        jailbreak: '',
        jailbreakToggle: false,
        chainOfThought: false,
        personaPrompt: false,
        promptPreprocess: false,
        descriptionPrefix: '',
        formatingOrder: [],
        bias: [],
        outputImageModal: false,
        rememberToolUsage: false,
        removeIncompleteResponse: false,
        streamingDisplayOptimizationMode: 'off',
        autoContinueMinTokens: 0,
        autoContinueChat: false,
        igpPrompt: '',
        notification: false,
        ttsAutoSpeech: false,
        supaModelType: 'none',
        hanuraiEnable: false,
        hypav2: false,
        hypaV3: false,
        inlayErrorResponse: false,
        plugins: [],
    } as unknown as Database
}

function deferred() {
    let resolve!: () => void
    const promise = new Promise<void>(done => { resolve = done })
    return { promise, resolve }
}

function createAccess(flushPendingData = flushPendingDataLocally): PluginDatabaseAccess {
    return createProductionPluginDatabaseAccess({
        owner: 'public-setter-fixture',
        getPersistentRevision,
        commitPersistentUnitIntent,
        flushPendingData,
        assertPersistentMutationAllowed,
        getStorageAuthorityEpoch: getPersistentStorageAuthorityEpoch,
        getCompatibilityDatabase: getDatabase,
        getSelectedCharacterId: () => getDatabase().characters[get(selectedCharID)]?.chaId ?? null,
        captureSelectedConversationTarget,
        acquireCompleteConversation,
        refreshSelectedConversationAfterReplacement,
        invalidateActiveConversationSession,
        reportIdentityReplacementRejected: vi.fn(),
        getNavigationGeneration: getPersistentNavigationGeneration,
        readPluginStorageSnapshot: async () => ({}),
        snapshot: <T>(value: T) => JSON.parse(JSON.stringify(value)) as T,
    })
}

async function install(initial: Database) {
    if (!fixture.store) {
        fixture.store = new IndexedDbPersistentDataStore(storeName, factory, IDBKeyRange)
        await fixture.store.open()
        await fixture.store.replaceFromDatabase(initial)
    } else {
        selectedCharID.set(0)
        const lease = await acquireCompleteConversation('public-setter-reset', captureSelectedConversationTarget())
        try { await replacePersistentDatabase(initial, 'public-setter-reset') }
        finally { lease.release() }
    }
    setDatabaseLite(initial)
    selectedCharID.set(0)
    await initializeActiveWorkingSet(getDatabase())
}

async function reopen(): Promise<Chat> {
    await flushPendingDataLocally('public-setter-generation-test')
    const reopened = new IndexedDbPersistentDataStore(storeName, factory, IDBKeyRange)
    await reopened.open()
    return (await reopened.readConversation(target.characterId, target.conversationId))!.value
}

type Mode = 'history-limit' | 'complete' | 'standard-hypa'
type Edit = 'promotion' | 'metadata' | 'content' | 'insert' | 'remove' | 'duplicate history IDs' | 'missing history ID'

describe.each<Mode>(['history-limit', 'complete', 'standard-hypa'])('public chat setter during %s generation', mode => {
    let access: PluginDatabaseAccess
    let provider: ReturnType<typeof deferred>
    let started: ReturnType<typeof deferred>
    let initialChat: Chat
    let output: Mock<(event: any) => Promise<void>>
    let log: ReturnType<typeof vi.spyOn>
    let debug: ReturnType<typeof vi.spyOn>
    let releaseCommit: (() => void) | undefined
    let restoreCursor: (() => void) | undefined
    let lifetime: AbortController
    let running: Promise<boolean> | undefined
    let pendingWrite: Promise<void> | undefined
    const generate = () => running = sendChat({ historyLimit: fixture.historyLimit, signal: lifetime.signal })
    const holdPublicFlush = () => {
        const entered = deferred()
        const released = deferred()
        releaseCommit = released.resolve
        access.closeReadBaselines()
        access = createAccess(async reason => {
            entered.resolve()
            await released.promise
            await flushPendingDataLocally(reason)
        })
        return { entered: entered.promise, release: released.resolve }
    }

    beforeEach(async () => {
        log = vi.spyOn(console, 'log').mockImplementation(() => undefined)
        debug = vi.spyOn(console, 'debug').mockImplementation(() => undefined)
        fixture.historyLimit = mode === 'history-limit'
        fixture.replacers.clear()
        fixture.outputs.clear()
        fixture.modelRequests = 0
        fixture.providerReturned = false
        lifetime = new AbortController()
        running = undefined
        pendingWrite = undefined
        provider = deferred()
        started = deferred()
        fixture.replyGate = provider.promise
        output = vi.fn(async () => undefined)
        const initial = database()
        initialChat = initial.characters[0].chats[0]
        initialChat.message = [
            { role: 'user', data: 'covered-a', chatId: 'a' },
            { role: 'char', data: 'covered-b', chatId: 'b' },
            { role: 'user', data: 'tail', chatId: 'c' },
        ]
        if (mode === 'standard-hypa') {
            initial.hypaV3 = true
            initial.characters[0].supaMemory = true
            initialChat.hypaV3Data = { summaries: [{ chatMemos: ['a', 'b'], summary: 'covered' }] } as any
        }
        initial.characters.push({
            ...structuredClone(initial.characters[0]), chaId: 'character-b', name: 'Character B',
            chats: [{ ...structuredClone(initialChat), id: 'chat-b', message: [{ role: 'user', data: 'Other conversation', chatId: 'other' }] }],
        })
        await install(initial)
        access = createAccess()
        fixture.onRequest = () => {
            started.resolve()
            if (mode === 'complete') expect(getActiveConversationSession()?.isActive).toBe(true)
            else {
                expect(getActiveConversationSession()).toBeNull()
                expect(captureSelectedConversationAuthority()?.totalMessages).toBe(3)
            }
            fixture.outputs.add(output)
        }
        if (mode === 'standard-hypa') expect(getSelectedBoundedGenerationFallbackReason()).toBeNull()
    })

    afterEach(async () => {
        releaseCommit?.()
        releaseCommit = undefined
        provider.resolve()
        lifetime.abort()
        await Promise.allSettled([running, pendingWrite])
        restoreCursor?.()
        restoreCursor = undefined
        fixture.replacers.clear()
        fixture.outputs.clear()
        access.closeReadBaselines()
        doingChat.set(false)
        selectedCharID.set(-1)
        log.mockRestore()
        debug.mockRestore()
        vi.restoreAllMocks()
    })

    it.each<Edit>(['promotion', 'metadata', 'content', 'insert', 'remove', 'duplicate history IDs', 'missing history ID'])(
        'keeps an awaited %s write and one reply after reopening the store',
        async edit => {
            const submitted = structuredClone(initialChat)
            if (edit === 'metadata') {
                submitted.note = 'Plugin note'
                submitted.message[0] = { ...submitted.message[0], __public: 'metadata' } as Message
                submitted.scriptstate = { $public: 'saved' }
            }
            if (edit === 'content') submitted.message[0].data = 'Plugin content'
            if (edit === 'insert') submitted.message.splice(1, 0, { role: 'user', data: 'Plugin insert', chatId: 'inserted' })
            if (edit === 'remove') submitted.message.splice(0, 1)
            if (edit === 'duplicate history IDs') submitted.message[0].chatId = submitted.message[1].chatId
            if (edit === 'missing history ID') delete submitted.message[0].chatId
            let writing: Promise<void> | undefined
            fixture.replacers.add(async formated => {
                writing = pendingWrite = access.setChatToIndex(0, 0, submitted, { pluginName: 'fixture', signal: lifetime.signal })
                await writing
                return formated
            })

            const sending = generate()
            await started.promise
            provider.resolve()
            await expect(sending).resolves.toBe(true)
            await expect(writing).resolves.toBeUndefined()

            const stored = await reopen()
            expect(stored.message.map(message => message.data)).toEqual([...submitted.message.map(message => message.data), 'answer'])
            expect(stored.message.slice(0, -1).map(message => message.chatId)).toEqual(submitted.message.map(message => message.chatId))
            expect(stored.note).toBe(submitted.note)
            expect(stored.scriptstate).toEqual(submitted.scriptstate)
            if (edit === 'metadata') expect(stored.message[0]).toMatchObject({ __public: 'metadata' })
            expect(fixture.modelRequests).toBe(1)
            expect(output).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
                characterIndex: 0, chatIndex: 0, messageIndex: submitted.message.length,
            }))
        },
    )

    it('waits for an unawaited admitted setter before applying the reply', async () => {
        const committed = deferred()
        const commitEntered = deferred()
        releaseCommit = committed.resolve
        const originalCommit = fixture.store.commit.bind(fixture.store)
        vi.spyOn(fixture.store, 'commit').mockImplementation(async (input: unknown) => {
            if (JSON.stringify(input).includes('Held public setter')) {
                commitEntered.resolve()
                await committed.promise
            }
            return originalCommit(input)
        })
        const submitted = structuredClone(initialChat)
        submitted.note = 'Held public setter'
        submitted.message.splice(1, 0, { role: 'user', data: 'Unawaited insert', chatId: 'unawaited' })
        let writing: Promise<void> | undefined
        fixture.replacers.add(async formated => {
            writing = pendingWrite = access.setChatToIndex(0, 0, submitted, { pluginName: 'fixture', signal: lifetime.signal })
            void writing.catch(() => undefined)
            return formated
        })
        let settled = false
        const sending = generate().finally(() => { settled = true })
        await started.promise
        await commitEntered.promise
        provider.resolve()
        await vi.waitFor(() => expect(fixture.providerReturned).toBe(true))
        expect(settled).toBe(false)
        expect(output).not.toHaveBeenCalled()
        committed.resolve()
        await expect(writing).resolves.toBeUndefined()
        await expect(sending).resolves.toBe(true)
        const stored = await reopen()
        expect(stored.note).toBe('Held public setter')
        expect(stored.message.map(message => message.data)).toEqual(['covered-a', 'Unawaited insert', 'covered-b', 'tail', 'answer'])
        expect(output).toHaveBeenCalledOnce()
    })

    it.each([
        ...['before-projection', 'lease-release', 'cursor'].flatMap(phase => [true, false].map(awaited => ({ phase, awaited, change: 'edit' }))),
        ...[true, false].map(awaited => ({ phase: 'cursor', awaited, change: 'metadata' })),
    ])('respects projection ownership at $phase after $change (awaited=$awaited)', async ({ phase, awaited, change }) => {
        const projected = deferred()
        const releaseProjection = deferred()
        releaseCommit = releaseProjection.resolve
        const originalCursor = fixture.store.commitWorkingSetChangeCursor
        restoreCursor = () => {
            if (originalCursor) fixture.store.commitWorkingSetChangeCursor = originalCursor
            else delete fixture.store.commitWorkingSetChangeCursor
        }
        let held = false
        const hold = async () => {
            held = true
            projected.resolve()
            await releaseProjection.promise
        }
        fixture.store.commitWorkingSetChangeCursor = async (revision: number) => {
            if (phase === 'cursor' && !held && getDatabase().characters[0].chats[0].note === 'Owned projection') await hold()
            await originalCursor?.call(fixture.store, revision)
        }
        if (phase === 'before-projection') {
            const originalCommit = fixture.store.commit.bind(fixture.store)
            vi.spyOn(fixture.store, 'commit').mockImplementation(async (input: unknown) => {
                const result = await originalCommit(input)
                if (!held && JSON.stringify(input).includes('Owned projection')) await hold()
                return result
            })
        } else if (phase === 'lease-release') {
            const originalAcquire = fixture.store.acquireRevision.bind(fixture.store)
            vi.spyOn(fixture.store, 'acquireRevision').mockImplementation(async (revision: number) => {
                const lease = await originalAcquire(revision)
                const release = lease.release.bind(lease)
                lease.release = async () => {
                    if (!held && getDatabase().characters[0].chats[0].note === 'Owned projection') await hold()
                    await release()
                }
                return lease
            })
        }
        const submitted = structuredClone(initialChat)
        submitted.note = 'Owned projection'
        if (phase !== 'before-projection') submitted.message[0].data = 'Public content'
        fixture.replacers.add(async formated => {
            pendingWrite = access.setChatToIndex(0, 0, submitted, { pluginName: 'fixture', signal: lifetime.signal })
            if (awaited) await pendingWrite
            else void pendingWrite.catch(() => undefined)
            return formated
        })
        const sending = generate()
        await projected.promise
        const session = getActiveConversationSession()!
        const before = session.generationInvalidationVersion
        if (change === 'edit') {
            session.replaceRange(session.positionAt(2), 1, [{ ...initialChat.message[2], data: 'Unrelated edit' }])
            expect(session.generationInvalidationVersion).toBeGreaterThan(before)
        } else {
            const version = session.version
            const persisted = JSON.parse(JSON.stringify(getDatabase().characters[0].chats[0])) as Chat
            expect(session.adoptPersistedMetadata(persisted, session.storeRevision)).toBe(true)
            expect(session.version).toBeGreaterThan(version)
            expect(session.generationInvalidationVersion).toBe(before)
        }
        releaseProjection.resolve()
        provider.resolve()
        await expect(pendingWrite).resolves.toBeUndefined()
        await expect(sending).resolves.toBe(change === 'metadata')
        const stored = await reopen()
        expect(stored.note).toBe('Owned projection')
        expect(stored.message.map(message => message.data)).toEqual(change === 'edit'
            ? [submitted.message[0].data, 'covered-b', 'Unrelated edit']
            : [submitted.message[0].data, 'covered-b', 'tail', 'answer'])
        expect(output).toHaveBeenCalledTimes(change === 'metadata' ? 1 : 0)
    })

    it('rejects a conversation identity replacement without losing the pending reply', async () => {
        fixture.replacers.add(async formated => {
            await expect(access.setChatToIndex(0, 0, { ...structuredClone(initialChat), id: 'different-chat' }, {
                pluginName: 'fixture', signal: new AbortController().signal,
            })).rejects.toThrow('identity')
            return formated
        })
        const sending = generate()
        await started.promise
        provider.resolve()
        await expect(sending).resolves.toBe(true)
        const stored = await reopen()
        expect(stored.id).toBe(target.conversationId)
        expect(stored.message.map(message => message.data)).toEqual(['covered-a', 'covered-b', 'tail', 'answer'])
        expect(output).toHaveBeenCalledOnce()
    })

    it('keeps invalid indices as no-ops inside an awaited beforeRequest replacer', async () => {
        fixture.replacers.add(async formated => {
            const submitted = { ...structuredClone(initialChat), note: 'Must not be stored' }
            for (const [characterIndex, chatIndex] of [[-1, 0], [99, 0], [0, -1], [0, 99], [0.5, 0], [0, Number.NaN]]) {
                await expect(access.setChatToIndex(characterIndex, chatIndex, submitted, {
                    pluginName: 'fixture', signal: lifetime.signal,
                })).resolves.toBeUndefined()
            }
            return formated
        })
        const sending = generate()
        await started.promise
        provider.resolve()
        await expect(sending).resolves.toBe(true)
        const stored = await reopen()
        expect(stored.note).toBe(initialChat.note)
        expect(stored.message.map(message => message.data)).toEqual(['covered-a', 'covered-b', 'tail', 'answer'])
        expect(output).toHaveBeenCalledOnce()
    })

    it.each(['navigation', 'cancellation'])(
        'keeps request ownership across %s while a beforeRequest setter is waiting to flush',
        async boundary => {
            const flush = holdPublicFlush()
            fixture.replacers.add(async formated => {
                pendingWrite = access.setChatToIndex(0, 0, { ...structuredClone(initialChat), note: 'Admitted plugin note' }, {
                    pluginName: 'fixture', signal: lifetime.signal,
                })
                void pendingWrite.catch(() => undefined)
                return formated
            })
            const sending = generate()
            await flush.entered
            if (boundary === 'navigation') {
                await expect(activateCharacter('character-b')).resolves.toBe(false)
                expect(getDatabase().characters[get(selectedCharID)].chaId).toBe(target.characterId)
            } else lifetime.abort()
            flush.release()
            provider.resolve()
            if (boundary === 'cancellation') await expect(pendingWrite).rejects.toThrow()
            else await expect(pendingWrite).resolves.toBeUndefined()
            await expect(sending).resolves.toBe(boundary === 'navigation')
            const stored = await reopen()
            expect(stored.note).toBe(boundary === 'navigation' ? 'Admitted plugin note' : initialChat.note)
            expect(stored.message.map(message => message.data)).toEqual(boundary === 'navigation'
                ? ['covered-a', 'covered-b', 'tail', 'answer']
                : ['covered-a', 'covered-b', 'tail'])
            const other = await fixture.store.readConversation('character-b', 'chat-b')
            expect(other.value.message.map((message: Message) => message.data)).toEqual(['Other conversation'])
            if (boundary === 'navigation') expect(output).toHaveBeenCalledOnce()
            else expect(output).not.toHaveBeenCalled()
        },
    )

    it('preserves unrelated committed fields while an awaited beforeRequest setter is waiting to flush', async () => {
        const flush = holdPublicFlush()
        fixture.replacers.add(async formated => {
            pendingWrite = access.setChatToIndex(0, 0, { ...structuredClone(initialChat), name: 'Plugin title' }, {
                pluginName: 'fixture', signal: lifetime.signal,
            })
            await pendingWrite
            return formated
        })
        const sending = generate()
        await flush.entered
        await commitPersistentUnitIntent('public-setter-concurrent-metadata', [
            { key: '["conversation","character-a","chat-a","note"]', type: 'set', value: 'Concurrent note' },
            { key: '["conversation","character-b","chat-b","name"]', type: 'set', value: 'Other title' },
        ])
        flush.release()
        provider.resolve()
        await expect(pendingWrite).resolves.toBeUndefined()
        await expect(sending).resolves.toBe(true)
        const stored = await reopen()
        expect(stored.name).toBe('Plugin title')
        expect(stored.note).toBe('Concurrent note')
        expect(stored.message.map(message => message.data)).toEqual(['covered-a', 'covered-b', 'tail', 'answer'])
        const other = await fixture.store.readConversation('character-b', 'chat-b')
        expect(other.value.name).toBe('Other title')
        expect(other.value.message.map((message: Message) => message.data)).toEqual(['Other conversation'])
        expect(output).toHaveBeenCalledOnce()
    })

    if (mode === 'history-limit') it('keeps the nonzero admitted window registered after an awaited public metadata write', async () => {
        const seed = database()
        seed.maxContext = 500
        seed.maxResponse = 0
        initialChat = seed.characters[0].chats[0]
        initialChat.message = Array.from({ length: 120 }, (_, index): Message => ({
            role: index % 2 ? 'char' : 'user', data: `Synthetic message ${index}`, chatId: `message-${index}`,
        }))
        access.closeReadBaselines()
        await install(seed)
        access = createAccess()
        const expectedTail = initialChat.message.slice(20).map(message => message.data)
        fixture.onRequest = () => {
            started.resolve()
            expect(getActiveConversationSession()).toBeNull()
            expect(captureSelectedConversationAuthority()?.totalMessages).toBe(120)
            const window = findActiveHistoryWindow(target.characterId, target.conversationId)
            expect(window?.absoluteStartIndex).toBe(20)
            expect(window?.chat.message.map(message => message.data)).toEqual(expectedTail)
            fixture.outputs.add(output)
        }
        fixture.replacers.add(async formated => {
            pendingWrite = access.setChatToIndex(0, 0, { ...structuredClone(initialChat), note: 'Window metadata' }, {
                pluginName: 'fixture', signal: lifetime.signal,
            })
            await pendingWrite
            const window = findActiveHistoryWindow(target.characterId, target.conversationId)
            expect(window?.absoluteStartIndex).toBe(20)
            expect(window?.chat.message.map(message => message.data)).toEqual(expectedTail)
            return formated
        })
        output.mockImplementation(async () => {
            const window = findActiveHistoryWindow(target.characterId, target.conversationId)
            expect(window?.absoluteStartIndex).toBe(20)
            expect(window?.chat.message.map(message => message.data)).toEqual([...expectedTail, 'answer'])
        })

        const sending = generate()
        await started.promise
        provider.resolve()
        await expect(sending).resolves.toBe(true)
        const stored = await reopen()
        expect(stored.note).toBe('Window metadata')
        expect(stored.message.map(message => message.data)).toEqual([...initialChat.message.map(message => message.data), 'answer'])
        expect(output).toHaveBeenCalledOnce()
        expect(findActiveHistoryWindow(target.characterId, target.conversationId)).toBeNull()
    })
})
