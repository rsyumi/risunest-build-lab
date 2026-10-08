import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('../storage/persistentDataRuntime.svelte', async (original) => ({
    ...(await original<object>()),
    assertPersistentMutationAllowed: vi.fn(),
    getPersistentStorageAuthorityEpoch: vi.fn(() => 0),
}))
vi.mock('../storage/persistentDataStoreFactory', () => ({
    getPersistentDataStore: () => undefined,
}))
vi.mock('../parser/parser.svelte', () => ({
    assetRegex: /$^/,
    hasher: vi.fn(async () => 'hash'),
    parseMarkdownSafe: (value: string) => value,
    ParseMarkdown: vi.fn(async (value: string) => value),
    risuChatParser: (value: string) => value,
}))
vi.mock('./apiV3/v3.svelte', () => ({
    loadV3Plugins: vi.fn(async () => undefined),
    customV3ProviderMetaStore: [],
    areV3PluginsIdle: () => true,
    canInterruptV3Plugins: () => true,
    prepareV3PluginsForReload: () => true,
    subscribeV3PluginActivity: () => () => {},
}))

// The store module has to finish evaluating before the plugin graph pulls it in.
import '../stores.svelte'
import * as alertModule from '../alert'
import { loadV3Plugins } from './apiV3/v3.svelte'
import { setDatabaseLite, type Database } from '../storage/database.svelte'
import { keepPluginsOffForThisStart, loadPlugins, loadPluginsAfterAuthoritativeRestore, requestPluginReloadAfterSync, type RisuPlugin } from './plugins.svelte'

function makePlugin(name: string, version: RisuPlugin['version']): RisuPlugin {
    return { name, version, enabled: true, script: '', realArg: {}, arguments: {}, customLink: [], argMeta: {} }
}

beforeEach(() => {
    vi.mocked(loadV3Plugins).mockClear()
    vi.spyOn(alertModule, 'alertError').mockImplementation(() => {})
    setDatabaseLite({
        characters: [],
        botPresets: [],
        botPresetsId: 0,
        plugins: [makePlugin('synthetic-modern', '3.0'), makePlugin('synthetic-unsupported', '2.1')],
    } as unknown as Database)
})

describe('plugins left off for this start', () => {
    it('loads plugins until startup leaves them off, then never again in this run', async () => {
        await loadPlugins()
        expect(loadV3Plugins).toHaveBeenCalledOnce()
        vi.mocked(loadV3Plugins).mockClear()
        vi.mocked(alertModule.alertError).mockClear()

        keepPluginsOffForThisStart()
        await loadPlugins()
        await loadPluginsAfterAuthoritativeRestore()

        expect(loadV3Plugins).not.toHaveBeenCalled()
        expect(alertModule.alertError).not.toHaveBeenCalled()
    })

    it('keeps plugins off when a received plugin change asks for a reload', async () => {
        keepPluginsOffForThisStart()
        requestPluginReloadAfterSync()
        for (let turn = 0; turn < 5; turn += 1) await new Promise((resolve) => setTimeout(resolve, 0))

        expect(loadV3Plugins).not.toHaveBeenCalled()
    })
})
