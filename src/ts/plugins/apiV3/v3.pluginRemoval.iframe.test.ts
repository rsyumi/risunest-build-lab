import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { RisuPlugin } from '../plugins.svelte'

const fixture = vi.hoisted(() => ({
    database: { characters: [], plugins: [], currentPluginProvider: '' } as any,
    // The library plugin storage both the sandbox and the data deletion use.
    storage: new Map<string, Map<string, unknown>>(),
}))

function ownedStorage(owner: string) {
    const values = () => {
        let owned = fixture.storage.get(owner)
        if (!owned) fixture.storage.set(owner, owned = new Map())
        return owned
    }
    return {
        getItem: async (key: string) => values().get(key) ?? null,
        setItem: async (key: string, value: unknown) => { values().set(key, value) },
        removeItem: async (key: string) => { values().delete(key) },
        clear: async () => values().clear(),
        key: async (index: number) => [...values().keys()][index] ?? null,
        keys: async () => [...values().keys()],
        length: async () => values().size,
        snapshot: async () => Object.fromEntries(values()),
        mutate: async (mutations: { type: 'set' | 'delete'; key: string; value?: unknown }[]) => {
            for (const mutation of mutations) {
                if (mutation.type === 'delete') values().delete(mutation.key)
                else values().set(mutation.key, mutation.value)
            }
        },
    }
}

vi.mock('../plugins.svelte', async () => {
    const { createPluginLoadOrchestrator } = await import('../pluginCompatibility')
    const load = createPluginLoadOrchestrator<RisuPlugin>({
        resetRegistry: async () => undefined,
        loadV3: async (plugins) => (await import('./v3.svelte')).loadV3Plugins([...plugins]),
    })
    const unrelated = vi.fn()
    return {
        allowedDbKeys: [],
        applyPreparedPluginDatabaseUpdate: vi.fn(),
        customProviderStore: {
            subscribe(run: (value: string[]) => void) { run([]); return () => undefined },
            set: vi.fn(),
        },
        getV2PluginAPIs: () => new Proxy({}, { get: () => unrelated }),
        handlePluginInstallViaPlugin: vi.fn(),
        loadPlugins: () => load(fixture.database.plugins.filter((plugin: RisuPlugin) => plugin.enabled)),
        pluginStorageStore: {
            forOwner: ownedStorage,
            ownerOf: () => 'removal-plugin',
            invalidateOwner: vi.fn(),
            synchronizeCommittedMutation: vi.fn(),
        },
        pluginV2: { providers: new Map(), providerOptions: new Map(), chatOutput: new Set() },
    }
})
vi.mock('../../storage/persistentDataStoreFactory', () => ({
    getPersistentDataStore: () => ({
        listPluginStorage: async () => [...fixture.storage].flatMap(([owner, values]) => [...values].map(([key, value]) => ({
            owner, key, valueType: typeof value === 'string' ? 'string' : 'json', byteSize: JSON.stringify(value).length,
        }))),
    }),
}))
vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => fixture.database }))
vi.mock('../pluginSafeClass', () => ({ SafeLocalPluginStorage: class {}, SafeLocalStorage: class {}, tagWhitelist: [] }))
vi.mock('src/ts/stores.svelte', () => ({
    DBState: { get db() { return fixture.database } },
    selectedCharID: {
        subscribe(run: (value: number) => void) { run(0); return () => undefined },
    },
    additionalChatMenu: [], additionalFloatingActionButtons: [], additionalHamburgerMenu: [],
    additionalSettingsMenu: [], bodyIntercepterStore: [], chatPanelStore: [],
}))
vi.mock('src/ts/alert', () => ({
    alertConfirm: vi.fn(async () => true), alertError: vi.fn(), alertNormal: vi.fn(),
}))
// Unload callbacks race this timeout, so it has to be a real one.
vi.mock('src/ts/util', () => ({ sleep: (ms: number) => new Promise((resolve) => setTimeout(resolve, ms)) }))
vi.mock('src/lang', () => ({ language: {} }))
vi.mock('src/ts/globalApi.svelte', () => ({
    checkCharOrder: vi.fn(), forageStorage: {}, getFetchLogs: vi.fn(),
}))
vi.mock('src/ts/gui/colorscheme', () => ({
    changeColorScheme: vi.fn(), updateColorScheme: vi.fn(), updateTextThemeAndCSS: vi.fn(),
}))
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('src/ts/process/mcp/pluginmcp', () => ({
    registerMCPModule: vi.fn(), unregisterMCPModule: vi.fn(),
}))
vi.mock('src/ts/process/files/inlays', () => ({ getInlayAsset: vi.fn() }))
vi.mock('src/ts/translator/translator', () => ({ getLLMCache: vi.fn(), searchLLMCache: vi.fn() }))
vi.mock('src/ts/parser/parser.svelte', () => ({ hasher: vi.fn(async () => 'hash') }))
vi.mock('localforage', () => ({ default: { createInstance: () => ({
    getItem: vi.fn(async () => null),
    setItem: vi.fn(),
}) } }))
vi.mock('src/ts/process/index.svelte', () => ({
    sendChat: vi.fn(),
    doingChat: { subscribe(run: (value: boolean) => void) { run(false); return () => undefined } },
}))
vi.mock('src/ts/model/modellist', () => ({ getModelInfo: () => ({ id: 'test-model' }) }))
vi.mock('src/ts/process/request/request', () => ({ requestChatDataMain: vi.fn() }))
vi.mock('src/ts/process/modules', () => ({ getModuleLorebooks: vi.fn() }))
vi.mock('src/ts/process/ttsHooks', () => ({
    registerTTSPreprocessor: vi.fn(), unregisterTTSPreprocessor: vi.fn(),
    registerTTSPostprocessor: vi.fn(), unregisterTTSPostprocessor: vi.fn(),
}))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    getPersistentRevision: () => 0,
    commitPersistentUnitIntent: vi.fn(),
    acquireCompleteConversation: vi.fn(),
    captureSelectedConversationTarget: vi.fn(() => null),
    flushPendingDataLocally: vi.fn(),
    assertPersistentMutationAllowed: vi.fn(),
    getPersistentStorageAuthorityEpoch: () => 0,
    getActiveConversationSession: vi.fn(() => null),
    getPersistentNavigationGeneration: vi.fn(() => 0),
    invalidateActiveConversationSession: vi.fn(),
    materializePersistentDatabaseSnapshotWithRevision: vi.fn(),
    refreshSelectedConversationAfterReplacement: vi.fn(),
    replacePersistentCompleteCharacter: vi.fn(),
    replacePersistentConversation: vi.fn(),
    replacePersistentDatabase: vi.fn(),
}))

import { listPluginDataItems } from '../pluginDataInventory'
import { removeInstalledPlugin } from '../pluginRemoval'
import { loadPlugins } from '../plugins.svelte'
import { loadV3Plugins } from './v3.svelte'

const owner = 'removal-plugin'
const removable: RisuPlugin = {
    name: owner,
    script: `
window.ready = (async () => {
    await risuai.onUnload(async () => {
        await risuai.pluginStorage.setItem('saved-while-unloading', { cache: 'flushed' })
    })
    await risuai.pluginStorage.setItem('saved-while-running', 'value')
})()
`,
    arguments: {},
    realArg: {},
    version: '3.0',
    customLink: [],
    argMeta: {},
    enabled: true,
}

async function executeIframeSrcdoc(frame: HTMLIFrameElement): Promise<void> {
    const child = frame.contentWindow!
    const childRealm = child as any
    Object.defineProperty(child, 'ImageBitmap', {
        configurable: true,
        value: globalThis.ImageBitmap,
    })
    vi.spyOn(window, 'postMessage').mockImplementation((data) => {
        window.dispatchEvent(new MessageEvent('message', { data, source: child }))
    })
    vi.spyOn(child.parent, 'postMessage').mockImplementation((data) => {
        window.dispatchEvent(new MessageEvent('message', { data, source: child }))
    })
    vi.spyOn(child, 'postMessage').mockImplementation((data, _origin, transfer?: Transferable[]) => {
        child.dispatchEvent(new childRealm.MessageEvent('message', { data, source: child.parent, ports: transfer ?? [] }))
    })
    const source = frame.srcdoc.match(/<script nonce="[^"]+">([\s\S]*)<\/script>/)?.[1]
    if (!source) throw new Error('Sandbox guest script was not found')
    await childRealm.eval(source)
}

async function runInstalledPlugin(): Promise<void> {
    fixture.database.plugins = [structuredClone(removable)]
    fixture.database.currentPluginProvider = owner
    await loadPlugins()
    const frame = document.querySelector<HTMLIFrameElement>('iframe[data-risu-plugin-frame]')
    if (!frame) throw new Error('Plugin iframe was not created')
    await executeIframeSrcdoc(frame)
    await (frame.contentWindow as any).ready
}

async function ownedKeys(): Promise<string[]> {
    return (await listPluginDataItems('library')).filter((item) => item.owner === owner).map((item) => item.key).sort()
}

beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('ImageBitmap', class {})
    fixture.storage.clear()
    vi.spyOn(console, 'log').mockImplementation(() => undefined)
})

afterEach(async () => {
    await loadV3Plugins([])
    vi.restoreAllMocks()
})

describe('plugin removal with a running sandbox', () => {
    it('leaves no data behind that the plugin saved while unloading', async () => {
        await runInstalledPlugin()
        expect(await ownedKeys()).toEqual(['saved-while-running'])

        await removeInstalledPlugin(owner, true)

        expect(fixture.database.plugins).toEqual([])
        expect(fixture.database.currentPluginProvider).toBe('')
        expect(document.querySelector('iframe[data-risu-plugin-frame]')).toBeNull()
        expect(await ownedKeys()).toEqual([])
    })

    it('keeps what the plugin saved while unloading when the data is kept', async () => {
        await runInstalledPlugin()

        await removeInstalledPlugin(owner, false)

        expect(fixture.database.plugins).toEqual([])
        expect(await ownedKeys()).toEqual(['saved-while-running', 'saved-while-unloading'])
    })
})
