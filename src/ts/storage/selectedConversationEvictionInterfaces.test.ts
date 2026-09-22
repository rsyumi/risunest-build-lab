// @vitest-environment node
import './tests/selectedConversationEvictionNodeDom.setup'
import { afterEach, describe, expect, it, onTestFinished, vi } from 'vitest'
import type { LuaEngine } from 'wasmoon'
import { writable } from 'svelte/store'
import { createConversationOperationContext } from '../process/conversationOperationContext'
import type { Chat, Database } from './database.svelte'
import { CONVERSATION_RANGE_MAX_LIMIT } from './persistentDataStore'
import type { createPersistentDataRuntime } from './persistentDataRuntime'
import { createEvictionFixture, makeConversation, VIEWPORT_ROW_BUDGET } from './tests/selectedConversationEvictionFixture'

const INITIAL_MESSAGE_COUNT = CONVERSATION_RANGE_MAX_LIMIT + 1
const DUPLICATE_POSITIONS = [VIEWPORT_ROW_BUDGET + 1, CONVERSATION_RANGE_MAX_LIMIT]

const interfaceMockModules = [
    '../parser/parser.svelte', '../parser/chatML', '../alert', '../globalApi.svelte',
    '../platform', '../tokenizer', '../util', './database.svelte', '../stores.svelte',
    './persistentDataRuntime.svelte', '../../lang', '../process/modules',
    '../process/files/inlays', '../process/lorebook.svelte', '../process/memory/hypamemory',
    '../process/request/request', '../process/stableDiff', '../process/luaRuntime',
    '../process/scripts', '../process/triggers', '../process/command',
    '../process/templates/templates', '../process/exampleMessages', '../process/tts',
    '../process/memory/supaMemory', '../process/group', '../process/embedding/addinfo',
    '../process/models/modelString', '../process/inlayScreen', '../process/transformers',
    '../process/memory/hanuraiMemory', '../process/memory/hypav2', '../process/memory/hypav3',
    '../process/scriptings', '../plugins/plugins.svelte', '../process/presetChain',
    '../model/modellist', '../sync/multiuser',
]

afterEach(() => {
    try {
        vi.restoreAllMocks()
    } finally {
        try {
            vi.unstubAllGlobals()
        } finally {
            for (const moduleId of interfaceMockModules) vi.doUnmock(moduleId)
            vi.resetModules()
        }
    }
})

async function waitForWindowed(
    runtime: ReturnType<typeof createPersistentDataRuntime>,
    stage = 'initial activation',
) {
    await vi.waitFor(() => expect(runtime.getSelectedConversationMode(), stage).toBe('windowed'))
    expect(runtime.getActiveConversationSession()).toBeNull()
    const source = runtime.getActiveConversationViewportSource()
    expect(source).not.toBeNull()
    expect(source!.snapshot().totalMessages).toBeGreaterThan(0)
}

describe('independent windowed conversation interfaces', () => {
    it('runs real Lua from a fresh windowed owner', async () => {
        const fixture = await createEvictionFixture(makeConversation(INITIAL_MESSAGE_COUNT, DUPLICATE_POSITIONS))
        const { store, initial, runtime, selectedConversation } = fixture
        const oracle = makeConversation(INITIAL_MESSAGE_COUNT, DUPLICATE_POSITIONS)
        let expectedRevision = initial.revision
        await runtime.initializeActiveWorkingSet(fixture.workingCopy)
        await waitForWindowed(runtime)
        const assertWindowed = async (stage: string) => {
            await waitForWindowed(runtime, stage)
            const persisted = await store.readConversation('char-a', 'chat-a')
            expect(persisted!.revision).toBe(expectedRevision)
            expect(persisted!.value).toEqual(oracle)
            const lease = await runtime.acquireCompleteConversation('independent-readback')
            try { expect(lease.session.materializeCompatibilityArray()).toEqual(oracle.message) }
            finally { lease.release() }
            await waitForWindowed(runtime, 'readback demotion')
        }
        const luaLease = await runtime.acquireCompleteConversation('lua-operation-context')
        onTestFinished(() => luaLease.release())
        const luaEngines: LuaEngine[] = []
        onTestFinished(() => {
            for (const engine of luaEngines) {
                if (!engine.global.isClosed()) engine.global.close()
            }
        })
        vi.doMock('../process/luaRuntime', async () => {
            const actual = await vi.importActual<typeof import('../process/luaRuntime')>('../process/luaRuntime')
            return {
                ...actual,
                async createLuaFactory() {
                    const factory = await actual.createLuaFactory()
                    const createEngine = factory.createEngine.bind(factory)
                    factory.createEngine = async (options) => {
                        const engine = await createEngine(options)
                        luaEngines.push(engine)
                        return engine
                    }
                    return factory
                },
            }
        })
        const luaOperation = createConversationOperationContext(
            luaLease.session,
            selectedConversation(),
        )
        onTestFinished(() => luaOperation.release())
        expect(luaOperation.mode).toBe('compatibility')
        vi.doMock('../parser/parser.svelte', () => ({
            hasher: vi.fn(),
            risuChatParser: (value: string) => value,
        }))
        vi.doMock('../alert', () => ({
            alertConfirm: vi.fn(),
            alertError: vi.fn(),
            alertInput: vi.fn(),
            alertNormal: vi.fn(),
            alertSelect: vi.fn(),
        }))
        vi.doMock('../globalApi.svelte', () => ({ fetchNative: vi.fn(), readImage: vi.fn() }))
        vi.doMock('../platform', () => ({
            isTauriMobile: true,
            isTauri: false,
            isMobile: false,
        }))
        vi.doMock('../tokenizer', () => ({ tokenize: vi.fn() }))
        vi.doMock('../util', () => ({
            asBuffer: vi.fn(),
            getPersonaPrompt: vi.fn(),
            getUserIcon: vi.fn(),
            getUserName: vi.fn(() => 'User'),
        }))
        vi.doMock('./database.svelte', () => ({
            getCurrentCharacter: () => fixture.workingCopy.characters[0],
            getCurrentChat: () => selectedConversation(),
            getDatabase: () => fixture.workingCopy,
            setDatabase: vi.fn(),
        }))
        vi.doMock('../stores.svelte', () => ({
            DBState: { db: fixture.workingCopy },
            ReloadChatPointer: { update: vi.fn() },
            ReloadGUIPointer: { update: vi.fn() },
            selectedCharID: { subscribe: (run: (value: number) => void) => (run(0), () => undefined) },
        }))
        vi.doMock('../process/modules', () => ({
            getModuleLorebooks: () => [],
            getModuleTriggers: () => [],
        }))
        vi.doMock('../process/files/inlays', () => ({
            getInlayAsset: vi.fn(),
            writeInlayImage: vi.fn(),
        }))
        vi.doMock('../process/lorebook.svelte', () => ({
            loadLoreBookV3PromptFromCompatibilitySnapshot: vi.fn(),
        }))
        vi.doMock('../process/memory/hypamemory', () => ({ HypaProcesser: vi.fn() }))
        vi.doMock('../process/request/request', () => ({ requestChatData: vi.fn() }))
        vi.doMock('../process/stableDiff', () => ({ generateAIImage: vi.fn() }))
        const { readFile } = await import('node:fs/promises')
        const { resolve } = await import('node:path')
        const jsonLuaSource = await readFile(resolve(process.cwd(), 'public/lua/json.lua'), 'utf8')
        const nodeDomGlobals = new Map(
            ['window', 'document', 'navigator', 'location', 'fetch'].map((key) =>
                [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const),
        )
        try {
            vi.stubGlobal('fetch', vi.fn(async () => new Response(jsonLuaSource, { status: 200 })))
            for (const key of ['window', 'document', 'navigator', 'location']) {
                Reflect.deleteProperty(globalThis, key)
            }
            const { runScripted } = await import('../process/scriptings')
            const luaResult = await runScripted(`
                listenEdit('editInput', function(id, value, meta)
                    addChat(id, 'user', 'Lua compatibility output')
                    return false
                end)
            `, {
                char: fixture.workingCopy.characters[0],
                mode: 'editInput',
                operationContext: luaOperation,
            })
            expect(luaResult.stopSending).toBe(true)
            const luaMessage = luaOperation.chat.message.at(-1)!
            luaMessage.chatId = 'op-lua'
            luaMessage.saying = 'Lua visited complete history'
            luaOperation.commit(luaLease.session)
            luaLease.release()
            oracle.message.push({ role: 'user', data: 'Lua compatibility output', chatId: 'op-lua', saying: 'Lua visited complete history' })
            await runtime.flushPendingData('lua-operation-context')
            expectedRevision += 1
            await assertWindowed('Lua operation context')
        } finally {
            for (const [key, descriptor] of nodeDomGlobals) {
                if (descriptor) Object.defineProperty(globalThis, key, descriptor)
                else Reflect.deleteProperty(globalThis, key)
            }
        }
        for (const moduleId of [
            '../parser/parser.svelte',
            '../alert',
            '../globalApi.svelte',
            '../platform',
            '../tokenizer',
            '../util',
            './database.svelte',
            '../stores.svelte',
            '../process/modules',
            '../process/files/inlays',
            '../process/lorebook.svelte',
            '../process/memory/hypamemory',
            '../process/request/request',
            '../process/stableDiff',
            '../process/luaRuntime',
        ]) vi.doUnmock(moduleId)
        vi.resetModules()

    }, 30_000)
    it('runs public generation from a fresh windowed owner', async () => {
        const fixture = await createEvictionFixture(makeConversation(INITIAL_MESSAGE_COUNT, DUPLICATE_POSITIONS))
        const { store, initial, runtime, selectedConversation } = fixture
        const oracle = makeConversation(INITIAL_MESSAGE_COUNT, DUPLICATE_POSITIONS)
        let expectedRevision = initial.revision
        await runtime.initializeActiveWorkingSet(fixture.workingCopy)
        await waitForWindowed(runtime)
        const assertWindowed = async (stage: string) => {
            await waitForWindowed(runtime, stage)
            const persisted = await store.readConversation('char-a', 'chat-a')
            expect(persisted!.revision).toBe(expectedRevision)
            expect(persisted!.value).toEqual(oracle)
            const lease = await runtime.acquireCompleteConversation('independent-readback')
            try { expect(lease.session.materializeCompatibilityArray()).toEqual(oracle.message) }
            finally { lease.release() }
            await waitForWindowed(runtime, 'readback demotion')
        }
        const generationDBState = {
            get db() { return fixture.workingCopy },
            set db(value: Database) { fixture.workingCopy = value },
        }
        vi.doMock('../stores.svelte', () => ({
            DBState: generationDBState,
            selectedCharID: writable(0),
            ReloadGUIPointer: { update: vi.fn() },
        }))
        vi.doMock('./persistentDataRuntime.svelte', () => ({
            acquireDestructiveReplacementFence: vi.fn(),
            acknowledgeGenerationCompletion: (epoch?: number) => runtime.acknowledgeGenerationCompletion(epoch),
            assertPersistentMutationAllowed: (epoch?: number) => runtime.assertPersistentMutationAllowed(epoch),
            getPersistentStorageAuthorityEpoch: () => runtime.getStorageAuthorityEpoch(),
            getPersistentNavigationGeneration: () => runtime.getNavigationGeneration(),
            capturePersistentMutationToken: vi.fn(),
            captureSelectedConversationTarget: () => runtime.captureSelectedConversationTarget(),
            acquireCompleteConversation: (reason: string, target: never) =>
                runtime.acquireCompleteConversation(reason, target),
            getActiveConversationSession: () => runtime.getActiveConversationSession(),
            getPersistentDataRuntime: () => runtime,
            invalidateActiveConversationSession: () => runtime.invalidateActiveConversationSession(),
        }))
        vi.doMock('../process/request/request', () => ({
            requestChatData: vi.fn(async () => ({
                type: 'streaming',
                result: new ReadableStream({
                    start(controller) {
                        controller.enqueue({ response: 'Generation compatibility output' })
                        controller.close()
                    },
                }),
            })),
        }))
        vi.doMock('../tokenizer', () => ({
            ChatTokenizer: class {
                async tokenizeChat() { return 1 }
                async tokenizeChats(chats: unknown[]) { return chats.length }
            },
            tokenize: vi.fn(async () => 1), tokenizeNum: vi.fn(async () => []),
        }))
        vi.doMock('../../lang', () => ({ language: { errors: {}, otherUserRequesting: '' } }))
        vi.doMock('../alert', () => ({ alertError: vi.fn(), alertToast: vi.fn() }))
        vi.doMock('../parser/chatML', () => ({ parseChatML: (value: string) => value }))
        vi.doMock('../parser/parser.svelte', () => ({ risuChatParser: (value: string) => value }))
        vi.doMock('../util', () => ({
            checkNullish: (value: unknown) => value == null,
            findCharacterbyId: () => fixture.workingCopy.characters[0],
            getAuthorNoteDefaultText: () => '', getPersonaPrompt: () => '', getUserName: () => 'User',
            isLastCharPunctuation: () => true, trimUntilPunctuation: (value: string) => value,
            parseToggleSyntax: () => [], prebuiltAssetCommand: '',
        }))
        vi.doMock('../process/scripts', () => ({
            createPromptScriptOperationScope: () => ({
                assertOwnerCurrent: vi.fn(), adoptMessageId: vi.fn(),
                parse: (_char: unknown, value: string) => value,
                finish: vi.fn(), finishAfterError: vi.fn(), release: vi.fn(),
            }),
            processScript: vi.fn(async (_char: unknown, value: string) => value),
            processScriptFull: vi.fn(async (_char: unknown, value: string) => ({ data: value, emoChanged: false })),
            risuChatParser: (value: string) => value, resetScriptCache: vi.fn(),
        }))
        vi.doMock('../process/triggers', () => ({
            runTrigger: vi.fn(async (_char: unknown, mode: string, arg: { chat: Chat }) =>
                mode === 'start' ? null : { chat: arg.chat }),
        }))
        vi.doMock('../process/modules', () => ({
            getModuleAssets: () => [], getModuleToggles: () => '', moduleUpdate: vi.fn(),
        }))
        for (const [id, exports] of [
            ['../process/lorebook.svelte', { loadLoreBookV3Prompt: vi.fn(async () => ({ actives: [] })) }],
            ['../process/templates/templates', { prebuiltNAIpresets: [], prebuiltPresets: { OAI: { mainPrompt: '', jailbreak: '' } } }],
            ['../process/exampleMessages', { exampleMessage: () => [] }],
            ['../process/tts', { sayTTS: vi.fn() }],
            ['../process/memory/supaMemory', { supaMemory: vi.fn() }],
            ['../process/group', { groupOrder: (value: unknown) => value }],
            ['../process/memory/hypamemory', { HypaProcesser: class {} }],
            ['../process/embedding/addinfo', { additionalInformations: vi.fn(async () => '') }],
            ['../process/files/inlays', { getInlayAsset: vi.fn(async () => null) }],
            ['../process/models/modelString', { getGenerationModelString: () => 'test-model' }],
            ['../process/inlayScreen', { runInlayScreen: (_char: unknown, data: string) => ({ text: data }) }],
            ['../process/transformers', { runImageEmbedding: vi.fn() }],
            ['../process/memory/hanuraiMemory', { hanuraiMemory: vi.fn() }],
            ['../process/memory/hypav2', { hypaMemoryV2: vi.fn() }],
            ['../process/memory/hypav3', { hypaMemoryV3: vi.fn() }],
            ['../process/scriptings', { runLuaEditTrigger: vi.fn(async (_c: unknown, _m: string, value: unknown) => value) }],
            ['../globalApi.svelte', { readImage: vi.fn() }],
            ['../plugins/plugins.svelte', { pluginV2: { chatOutput: new Set() } }],
            ['../process/presetChain', { activatePresetChainForRequest: vi.fn() }],
        ] as const) vi.doMock(id, () => exports)
        vi.doMock('../model/modellist', () => ({ getModelInfo: () => ({ flags: [] }), LLMFlags: {} }))
        vi.doMock('../sync/multiuser', () => ({
            connectionOpen: false, peerRevertChat: vi.fn(), peerSafeCheck: vi.fn(async () => true),
            peerSync: vi.fn(),
        }))
        const generationBefore = oracle.message.length
        const { sendChat } = await import('../process/index.svelte')
        const generationLog = vi.spyOn(console, 'log').mockImplementation(() => undefined)
        await expect(sendChat()).resolves.toBe(true)
        generationLog.mockRestore()
        expectedRevision += 1
        const generatedConversation = await store.readConversation('char-a', 'chat-a')
        const generatedMessage = generatedConversation!.value.message[generationBefore]
        expect(generatedMessage.data).toBe('Generation compatibility output')
        oracle.message.push({
            role: 'char', data: 'Generation compatibility output', saying: 'char-a',
            chatId: expect.any(String), time: expect.any(Number),
            generationInfo: {
                model: 'test-model', generationId: expect.any(String),
                inputTokens: expect.any(Number), outputTokens: expect.any(Number), maxContext: expect.any(Number),
                stageTiming: { stage1: expect.any(Number), stage2: expect.any(Number), stage3: expect.any(Number), stage4: expect.any(Number) },
            },
            promptInfo: {},
        })
        oracle.isStreaming = false

        await assertWindowed('public generation gateway')
        vi.resetModules()

    }, 30_000)
})
