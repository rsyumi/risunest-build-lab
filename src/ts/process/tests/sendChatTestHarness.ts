import { vi } from 'vitest'

// Shared vi.mock module factories for the sendChat integration suites.
// Each factory returns the module shape expected by index.svelte.ts and its
// dependency graph; suites pass overrides for the pieces they drive from
// their own hoisted state. Consume from a test file as:
//   vi.mock('../tokenizer', async () =>
//       (await import('./tests/sendChatTestHarness')).tokenizerModule({ ... }))
// Defaults are checked against the module types, and overrides must name exports the module has.

type Overrides<M> = { [K in keyof M]?: unknown }

type TokenizerModule = typeof import('../../tokenizer')
type UtilModule = typeof import('../../util')
type ScriptsModule = typeof import('../scripts')
type TemplatesModule = typeof import('../templates/templates')
type HypamemoryModule = typeof import('../memory/hypamemory')
type Hypav2Module = typeof import('../memory/hypav2')
type HanuraiMemoryModule = typeof import('../memory/hanuraiMemory')
type DatabasePreparationModule = typeof import('../../storage/databasePreparation')
type ModellistModule = typeof import('../../model/modellist')
type PluginsModule = typeof import('../../plugins/plugins.svelte')
type PluginDatabaseAccessModule = typeof import('../../plugins/pluginDatabaseAccess')
type PersistentRuntimeModule = typeof import('../../storage/persistentDataRuntime.svelte')
type DeviceSettingsModule = typeof import('../../storage/deviceSettings')
type IOSNativeModule = typeof import('../../iosNative')

export function tokenizerModule(overrides: Overrides<TokenizerModule> & {
    tokenizeChat?: (...args: unknown[]) => Promise<number> | number
    tokenizeChats?: (chats: unknown[]) => Promise<number> | number
} = {}) {
    const { tokenizeChat, tokenizeChats, ...rest } = overrides
    const chatTokenizer = tokenizeChat ?? (async () => 1)
    const chatsTokenizer = tokenizeChats ?? (async (chats: unknown[]) => chats.length)
    const module = {
        ChatTokenizer: class {
            tokenizeChat(...args: unknown[]) {
                return chatTokenizer(...args)
            }
            tokenizeChats(chats: unknown[]) {
                return chatsTokenizer(chats)
            }
        } as unknown as TokenizerModule['ChatTokenizer'],
        tokenize: vi.fn(async () => 1),
        tokenizeNum: vi.fn(async () => []),
    } satisfies Partial<TokenizerModule>
    return { ...module, ...rest }
}

export function alertModule(overrides: Overrides<typeof import('../../alert')> = {}) {
    const module = {
        alertError: vi.fn(), alertToast: vi.fn(), alertNormal: vi.fn(),
    } satisfies Partial<typeof import('../../alert')>
    return { ...module, ...overrides }
}

export function chatMLModule(overrides: Overrides<typeof import('../../parser/chatML')> = {}) {
    const module = {
        parseChatML: ((value: string) => value) as unknown as typeof import('../../parser/chatML')['parseChatML'],
    } satisfies Partial<typeof import('../../parser/chatML')>
    return { ...module, ...overrides }
}

export function lorebookModule(overrides: Overrides<typeof import('../lorebook.svelte')> = {}) {
    const module = {
        loadLoreBookV3Prompt: vi.fn(async () => ({ actives: [] })) as unknown as typeof import('../lorebook.svelte')['loadLoreBookV3Prompt'],
    } satisfies Partial<typeof import('../lorebook.svelte')>
    return { ...module, ...overrides }
}

export function utilModule(overrides: Overrides<UtilModule> = {}) {
    const module = {
        checkNullish: (value: unknown) => value === null || value === undefined,
        decryptBuffer: vi.fn(),
        encryptBuffer: vi.fn(),
        selectSingleFile: vi.fn(),
        findCharacterbyId: vi.fn(),
        getAuthorNoteDefaultText: () => '',
        getPersonaPrompt: () => '',
        getUserName: () => 'User',
        isLastCharPunctuation: () => true,
        trimUntilPunctuation: (value: string) => value,
        parseToggleSyntax: () => [],
        parseKeyValue: () => [],
        sleep: async () => undefined,
        prebuiltAssetCommand: '' as unknown as UtilModule['prebuiltAssetCommand'],
    } satisfies Partial<UtilModule>
    return { ...module, ...overrides }
}

export function scriptsModule(overrides: Overrides<ScriptsModule> = {}) {
    const module = {
        createPromptScriptOperationScope: (() => ({
            assertOwnerCurrent: vi.fn(),
            adoptMessageId: vi.fn(),
            parse: (_char: unknown, text: string) => text,
            finish: vi.fn(),
            finishAfterError: vi.fn(),
            release: vi.fn(),
        })) as unknown as ScriptsModule['createPromptScriptOperationScope'],
        processScript: vi.fn(async (_char: unknown, data: string) => data),
        processScriptFull: vi.fn(async (_char: unknown, data: string) => ({
            data,
            emoChanged: false,
        })),
        risuChatParser: (value: string) => value,
        resetScriptCache: vi.fn(),
    } satisfies Partial<ScriptsModule>
    return { ...module, ...overrides }
}

export function templatesModule(overrides: Overrides<TemplatesModule> = {}) {
    const module = {
        prebuiltNAIpresets: [] as unknown as TemplatesModule['prebuiltNAIpresets'],
        prebuiltPresets: { OAI: { mainPrompt: '', jailbreak: '' } } as unknown as TemplatesModule['prebuiltPresets'],
    } satisfies Partial<TemplatesModule>
    return { ...module, ...overrides }
}

export function exampleMessagesModule(overrides: Overrides<typeof import('../exampleMessages')> = {}) {
    const module = {
        exampleMessage: vi.fn(() => []),
    } satisfies Partial<typeof import('../exampleMessages')>
    return { ...module, ...overrides }
}

export function ttsModule(overrides: Overrides<typeof import('../tts')> = {}) {
    const module = {
        sayTTS: vi.fn(async () => undefined),
    } satisfies Partial<typeof import('../tts')>
    return { ...module, ...overrides }
}

export function stableDiffModule(overrides: Overrides<typeof import('../stableDiff')> = {}) {
    const module = { stableDiff: vi.fn() } satisfies Partial<typeof import('../stableDiff')>
    return { ...module, ...overrides }
}

export function supaMemoryModule(overrides: Overrides<typeof import('../memory/supaMemory')> = {}) {
    const module = { supaMemory: vi.fn() } satisfies Partial<typeof import('../memory/supaMemory')>
    return { ...module, ...overrides }
}

export function hypamemoryModule(overrides: Overrides<HypamemoryModule> = {}) {
    const module = {
        HypaProcesser: class {} as unknown as HypamemoryModule['HypaProcesser'],
    } satisfies Partial<HypamemoryModule>
    return { ...module, ...overrides }
}

/** Hanurai memory that keeps every chat and reports the tokens it was given. */
export function hanuraiMemoryModule(overrides: Overrides<HanuraiMemoryModule> = {}) {
    const module = {
        hanuraiMemory: vi.fn(async (
            chats: Parameters<HanuraiMemoryModule['hanuraiMemory']>[0],
            { currentTokens }: { currentTokens: number },
        ) => ({ chats, tokens: currentTokens })),
    } satisfies Partial<HanuraiMemoryModule>
    return { ...module, ...overrides }
}

/** HypaV2 memory that keeps every chat and reports the tokens it was given. */
export function hypav2Module(overrides: Overrides<Hypav2Module> = {}) {
    const module = {
        hypaMemoryV2: vi.fn(async (chats: Parameters<Hypav2Module['hypaMemoryV2']>[0], currentTokens: number) => ({
            chats, currentTokens,
        })),
    } satisfies Partial<Hypav2Module>
    return { ...module, ...overrides }
}

export function hypav3Module(overrides: Overrides<typeof import('../memory/hypav3')> = {}) {
    const module = { hypaMemoryV3: vi.fn() } satisfies Partial<typeof import('../memory/hypav3')>
    return { ...module, ...overrides }
}

export function addinfoModule(overrides: Overrides<typeof import('../embedding/addinfo')> = {}) {
    const module = {
        additionalInformations: vi.fn(async () => ''),
    } satisfies Partial<typeof import('../embedding/addinfo')>
    return { ...module, ...overrides }
}

export function inlaysModule(overrides: Overrides<typeof import('../files/inlays')> = {}) {
    const module = {
        getInlayAsset: vi.fn(async () => null),
    } satisfies Partial<typeof import('../files/inlays')>
    return { ...module, ...overrides }
}

export function modelStringModule(overrides: Overrides<typeof import('../models/modelString')> = {}) {
    const module = {
        getGenerationModelString: vi.fn(() => 'test-model'),
    } satisfies Partial<typeof import('../models/modelString')>
    return { ...module, ...overrides }
}

export function inlayScreenModule(overrides: Overrides<typeof import('../inlayScreen')> = {}) {
    const module = {
        runInlayScreen: vi.fn((_char: unknown, data: string) => ({ text: data })),
    } satisfies Partial<typeof import('../inlayScreen')>
    return { ...module, ...overrides }
}

export function transformersModule(overrides: Overrides<typeof import('../transformers')> = {}) {
    const module = { runImageEmbedding: vi.fn() } satisfies Partial<typeof import('../transformers')>
    return { ...module, ...overrides }
}

export function scriptingsModule(overrides: Overrides<typeof import('../scriptings')> = {}) {
    const module = {
        runLuaEditTrigger: vi.fn(async (_char: unknown, _mode: string, value: unknown) => value) as unknown as
            typeof import('../scriptings')['runLuaEditTrigger'],
    } satisfies Partial<typeof import('../scriptings')>
    return { ...module, ...overrides }
}

export function modellistModule(overrides: Overrides<ModellistModule> = {}) {
    const module = {
        getModelInfo: vi.fn(() => ({ flags: [] }) as unknown as ReturnType<ModellistModule['getModelInfo']>),
        LLMFlags: { hasImageInput: 'hasImageInput' } as unknown as ModellistModule['LLMFlags'],
    } satisfies Partial<ModellistModule>
    return { ...module, ...overrides }
}

export function modulesModule(overrides: Overrides<typeof import('../modules')> = {}) {
    const module = {
        getModuleAssets: vi.fn(() => []),
        getModuleLorebooks: vi.fn(() => []),
        getModuleRegexScripts: vi.fn(() => []),
        getModuleTriggers: vi.fn(() => []),
        getModuleToggles: vi.fn(() => ''),
        moduleUpdate: vi.fn(),
    } satisfies Partial<typeof import('../modules')>
    return { ...module, ...overrides }
}

export function globalApiModule(overrides: Record<string, unknown> = {}) {
    return { readImage: vi.fn(), ...overrides }
}

export function commandModule() {
    return { processMultiCommand: vi.fn() } satisfies Partial<typeof import('../command')>
}

/** The provider request, answered by `requestChatData`. */
export function requestModule(requestChatData: (...args: any[]) => unknown) {
    return {
        requestChatData: requestChatData as typeof import('../request/request')['requestChatData'],
    } satisfies Partial<typeof import('../request/request')>
}

/** The iOS bridge on a platform without background generation. */
export function iosNativeModule() {
    return {
        beginIOSGeneration: (async (signal?: AbortSignal) => ({
            signal,
            progress: () => undefined,
            dispose: async () => undefined,
        })) as unknown as IOSNativeModule['beginIOSGeneration'],
        notifyIOSGenerationComplete: vi.fn(),
        isBackgroundExpiryReason: () => false,
    } satisfies Partial<IOSNativeModule>
}

/** The plugin registry; `pluginV2` members not given are empty. */
export function pluginsModule(pluginV2: { [K in keyof PluginsModule['pluginV2']]?: unknown } = {}) {
    return {
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
            ...pluginV2,
        } as PluginsModule['pluginV2'],
    } satisfies Partial<PluginsModule>
}

export async function pluginDatabaseAccessModule(importOriginal: () => Promise<unknown>) {
    const module = {
        createProductionPluginChatOutputProjector: ((
            snapshot: <T>(value: T) => T,
        ) => async (input: { liveCharacter: unknown; liveConversation: unknown }) => ({
            char: snapshot(input.liveCharacter),
            chat: snapshot(input.liveConversation),
        })) as unknown as PluginDatabaseAccessModule['createProductionPluginChatOutputProjector'],
    } satisfies Partial<PluginDatabaseAccessModule>
    return { ...(await importOriginal() as PluginDatabaseAccessModule), ...module }
}

export function pinnedConversationPositionModule(resolve: (characterId: string, conversationId: string) => Promise<unknown>) {
    return {
        resolvePinnedConversationPosition: resolve as typeof import('../../plugins/pinnedConversationPosition')['resolvePinnedConversationPosition'],
    } satisfies Partial<typeof import('../../plugins/pinnedConversationPosition')>
}

export function presetChainModule(overrides: Overrides<typeof import('../presetChain')> = {}) {
    const module = {
        activatePresetChainForRequest: vi.fn(async () => undefined),
    } satisfies Partial<typeof import('../presetChain')>
    return { ...module, ...overrides }
}

/**
 * The persistence runtime as sendChat sees it with no selected conversation: every mutation is allowed,
 * nothing is captured, and acknowledgements and flushes resolve at once.
 */
export function persistentRuntimeModule(overrides: Overrides<PersistentRuntimeModule> = {}) {
    const module = {
        assertPersistentMutationAllowed: vi.fn(),
        getPersistentStorageAuthorityEpoch: () => 0,
        getPersistentNavigationGeneration: () => 0,
        acknowledgeGenerationCompletion: vi.fn(async () => undefined),
        drainDeferredLwwReceives: vi.fn(async () => undefined),
        captureSelectedConversationTarget: () => null,
        captureSelectedConversationAuthority: () => null,
        recordSelectedCharacterLastInteraction: vi.fn(() => true),
        captureWindowedConversationMutationController: () => null,
        acquireCompleteConversation: vi.fn(),
        flushPendingData: vi.fn(async () => undefined),
        getActiveConversationSession: () => null,
        peekActiveConversationSession: () => null,
        invalidateActiveConversationSession: vi.fn(),
    } satisfies Partial<PersistentRuntimeModule>
    return { ...module, ...overrides }
}

export function persistentDataStoreFactoryModule(store: () => unknown) {
    return {
        getPersistentDataStore: store as typeof import('../../storage/persistentDataStoreFactory')['getPersistentDataStore'],
    } satisfies Partial<typeof import('../../storage/persistentDataStoreFactory')>
}

/** Device settings with the generation history window set as `enabled` says, at twice the context. */
export async function historyWindowDeviceSettingsModule(importOriginal: () => Promise<unknown>, enabled: () => boolean) {
    const original = await importOriginal() as DeviceSettingsModule
    return {
        ...original,
        getDeviceSettings: () => ({
            ...original.getDeviceSettings(),
            generationHistoryLimitEnabled: enabled(),
            generationHistoryLimitMultiplier: 2,
        }),
    } satisfies DeviceSettingsModule
}

/** Persistence preparation that copies the database instead of preparing it. */
export async function copyingDatabasePreparationModule(importOriginal: () => Promise<unknown>) {
    return {
        ...await importOriginal() as DatabasePreparationModule,
        prepareDatabaseForPersistence: async (input: Parameters<DatabasePreparationModule['prepareDatabaseForPersistence']>[0]) =>
            structuredClone(input),
    } satisfies DatabasePreparationModule
}

export function generationDiagnosticsModule(
    actual: typeof import('../../generationDiagnostics'),
    invoke: (command: string, args: Record<string, unknown>) => Promise<unknown>,
) {
    const diagnostics = actual.createGenerationDiagnostics({ enabled: true, invoke })
    diagnostics.startDocument()
    return { ...actual, generationDiagnostics: diagnostics } satisfies typeof import('../../generationDiagnostics')
}
