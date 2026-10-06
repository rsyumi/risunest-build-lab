// @vitest-environment node

import '../storage/tests/selectedConversationEvictionNodeDom.setup'
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'
import type { Chat, Database, Message, character } from '../storage/database.svelte'
import { DBState, selectedCharID } from '../stores.svelte'
import { IndexedDbPersistentDataStore } from '../storage/indexedDbPersistentDataStore'
import {
    capturePersistentRoot,
    createPersistentDataRuntime,
    publishPersistentConversationReplacementToWorkingSet,
    type PersistentDataRuntimeStateAdapter,
} from '../storage/persistentDataRuntime'

// The send runs against the production runtime, its windowed store and the real
// trigger, regex and Lua code. Only the provider, the tokenizer and the
// surfaces a send does not exercise here are stand-ins.
const current = vi.hoisted(() => ({
    runtime: null as unknown as ReturnType<typeof createPersistentDataRuntime>,
    store: null as unknown as IndexedDbPersistentDataStore,
    alertError: null as unknown as ReturnType<typeof vi.fn>,
    snapshots: ['answer'],
}))

vi.mock('../storage/persistentDataRuntime.svelte', async (importOriginal) => ({
    ...await importOriginal<typeof import('../storage/persistentDataRuntime.svelte')>(),
    acknowledgeGenerationCompletion: (epoch?: number) => current.runtime.acknowledgeGenerationCompletion(epoch),
    drainDeferredLwwReceives: async () => undefined,
    assertPersistentMutationAllowed: (epoch?: number) => current.runtime.assertPersistentMutationAllowed(epoch),
    getPersistentStorageAuthorityEpoch: () => current.runtime.getStorageAuthorityEpoch(),
    getPersistentNavigationGeneration: () => current.runtime.getNavigationGeneration(),
    acquireCompleteConversation: (reason: string, target: never) => current.runtime.acquireCompleteConversation(reason, target),
    captureSelectedConversationAuthority: () => current.runtime.captureSelectedConversationAuthority(),
    recordSelectedCharacterLastInteraction: (authority: never, before: number | undefined, after: number) =>
        current.runtime.recordSelectedCharacterLastInteraction(authority, before, after),
    captureWindowedConversationMutationController: (target: never, chat: Chat, start: number) =>
        current.runtime.captureWindowedConversationMutationController(target, chat, start),
    captureSelectedConversationTarget: () => current.runtime.captureSelectedConversationTarget(),
    flushPendingData: (reason: string) => current.runtime.flushPendingData(reason),
    flushPendingDataLocally: (reason: string) => current.runtime.flushPendingDataLocally(reason),
    getActiveConversationSession: () => current.runtime.getActiveConversationSession(),
    peekActiveConversationSession: () => current.runtime?.getActiveConversationSession() ?? null,
    invalidateActiveConversationSession: () => current.runtime.invalidateActiveConversationSession(),
    getPersistentDataRuntime: () => current.runtime,
}))
vi.mock('../storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: () => current.store }))
vi.mock('../storage/deviceSettings', async (importOriginal) => {
    const original = await importOriginal<typeof import('../storage/deviceSettings')>()
    return {
        ...original,
        getDeviceSettings: () => ({
            ...original.getDeviceSettings(),
            generationHistoryLimitEnabled: true,
            generationHistoryLimitMultiplier: 2,
        }),
    }
})
vi.mock('../iosNative', () => ({
    beginIOSGeneration: async (signal?: AbortSignal) => ({
        signal,
        progress: () => undefined,
        dispose: async () => undefined,
    }),
    notifyIOSGenerationComplete: vi.fn(),
    isBackgroundExpiryReason: () => false,
}))
vi.mock('../tokenizer', async () => (await import('./tests/sendChatTestHarness')).tokenizerModule({
    // Every message costs ten tokens in the window walk.
    tokenize: vi.fn(async () => 10),
    encodeWithTokenizer: vi.fn(async () => new Array(10).fill(0)),
}))
vi.mock('../../lang', async () => (await import('./tests/sendChatTestHarness')).langModule({
    generationConversationChanged: 'conversation changed',
    chatConversationActionFailed: 'conversation action failed',
}))
vi.mock('../alert', async () => {
    const harness = await import('./tests/sendChatTestHarness')
    current.alertError = vi.fn()
    return harness.alertModule({ alertError: current.alertError, alertInput: vi.fn(), alertSelect: vi.fn(), alertConfirm: vi.fn() })
})
vi.mock('./request/request', () => ({
    requestChatData: vi.fn(async () => ({
        type: 'streaming',
        result: new ReadableStream<Record<string, string>>({
            start(controller) {
                for (const response of current.snapshots) controller.enqueue({ response })
                controller.close()
            },
        }),
    })),
}))
vi.mock('../parser/chatML', async () => (await import('./tests/sendChatTestHarness')).chatMLModule())
vi.mock('../parser/parser.svelte', async () => (await import('./tests/sendChatTestHarness')).parserModule({
    assetRegex: /$^/,
    hasher: vi.fn(async () => 'hash'),
}))
vi.mock('./lorebook.svelte', async () => (await import('./tests/sendChatTestHarness')).lorebookModule())
vi.mock('./stableDiff', async () => (await import('./tests/sendChatTestHarness')).stableDiffModule({ generateAIImage: vi.fn() }))
vi.mock('./templates/templates', async () => (await import('./tests/sendChatTestHarness')).templatesModule())
vi.mock('./exampleMessages', async () => (await import('./tests/sendChatTestHarness')).exampleMessagesModule())
vi.mock('./tts', async () => (await import('./tests/sendChatTestHarness')).ttsModule())
vi.mock('./memory/supaMemory', async () => (await import('./tests/sendChatTestHarness')).supaMemoryModule())
vi.mock('./memory/hypamemory', async () => (await import('./tests/sendChatTestHarness')).hypamemoryModule())
vi.mock('./embedding/addinfo', async () => (await import('./tests/sendChatTestHarness')).addinfoModule())
vi.mock('./files/inlays', async () => (await import('./tests/sendChatTestHarness')).inlaysModule({ writeInlayImage: vi.fn() }))
vi.mock('./models/modelString', async () => (await import('./tests/sendChatTestHarness')).modelStringModule())
vi.mock('./inlayScreen', () => ({ runInlayScreen: (_char: unknown, data: string) => ({ text: data }) }))
vi.mock('./transformers', async () => (await import('./tests/sendChatTestHarness')).transformersModule())
vi.mock('./memory/hanuraiMemory', async () => (await import('./tests/sendChatTestHarness')).hanuraiMemoryModule())
vi.mock('./memory/hypav2', async () => (await import('./tests/sendChatTestHarness')).hypav2Module())
vi.mock('./memory/hypav3', async () => (await import('./tests/sendChatTestHarness')).hypav3Module())
vi.mock('../model/modellist', async () => (await import('./tests/sendChatTestHarness')).modellistModule())
vi.mock('./modules', async () => (await import('./tests/sendChatTestHarness')).modulesModule())
vi.mock('../globalApi.svelte', async () => (await import('./tests/sendChatTestHarness')).globalApiModule({
    fetchNative: vi.fn(),
    downloadFile: vi.fn(),
}))
vi.mock('../plugins/plugins.svelte', () => ({
    pluginV2: {
        providers: new Map(),
        providerOptions: new Map(),
        editdisplay: new Set(),
        editoutput: new Set(),
        editprocess: new Set(),
        editinput: new Set(),
        replacerbeforeRequest: new Set(),
        replacerafterRequest: new Set(),
        chatOutput: new Set(),
        unload: new Set(),
        loaded: false,
    },
}))
vi.mock('../plugins/pluginDatabaseAccess', async (importOriginal) => (await import('./tests/sendChatTestHarness')).pluginDatabaseAccessModule(importOriginal as () => Promise<Record<string, unknown>>))
vi.mock('./presetChain', async () => (await import('./tests/sendChatTestHarness')).presetChainModule())
vi.mock('./command', () => ({ processMultiCommand: vi.fn() }))

const MESSAGES = 1_000
const WINDOW_START = 900

function storedMessages(): Message[] {
    return Array.from({ length: MESSAGES }, (_, index): Message => ({
        role: index % 2 ? 'char' : 'user',
        data: index === 100 ? 'WRITE:outside' : index === 950 ? 'WRITE:inside' : `m${index}`,
        chatId: `id-${index}`,
        saying: index % 2 ? 'character-a' : undefined,
        time: 1_800_000_000_000 + index,
    }))
}

function syntheticDatabase(messages: Message[]): Database {
    const owner = {
        type: 'character',
        chaId: 'character-a',
        name: 'Synthetic character',
        image: '',
        firstMessage: 'greeting',
        alternateGreetings: [],
        desc: '',
        personality: '',
        scenario: '',
        notes: '',
        bias: [],
        additionalAssets: [],
        emotionImages: [],
        globalLore: [],
        chatFolders: [],
        reloadKeys: 0,
        viewScreen: 'none',
        inlayViewScreen: false,
        supaMemory: false,
        utilityBot: false,
        defaultVariables: '',
        chatPage: 0,
        customscript: [{
            comment: 'Prepare stored content',
            in: 'WRITE:',
            out: 'STORED:',
            type: 'editprocess',
            flag: 'g',
            ableFlag: true,
        }, {
            comment: 'Persist processed content',
            in: 'STORED:',
            out: '@@inject',
            type: 'editprocess',
            flag: 'g',
            ableFlag: true,
        }],
        triggerscript: [{
            comment: '',
            type: 'start',
            conditions: [],
            effect: [{
                type: 'triggerlua',
                code: `
                    function onStart(id)
                        insertChat(id, 960, 'char', 'lua inserted')
                    end
                `,
            }],
        }],
        chats: [{ id: 'chat-a', name: 'Synthetic chat', note: '', localLore: [], fmIndex: -1, message: messages }],
    } as unknown as character
    return {
        username: 'Synthetic user',
        characters: [owner],
        botPresets: [],
        botPresetsId: 0,
        plugins: [],
        pluginCustomStorage: {},
        statics: { messages: 0 },
        aiModel: 'test-model',
        maxContext: 500,
        maxResponse: 0,
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
        presetRegex: [],
        outputImageModal: false,
        rememberToolUsage: false,
    } as unknown as Database
}

async function bootWindowedConversation(messages: Message[], configure?: (seed: Database) => void) {
    const seed = syntheticDatabase(messages)
    configure?.(seed)
    const name = `history-window-send-${crypto.randomUUID()}`
    const factory = new IDBFactory()
    const store = new IndexedDbPersistentDataStore(
        name,
        factory,
        IDBKeyRange,
    )
    await store.open()
    await store.replaceFromDatabase(structuredClone(seed))
    // The working set lives in DBState, as in the app.
    DBState.db = seed
    selectedCharID.set(0)
    const selected = () => DBState.db.characters[0]
    const state: PersistentDataRuntimeStateAdapter = {
        captureWorkingSetDatabase: () => DBState.db,
        captureRoot: () => capturePersistentRoot(DBState.db),
        captureSelectedCharacter: () => selected() ?? null,
        captureCharacter: (id) => DBState.db.characters.find((candidate) => candidate.chaId === id) ?? null,
        getSelectedCharacterId: () => selected()?.chaId,
        getSelectedConversationId: () => selected()?.chats[selected().chatPage ?? 0]?.id,
        replaceDatabase: (next) => { DBState.db = next },
        publishCharacter: (next) => { DBState.db.characters[0] = next },
        publishConversation: (_characterId, conversation, nextCharacter) => {
            if (nextCharacter) DBState.db.characters[0] = nextCharacter
            else DBState.db.characters[0].chats[DBState.db.characters[0].chatPage ?? 0] = conversation
        },
        publishConversationReplacement: (result) => {
            publishPersistentConversationReplacementToWorkingSet(DBState.db, result)
        },
        canUseWindowedSelectedConversation: () => true,
        isConversationOperationActive: () => false,
        conversationViewportRowBudget: 64,
    }
    const backgroundErrors: unknown[] = []
    current.store = store
    current.runtime = createPersistentDataRuntime({
        store,
        state,
        onBackgroundError: (error) => { backgroundErrors.push(error) },
        prepareDatabase: async (candidate) => candidate,
    })
    await current.runtime.initializeActiveWorkingSet(DBState.db)
    await vi.waitFor(
        () => expect(current.runtime.getSelectedConversationMode()).toBe('windowed'),
        { timeout: 5_000 },
    )
    return { store, backgroundErrors, async reopen() {
        const reopened = new IndexedDbPersistentDataStore(name, factory, IDBKeyRange)
        await reopened.open()
        return (await reopened.readConversation('character-a', 'chat-a'))!.value
    } }
}

let restoreFetch: (() => void) | null = null

beforeAll(async () => {
    const jsonLuaSource = await readFile(resolve(process.cwd(), 'public/lua/json.lua'), 'utf8')
    const fetch = globalThis.fetch
    globalThis.fetch = vi.fn(async () => new Response(jsonLuaSource, { status: 200 })) as typeof globalThis.fetch
    restoreFetch = () => { globalThis.fetch = fetch }
    // The Lua runtime loads its WebAssembly from disk only when no browser globals are present,
    // so its factory is created once here before the send runs with the DOM in place.
    const { runScripted } = await import('./scriptings')
    DBState.db = syntheticDatabase([])
    selectedCharID.set(0)
    const browserGlobals = new Map(
        ['window', 'document', 'navigator', 'location'].map((key) =>
            [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const),
    )
    try {
        for (const key of browserGlobals.keys()) Reflect.deleteProperty(globalThis, key)
        await runScripted(`listenEdit('editInput', function(id, value, meta) return value end)`, {
            char: { chaId: 'lua-warm-up' } as never,
            chat: { message: [] } as never,
            data: 'input',
            mode: 'editInput',
        })
    } finally {
        for (const [key, descriptor] of browserGlobals) {
            if (descriptor) Object.defineProperty(globalThis, key, descriptor)
        }
    }
})

afterAll(() => {
    restoreFetch?.()
})

describe('a windowed send through the production runtime', () => {
    it.each(['insert', 'remove'] as const)('persists the response after Lua %s moves its output target', async (action) => {
        const original = storedMessages()
        const { reopen, backgroundErrors } = await bootWindowedConversation(structuredClone(original), (seed) => {
            seed.characters[0].customscript = []
            seed.characters[0].triggerscript = [{ type: 'start', conditions: [], effect: [{ type: 'triggerlua', code: `
                local moved = false
                listenEdit('editOutput', function(id, value, meta)
                    if not moved then
                        setChatVar(id, 'first_index', tostring(meta.index))
                        ${action === 'insert' ? "insertChat(id, 960, 'user', 'owned insert')" : 'removeChat(id, 960)'}
                        moved = true
                    else
                        setChatVar(id, 'later_index', tostring(meta.index))
                    end
                    return value
                end)
            ` }] }] as never
        })
        const { sendChat } = await import('./index.svelte')
        const { pluginV2 } = await import('../plugins/plugins.svelte')
        const output = vi.fn(async () => undefined)
        pluginV2.chatOutput.add(output)
        current.snapshots = ['preview', 'answer']
        vi.spyOn(console, 'log').mockImplementation(() => undefined)
        try {
            await expect(sendChat({ historyLimit: true })).resolves.toBe(true)
            const stored = await reopen()
            const index = action === 'insert' ? 1001 : 999
            expect(stored.message[index]).toMatchObject({ role: 'char', data: 'answer' })
            expect(stored.scriptstate).toMatchObject({ $first_index: '1000', $later_index: String(index) })
            expect(JSON.stringify(stored.message.slice(0, WINDOW_START))).toBe(JSON.stringify(original.slice(0, WINDOW_START)))
            expect(output).toHaveBeenCalledOnce()
            expect(backgroundErrors).toEqual([])
        } finally {
            pluginV2.chatOutput.delete(output)
            current.snapshots = ['answer']
        }
    })

    it('stores trigger, Lua, @@inject and response writes inside the window and leaves earlier messages byte-identical', async () => {
        const original = storedMessages()
        const { store, backgroundErrors } = await bootWindowedConversation(structuredClone(original))
        const { sendChat } = await import('./index.svelte')
        vi.spyOn(console, 'log').mockImplementation(() => undefined)

        await expect(sendChat({ historyLimit: true })).resolves.toBe(true)
        await current.runtime.flushPendingData('history-window-send-test')

        expect(current.runtime.getSelectedConversationMode()).toBe('windowed')
        const persisted = (await store.readConversation('character-a', 'chat-a'))!.value.message
        expect(JSON.stringify(persisted.slice(0, WINDOW_START))).toBe(JSON.stringify(original.slice(0, WINDOW_START)))
        const inserted = persisted[960]
        expect(persisted.slice(WINDOW_START)).toEqual([
            ...original.slice(WINDOW_START, 950),
            { ...original[950], data: 'STORED:inside' },
            ...original.slice(951, 960),
            { role: 'char', data: 'lua inserted', chatId: inserted.chatId },
            ...original.slice(960),
            expect.objectContaining({ role: 'char', data: 'answer' }),
        ])
        expect(inserted.chatId).toEqual(expect.any(String))
        expect(persisted).toHaveLength(MESSAGES + 2)
        expect(current.alertError).not.toHaveBeenCalled()
        expect(backgroundErrors).toEqual([])
    })
})
