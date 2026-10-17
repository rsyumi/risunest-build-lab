// @vitest-environment node

import '../storage/tests/selectedConversationEvictionNodeDom.setup'
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import type { Chat, Database, Message, character } from '../storage/database.svelte'
import { DBState, selectedCharID, settingsOpen, ReloadChatPointer } from '../stores.svelte'
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
vi.mock('../storage/persistentDataStoreFactory', async () => (await import('./tests/sendChatTestHarness')).persistentDataStoreFactoryModule(() => current.store))
vi.mock('../storage/deviceSettings', async (importOriginal) => (await import('./tests/sendChatTestHarness')).historyWindowDeviceSettingsModule(importOriginal, () => true))
vi.mock('../iosNative', async () => (await import('./tests/sendChatTestHarness')).iosNativeModule())
vi.mock('../tokenizer', async () => (await import('./tests/sendChatTestHarness')).tokenizerModule({
    // Every message costs ten tokens in the window walk.
    tokenize: vi.fn(async () => 10),
    encodeWithTokenizer: vi.fn(async () => new Array(10).fill(0)),
}))
vi.mock('../alert', async () => {
    const harness = await import('./tests/sendChatTestHarness')
    current.alertError = vi.fn()
    return harness.alertModule({ alertError: current.alertError, alertInput: vi.fn(), alertSelect: vi.fn(), alertConfirm: vi.fn() })
})
vi.mock('./request/request', async () => (await import('./tests/sendChatTestHarness')).requestModule(vi.fn(async () => ({
    type: 'streaming',
    result: new ReadableStream<Record<string, string>>({
        start(controller) {
            for (const response of current.snapshots) controller.enqueue({ response })
            controller.close()
        },
    }),
}))))
vi.mock('../parser/chatML', async () => (await import('./tests/sendChatTestHarness')).chatMLModule())
vi.mock('../parser/parser.svelte', async (importOriginal) => ({
    ...(await import('src/ts/parser/parser.testSupport')).parserStubModule(),
    risuChatParser: (await importOriginal<typeof import('../parser/parser.svelte')>()).risuChatParser,
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
vi.mock('./inlayScreen', async () => (await import('./tests/sendChatTestHarness')).inlayScreenModule())
vi.mock('./transformers', async () => (await import('./tests/sendChatTestHarness')).transformersModule())
vi.mock('./memory/hanuraiMemory', async () => (await import('./tests/sendChatTestHarness')).hanuraiMemoryModule())
vi.mock('./memory/hypav2', async () => (await import('./tests/sendChatTestHarness')).hypav2Module())
vi.mock('./memory/hypav3', async () => (await import('./tests/sendChatTestHarness')).hypav3Module())
vi.mock('../model/modellist', async () => (await import('./tests/sendChatTestHarness')).modellistModule())
vi.mock('./modules', async (importOriginal) => ({
    ...await importOriginal<typeof import('./modules')>(),
    ...(await import('./tests/sendChatTestHarness')).modulesModule(),
}))
vi.mock('../globalApi.svelte', async () => (await import('./tests/sendChatTestHarness')).globalApiModule({
    fetchNative: vi.fn(),
    downloadFile: vi.fn(),
}))
vi.mock('../plugins/plugins.svelte', async () => (await import('./tests/sendChatTestHarness')).pluginsModule())
vi.mock('../plugins/pluginDatabaseAccess', async (importOriginal) => (await import('./tests/sendChatTestHarness')).pluginDatabaseAccessModule(importOriginal))
vi.mock('./presetChain', async () => (await import('./tests/sendChatTestHarness')).presetChainModule())
vi.mock('./command', async () => (await import('./tests/sendChatTestHarness')).commandModule())

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
    const { resetScriptCache } = await import('./scripts')
    resetScriptCache()
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
    it.each(['direct', 'expanded'])('uses a shared complete lease for %s full-history background CBS', async (kind) => {
        await bootWindowedConversation(storedMessages(), (seed) => {
            seed.characters[0].customscript = []
            seed.characters[0].triggerscript = []
            seed.characters[0].chats[0].scriptstate = { $expanded: '{{history}}' }
        })
        const { acquireLiveDisplayParserLease } = await import('../liveDisplayParserLease')
        const { risuChatParser } = await import('../parser/parser.svelte')
        const shared = vi.spyOn(current.runtime, 'acquireCompleteConversation')
        const controller = new AbortController()
        const lease = await acquireLiveDisplayParserLease({
            source: kind === 'direct' ? '{{history}}' : '{{getvar::expanded}}',
            character: DBState.db.characters[0], signal: controller.signal, parseFirst: true,
        })
        try {
            expect(shared).toHaveBeenCalledOnce()
            if (kind === 'expanded') {
                expect(lease?.projection?.parsedSource).toBe('{{history}}')
                const parser = lease!.projection!.context.parserContext
                const result = risuChatParser(lease!.projection!.parsedSource!, {
                    ...parser, db: parser.database, chara: parser.character,
                })
                expect(result).toContain('m0')
                expect(parser.historyAccess!.row(999)?.data).toBe('m999')
                expect(result).toBe(risuChatParser('{{history}}', { db: DBState.db, chara: DBState.db.characters[0] }))
                parser.historyAccess!.assertComplete()
            }
        } finally { lease?.release(); controller.abort(); shared.mockRestore() }
    })
    it('orders concurrent parser variable increments before reentrant script callbacks', async () => {
        const { reopen, backgroundErrors } = await bootWindowedConversation(storedMessages(), (seed) => {
            seed.characters[0].customscript = []
            seed.characters[0].triggerscript = []
            seed.characters[0].chats[0].scriptstate = { $counter: '0' }
        })
        const { parseRisuChat } = await import('../plugins/parseRisuChat')
        const { pluginV2 } = await import('../plugins/plugins.svelte')
        const increment = '{{setvar::counter::{{calc::{{getvar::counter}}+1}}}}{{getvar::counter}}'
        expect(await Promise.all([
            parseRisuChat(increment, { runVar: true }),
            parseRisuChat(increment, { runVar: true }),
            parseRisuChat('{{getvar::counter}}'),
        ])).toEqual(['1', '2', '2'])
        const hook = async (data: string) => `${data}:${await parseRisuChat(increment, { runVar: true })}`
        pluginV2.editprocess.add(hook)
        try {
            expect(await parseRisuChat(increment, { runVar: true, processRegex: true })).toBe('3:4')
        } finally { pluginV2.editprocess.delete(hook) }
        expect((await reopen()).scriptstate.$counter).toBe('4')
        expect(backgroundErrors).toEqual([])
    })
    it.each(['variable', 'inject'])('persists a delayed parser %s write to the invocation conversation after navigation', async (effect) => {
        const { store, reopen, backgroundErrors } = await bootWindowedConversation(storedMessages(), (seed) => {
            seed.characters[0].customscript = effect === 'inject'
                ? [{ type: 'editprocess', in: 'm999', out: '@@inject', flag: 'g', ableFlag: true }] as any : []
            seed.characters[0].triggerscript = []
            seed.characters[0].chats.push({ ...seed.characters[0].chats[0], id: 'chat-b',
                message: [{ role: 'user', data: 'other conversation' }], scriptstate: {} })
        })
        const { parseRisuChat } = await import('../plugins/parseRisuChat')
        let entered!: () => void
        const reading = new Promise<void>((resolve) => { entered = resolve })
        let release!: () => void
        const wait = new Promise<void>((resolve) => { release = resolve })
        const acquire = store.acquireRevision.bind(store)
        let delayed = false
        const spy = vi.spyOn(store, 'acquireRevision').mockImplementation(async (revision) => {
            const lease = await acquire(revision)
            const read = lease.readConversationWindow.bind(lease)
            lease.readConversationWindow = async (query) => {
                if (!delayed && query.conversationId === 'chat-a') {
                    delayed = true
                    entered()
                    await wait
                }
                return read(query)
            }
            return lease
        })
        const pending = parseRisuChat('{{lastmessage}}|{{setvar::captured::yes}}{{getvar::captured}}', {
            runVar: true, processRegex: effect === 'inject', messageIndex: 0,
        })
        try {
            await reading
            await current.runtime.activateConversation('chat-b')
            release()
            expect(await pending).toBe(effect === 'inject' ? '|yes' : 'm999|yes')
            await current.runtime.flushPendingData('parser-navigation')
            expect((await reopen()).scriptstate.$captured).toBe('yes')
            expect((await reopen()).message[0].data).toBe(effect === 'inject' ? 'm999|yes' : 'm0')
            expect((await store.readConversation('character-a', 'chat-b'))?.value.scriptstate?.$captured).toBeUndefined()
            expect((await store.readConversation('character-a', 'chat-b'))?.value.message[0].data).toBe('other conversation')
            expect(backgroundErrors).toEqual([])
        } finally { release(); spy.mockRestore() }
    })

    it('allows parseRisuChat and its variable writes inside the provider callback without waiting for the send', async () => {
        const { reopen, backgroundErrors } = await bootWindowedConversation(storedMessages(), (seed) => {
            seed.characters[0].customscript = []
            seed.characters[0].triggerscript = []
            seed.mainPrompt = '{{#when::1::is::1}}system{{/when}}{{#if 1}}prompt{{/if}}{{#pure}}plain{{/pure}}'
        })
        const complete = vi.spyOn(current.runtime, 'acquireCompleteConversation')
        const { sendChat } = await import('./index.svelte')
        const { requestChatData } = await import('./request/request')
        const { parseRisuChat } = await import('../plugins/parseRisuChat')
        const results: string[] = []
        const { registerDisplayRow, DisplayReadRecord } = await import('./scriptVariableReads')
        const row = registerDisplayRow(() => 0)
        const reads = new DisplayReadRecord()
        reads.note('api_write')
        row.track('body', reads)
        vi.mocked(requestChatData).mockImplementationOnce(async () => {
            results.push(await parseRisuChat('{{lastmessageindex}}|{{previouschatlog::0}}'))
            const previousReload = get(ReloadChatPointer)[0] ?? 0
            results.push(await parseRisuChat('{{setvar::api_write::saved}}{{getvar::api_write}}', { runVar: true }))
            expect(get(ReloadChatPointer)[0] ?? 0).toBeGreaterThan(previousReload)
            results.push(await parseRisuChat('{{previouschatlog::0}}', { processRegex: true }))
            return { type: 'success', result: 'api answer' }
        })
        vi.spyOn(console, 'log').mockImplementation(() => undefined)
        try { await expect(sendChat({ historyLimit: true })).resolves.toBe(true) }
        finally { row.dispose() }
        expect(results).toEqual(['999|m0', 'saved', 'm0'])
        const stored = await reopen()
        expect(stored.message.at(-1)?.data).toBe('api answer')
        expect(stored.scriptstate.$api_write).toBe('saved')
        expect(backgroundErrors).toEqual([])
        expect(complete).not.toHaveBeenCalled()
        complete.mockRestore()
    })

    it('keeps parser reads and variable commits available inside successive streaming output callbacks', async () => {
        const { reopen, backgroundErrors } = await bootWindowedConversation(storedMessages(), (seed) => {
            seed.characters[0].customscript = []
            seed.characters[0].triggerscript = []
        })
        const { sendChat } = await import('./index.svelte')
        const { pluginV2 } = await import('../plugins/plugins.svelte')
        const { parseRisuChat } = await import('../plugins/parseRisuChat')
        const counts: string[] = []
        const output = async (data: string) => {
            counts.push(await parseRisuChat('{{lastmessageindex}}', { processRegex: true }))
            await parseRisuChat(`{{setvar::stream_calls::${counts.length}}}`, { runVar: true })
            return data
        }
        pluginV2.editoutput.add(output)
        current.snapshots = ['first token', 'second token']
        vi.spyOn(console, 'log').mockImplementation(() => undefined)
        try {
            await expect(sendChat({ historyLimit: true })).resolves.toBe(true)
            expect(counts.length).toBeGreaterThanOrEqual(2)
            expect(counts.every((count) => count === '1000')).toBe(true)
            const stored = await reopen()
            expect(stored.message.at(-1)?.data).toBe('second token')
            expect(stored.scriptstate.$stream_calls).toBe(String(counts.length))
            expect(backgroundErrors).toEqual([])
        } finally { pluginV2.editoutput.delete(output); current.snapshots = ['answer'] }
    })

    it.each(['success', 'streaming'] as const)('persists a delayed %s response while settings remain open', async (type) => {
        const { reopen, backgroundErrors } = await bootWindowedConversation(storedMessages())
        const { sendChat } = await import('./index.svelte')
        const { requestChatData } = await import('./request/request')
        const { reserveGeneration } = await import('./generationState')
        const { beginGenerationEndRun, subscribeGenerationEnd } = await import('./generationEnd')
        const controller = new AbortController()
        const reservation = reserveGeneration(() => controller.abort())!
        const target = current.runtime.captureSelectedConversationTarget()!
        const navigation = current.runtime.getNavigationGeneration()
        const finish = vi.fn()
        const unsubscribe = subscribeGenerationEnd(finish)
        const run = beginGenerationEndRun(target)
        let entered!: () => void
        const requested = new Promise<void>(resolve => { entered = resolve })
        let release!: () => void
        const responseReady = new Promise<void>(resolve => { release = resolve })
        vi.mocked(requestChatData).mockImplementationOnce(async () => {
            entered()
            await responseReady
            return type === 'success'
                ? { type: 'success', result: 'hidden answer' }
                : { type: 'streaming', result: new ReadableStream({ start(stream) {
                    stream.enqueue({ response: 'hidden' })
                    stream.enqueue({ response: 'hidden answer' })
                    stream.close()
                } }) }
        })
        vi.spyOn(console, 'log').mockImplementation(() => undefined)
        const sending = run.capture(() => sendChat({ historyLimit: true, signal: controller.signal }, reservation))
        try {
            await requested
            settingsOpen.set(true)
            expect(current.runtime.getNavigationGeneration()).toBe(navigation)
            expect(current.runtime.captureSelectedConversationTarget()?.conversationId).toBe(target.conversationId)
            release()
            await expect(sending).resolves.toBe(true)
            reservation.release()
            run.finish('completed')
            const stored = await reopen()
            expect(stored.message.filter(message => message.data === 'hidden answer')).toHaveLength(1)
            expect(finish).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
                characterId: target.characterId, conversationId: target.conversationId, status: 'completed',
            }))
            expect(backgroundErrors).toEqual([])
        } finally {
            release()
            await sending
            reservation.release()
            unsubscribe()
            settingsOpen.set(false)
        }
    })

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
