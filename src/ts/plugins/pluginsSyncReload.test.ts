import { afterEach, describe, expect, it, vi } from 'vitest'

const activity = vi.hoisted(() => ({ idle: false, interruptible: true, changed: () => {} }))
vi.mock('../storage/persistentDataRuntime.svelte', async (original) => ({
    ...(await original<object>()),
    assertPersistentMutationAllowed: vi.fn(),
    getPersistentStorageAuthorityEpoch: () => 0,
}))
vi.mock('../storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: () => undefined }))
vi.mock('../parser/parser.svelte', () => ({
    assetRegex: /$^/, hasher: vi.fn(async () => 'hash'),
    parseMarkdownSafe: (value: string) => value, ParseMarkdown: vi.fn(async (value: string) => value),
    risuChatParser: (value: string) => value,
}))
vi.mock('./apiV3/v3.svelte', () => ({
    loadV3Plugins: vi.fn(async () => undefined), customV3ProviderMetaStore: [],
    areV3PluginsIdle: () => activity.idle,
    canInterruptV3Plugins: () => activity.interruptible,
    prepareV3PluginsForReload: (interrupt?: boolean) => activity.idle || (interrupt === true && activity.interruptible),
    subscribeV3PluginActivity: (changed: () => void) => { activity.changed = changed; return () => {} },
}))

import '../stores.svelte'
import { loadV3Plugins } from './apiV3/v3.svelte'
import { setDatabaseLite, getDatabase, type Database } from '../storage/database.svelte'
import { cancelPluginReloadAfterSync, loadPlugins, PLUGIN_SYNC_RELOAD_WAIT_LIMIT_MS, requestPluginReloadAfterSync } from './plugins.svelte'

function install() {
    activity.idle = false
    activity.interruptible = true
    vi.mocked(loadV3Plugins).mockClear()
    setDatabaseLite({ characters: [], botPresets: [], botPresetsId: 0,
        plugins: [{name:'synthetic', version:'3.0', enabled:true, script:'', realArg:{value:1}, arguments:{}, customLink:[], argMeta:{}}],
    } as unknown as Database)
}

afterEach(() => { cancelPluginReloadAfterSync() })

describe('remote plugin reload wiring', () => {
    it('keeps active plugins running, then loads the latest received values once idle', async () => {
        install()
        requestPluginReloadAfterSync()
        requestPluginReloadAfterSync()
        await Promise.resolve()
        expect(loadV3Plugins).not.toHaveBeenCalled()
        getDatabase().plugins[0].realArg.value = 2
        activity.idle = true
        activity.changed()
        await vi.waitFor(() => expect(loadV3Plugins).toHaveBeenCalledOnce())
        expect(vi.mocked(loadV3Plugins).mock.calls[0][0][0].realArg.value).toBe(2)
    })

    it('interrupts plugin work that outlasts the wait limit with the latest received values', async () => {
        install()
        vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
        try {
            requestPluginReloadAfterSync()
            getDatabase().plugins[0].realArg.value = 3
            await vi.advanceTimersByTimeAsync(PLUGIN_SYNC_RELOAD_WAIT_LIMIT_MS - 1)
            expect(loadV3Plugins).not.toHaveBeenCalled()
            await vi.advanceTimersByTimeAsync(1)
            await vi.waitFor(() => expect(loadV3Plugins).toHaveBeenCalledOnce())
            expect(vi.mocked(loadV3Plugins).mock.calls[0][0][0].realArg.value).toBe(3)
        } finally { vi.useRealTimers() }
    })

    it('drops a queued remote reload when a direct reload replaces it', async () => {
        install()
        requestPluginReloadAfterSync()
        await loadPlugins()
        expect(loadV3Plugins).toHaveBeenCalledOnce()
        activity.idle = true
        activity.changed()
        await Promise.resolve()
        expect(loadV3Plugins).toHaveBeenCalledOnce()
    })
})
