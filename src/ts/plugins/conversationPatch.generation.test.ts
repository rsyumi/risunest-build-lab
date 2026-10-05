import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'

const fixture = vi.hoisted(() => ({
    store: null as any,
    replacers: new Set<(formated: unknown[], model: string) => Promise<unknown[]>>(),
    modelRequests: 0,
}))

vi.mock('src/ts/storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: () => fixture.store }))
vi.mock('src/ts/parser/parser.svelte', () => ({
    assetRegex: /$^/,
    hasher: vi.fn(async () => 'hash'),
    parseMarkdownSafe: (value: string) => value,
    ParseMarkdown: vi.fn(async (value: string) => value),
    risuChatParser: (value: string) => value,
}))
vi.mock('src/ts/tokenizer', async () => (await import('../process/tests/sendChatTestHarness')).tokenizerModule())
vi.mock('src/lang', async () => (await import('../process/tests/sendChatTestHarness')).langModule())
vi.mock('src/ts/alert', async () => (await import('../process/tests/sendChatTestHarness')).alertModule())
vi.mock('src/ts/parser/chatML', async () => (await import('../process/tests/sendChatTestHarness')).chatMLModule())
vi.mock('src/ts/process/lorebook.svelte', async () => (await import('../process/tests/sendChatTestHarness')).lorebookModule())
// The provider request: `beforeRequest` replacers run where `requestChatData` runs them, then the reply.
vi.mock('src/ts/process/request/request', () => ({
    requestChatData: vi.fn(async (request: { formated: unknown[] }, purpose: string) => {
        if (purpose === 'emotion') return '|igp'
        fixture.modelRequests += 1
        for (const replacer of fixture.replacers) request.formated = await replacer(request.formated, purpose)
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
vi.mock('src/ts/process/group', async () => (await import('../process/tests/sendChatTestHarness')).groupModule())
// A character without trigger scripts: the real `runTrigger` returns null for every mode.
vi.mock('src/ts/process/triggers', () => ({ runTrigger: vi.fn(async () => null) }))
vi.mock('src/ts/process/memory/hypamemory', async () => (await import('../process/tests/sendChatTestHarness')).hypamemoryModule())
vi.mock('src/ts/process/embedding/addinfo', async () => (await import('../process/tests/sendChatTestHarness')).addinfoModule())
vi.mock('src/ts/process/files/inlays', async () => (await import('../process/tests/sendChatTestHarness')).inlaysModule())
vi.mock('src/ts/process/models/modelString', async () => (await import('../process/tests/sendChatTestHarness')).modelStringModule())
vi.mock('src/ts/sync/multiuser', async () => (await import('../process/tests/sendChatTestHarness')).multiuserModule())
vi.mock('src/ts/process/inlayScreen', () => ({ runInlayScreen: (_char: unknown, data: string) => ({ text: data }) }))
vi.mock('src/ts/process/transformers', async () => (await import('../process/tests/sendChatTestHarness')).transformersModule())
vi.mock('src/ts/process/memory/hanuraiMemory', () => ({
    hanuraiMemory: vi.fn(async (chats: unknown, { currentTokens }: { currentTokens: number }) => ({ chats, tokens: currentTokens })),
}))
vi.mock('src/ts/process/memory/hypav2', () => ({
    hypaMemoryV2: vi.fn(async (chats: unknown, currentTokens: number) => ({ chats, currentTokens })),
}))
vi.mock('src/ts/process/memory/hypav3', async () => (await import('../process/tests/sendChatTestHarness')).hypav3Module())
vi.mock('src/ts/process/scriptings', async () => (await import('../process/tests/sendChatTestHarness')).scriptingsModule())
vi.mock('src/ts/model/modellist', async () => (await import('../process/tests/sendChatTestHarness')).modellistModule())
vi.mock('src/ts/process/modules', async () => (await import('../process/tests/sendChatTestHarness')).modulesModule())
vi.mock('src/ts/globalApi.svelte', async () => (await import('../process/tests/sendChatTestHarness')).globalApiModule({ forageStorage: {} }))
vi.mock('src/ts/plugins/plugins.svelte', () => ({
    pluginV2: { chatOutput: new Set(), editprocess: new Set(), replacerbeforeRequest: fixture.replacers, replacerafterRequest: new Set() },
}))
vi.mock('src/ts/plugins/pluginDatabaseAccess', async (importOriginal) =>
    (await import('../process/tests/sendChatTestHarness')).pluginDatabaseAccessModule(importOriginal as () => Promise<Record<string, unknown>>))
vi.mock('src/ts/process/presetChain', async () => (await import('../process/tests/sendChatTestHarness')).presetChainModule())

import type { Database, Message } from '../storage/database.svelte'
import { getDatabase, setDatabaseLite } from '../storage/database.svelte'
import { IndexedDbPersistentDataStore } from '../storage/indexedDbPersistentDataStore'
import {
    acquireCompleteConversation,
    captureSelectedConversationTarget,
    flushPendingDataLocally,
    getActiveConversationSession,
    initializeActiveWorkingSet,
} from '../storage/persistentDataRuntime.svelte'
import { selectedCharID } from '../stores.svelte'
import { doingChat, sendChat } from '../process/index.svelte'
import { createRisunestPrivateApi } from './apiV3/risunestPrivateApi'
import { conversationPatchAccess } from './conversationPatchHost'

const target = { characterId: 'character-a', conversationId: 'chat-a' }
const factory = new IDBFactory()
const storeName = `patch-generation-${crypto.randomUUID()}`

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

const patches = createRisunestPrivateApi({
    databaseAccess: { readConversationContext: vi.fn() } as any,
    patchAccess: conversationPatchAccess,
    hostTools: { listTools: vi.fn(), callTool: vi.fn() },
    chatView: { register: vi.fn(), unregister: vi.fn(), dispose: vi.fn() } as any,
    hasDatabasePermission: async () => true,
    lifetimeSignal: new AbortController().signal,
})

// The selected conversation keeps its messages in the active session, and the chat object carries
// its metadata. A generation holds the conversation complete; once idle it may be windowed again,
// so reads after a generation promote it the way the chat screen does.
function generatingConversation() {
    const session = getActiveConversationSession()
    expect(session?.isActive).toBe(true)
    return { message: session!.materializeCompatibilityArray(), scriptstate: getDatabase().characters[0].chats[0].scriptstate }
}

async function live() {
    const lease = await acquireCompleteConversation('patch-generation-test', captureSelectedConversationTarget())
    try {
        return { message: lease.session.materializeCompatibilityArray(), scriptstate: getDatabase().characters[0].chats[0].scriptstate }
    } finally {
        lease.release()
    }
}

async function reloadedConversation() {
    const reopened = new IndexedDbPersistentDataStore(storeName, factory, IDBKeyRange)
    await reopened.open()
    return (await reopened.readConversation(target.characterId, target.conversationId))!.value
}

describe('plugin conversation patch from a beforeRequest replacer', () => {
    let log: { mockRestore(): void } | undefined
    beforeAll(async () => {
        log = vi.spyOn(console, 'log').mockImplementation(() => undefined)
        fixture.store = new IndexedDbPersistentDataStore(storeName, factory, IDBKeyRange)
        await fixture.store.open()
        const initial = database()
        await fixture.store.replaceFromDatabase(initial)
        setDatabaseLite(initial)
        selectedCharID.set(0)
        await initializeActiveWorkingSet(getDatabase())
    })
    afterAll(() => {
        fixture.replacers.clear()
        doingChat.set(false)
        selectedCharID.set(-1)
        log?.mockRestore()
    })

    it('applies a plugin field and chat variable during the request, keeps the reply, and refuses message data', async () => {
        const outcomes: unknown[] = []
        fixture.replacers.add(async (formated) => {
            outcomes.push(await patches.patchConversation({
                ...target,
                mutationId: 'data-during-request',
                messages: [{ index: 0, messageId: 'user-message', expected: { data: 'hello' }, set: { data: 'rewritten' } }],
            }))
            outcomes.push(await patches.patchConversation({
                ...target,
                mutationId: 'metadata-during-request',
                messages: [{ index: 0, messageId: 'user-message', expected: { data: 'hello' }, set: { __translation: 'record' } }],
                chatVariables: [{ key: '$bridge', expected: null, value: 'on' }],
            }))
            return formated
        })

        await expect(sendChat()).resolves.toBe(true)

        expect(fixture.modelRequests).toBe(1)
        expect(outcomes).toEqual([
            { status: 'busy', revision: expect.any(Number) },
            { status: 'applied', revision: expect.any(Number) },
        ])
        const applied = await live()
        expect(applied.message.map((message) => message.data)).toEqual(['hello', 'answer'])
        expect(applied.message[0]).toMatchObject({ data: 'hello', __translation: 'record' })
        expect(applied.scriptstate).toEqual({ $bridge: 'on' })

        await flushPendingDataLocally('patch-generation-test')
        const stored = await reloadedConversation()
        expect(stored.message.map((message) => message.data)).toEqual(['hello', 'answer'])
        expect(stored.message[0]).toMatchObject({ data: 'hello', __translation: 'record' })
        expect(stored.scriptstate).toEqual({ $bridge: 'on' })
    })

    it('lands a patch the replacer does not wait for before the reply is applied', async () => {
        fixture.replacers.clear()
        let pending: Promise<unknown> | undefined
        fixture.replacers.add(async (formated) => {
            pending = patches.patchConversation({
                ...target,
                mutationId: 'unawaited-during-request',
                messages: [{ index: 0, messageId: 'user-message', expected: { data: 'hello' }, set: { __summary: 'late' } }],
                chatVariables: [{ key: '$late', expected: null, value: 'yes' }],
            })
            return formated
        })

        await expect(sendChat()).resolves.toBe(true)

        await expect(pending).resolves.toEqual({ status: 'applied', revision: expect.any(Number) })
        const applied = await live()
        expect(applied.message.map((message) => message.data)).toEqual(['hello', 'answer', 'answer'])
        expect(applied.message[0]).toMatchObject({ __translation: 'record', __summary: 'late' })
        expect(applied.scriptstate).toEqual({ $bridge: 'on', $late: 'yes' })

        await flushPendingDataLocally('patch-generation-test')
        const stored = await reloadedConversation()
        expect(stored.message.map((message) => message.data)).toEqual(['hello', 'answer', 'answer'])
        expect(stored.message[0]).toMatchObject({ __translation: 'record', __summary: 'late' })
        expect(stored.scriptstate).toEqual({ $bridge: 'on', $late: 'yes' })
    })

    it('applies the reply only after a patch admitted during the request has landed', async () => {
        fixture.replacers.clear()
        let releaseCommit!: () => void
        const commitHeld = new Promise<void>((resolve) => { releaseCommit = resolve })
        let held = false
        const commit = fixture.store.commit.bind(fixture.store)
        // Holds the store commit that carries the patch, so the patch is admitted but has not landed.
        const commits = vi.spyOn(fixture.store, 'commit').mockImplementation(async (input: unknown) => {
            if (!held && JSON.stringify(input).includes('__held')) {
                held = true
                await commitHeld
            }
            return commit(input)
        })
        let pending: Promise<unknown> | undefined
        fixture.replacers.add(async (formated) => {
            pending = patches.patchConversation({
                ...target,
                mutationId: 'held-during-request',
                messages: [{ index: 0, messageId: 'user-message', expected: { data: 'hello' }, set: { __held: 'landed' } }],
                chatVariables: [{ key: '$held', expected: null, value: 'landed' }],
            })
            return formated
        })
        try {
            const sending = sendChat()
            await vi.waitFor(() => expect(held).toBe(true))
            await vi.waitFor(() => expect(fixture.modelRequests).toBe(3))
            expect(generatingConversation().message).toHaveLength(3)
            releaseCommit()

            await expect(sending).resolves.toBe(true)
            await expect(pending).resolves.toEqual({ status: 'applied', revision: expect.any(Number) })
            const applied = await live()
            expect(applied.message.map((message) => message.data)).toEqual(['hello', 'answer', 'answer', 'answer'])
            expect(applied.message[0]).toMatchObject({ __held: 'landed' })
            expect(applied.scriptstate).toMatchObject({ $held: 'landed' })

            await flushPendingDataLocally('patch-generation-test')
            const stored = await reloadedConversation()
            expect(stored.message.map((message) => message.data)).toEqual(['hello', 'answer', 'answer', 'answer'])
            expect(stored.message[0]).toMatchObject({ __held: 'landed' })
            expect(stored.scriptstate).toMatchObject({ $held: 'landed' })
        } finally {
            commits.mockRestore()
        }
    })
})
