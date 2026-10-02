import { beforeEach, describe, expect, it, vi } from 'vitest'

// Version-gate unit tests isolate admission; actual write fences have runtime coverage.
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
}))

// The store module has to finish evaluating before the plugin graph pulls it in
// through the database module, otherwise its startup effect runs mid-cycle.
import '../stores.svelte'
import * as alertModule from '../alert'
import { customV3ProviderMetaStore, loadV3Plugins } from './apiV3/v3.svelte'
import {
    getDatabase,
    setDatabaseLite,
    type Database,
} from '../storage/database.svelte'
import { importPlugin, loadPlugins, loadPluginsAfterAuthoritativeRestore, handlePluginInstallViaPlugin, applyPreparedPluginDatabaseUpdate, allowedDbKeys, type RisuPlugin } from './plugins.svelte'

import { createPluginDatabaseAccess } from './pluginDatabaseAccess'

let alertError: ReturnType<typeof vi.spyOn>

function makePlugin(name: string, version: RisuPlugin['version']): RisuPlugin {
    return {
        name,
        version,
        enabled: true,
        script: '',
        realArg: {},
        arguments: {},
        customLink: [],
        argMeta: {},
    }
}

beforeEach(() => {
    vi.restoreAllMocks()
    vi.mocked(loadV3Plugins).mockClear()
    alertError = vi.spyOn(alertModule, 'alertError').mockImplementation(() => {})
    setDatabaseLite({
        characters: [],
        botPresets: [],
        botPresetsId: 0,
        plugins: [],
    } as unknown as Database)
})

describe('plugin API version gate', () => {
    it('restarts only supported enabled hosts and retains fresh provider registrations through the exact restore followup',async()=>{
        await loadPlugins()
        const meta=customV3ProviderMetaStore as unknown as Array<{name:string}>
        meta.push({name:'stale-provider'})
        const db=getDatabase()
        db.plugins=[makePlugin('modern','3.0'),{...makePlugin('disabled','3.0'),enabled:false},makePlugin('unsupported','2.1')]
        setDatabaseLite(db)
        vi.mocked(loadV3Plugins).mockClear()
        vi.mocked(loadV3Plugins).mockImplementationOnce(async plugins=>{meta.push({name:plugins[0].name})})
        await loadPluginsAfterAuthoritativeRestore()
        expect(vi.mocked(loadV3Plugins).mock.calls.map(([plugins])=>plugins.map(plugin=>plugin.name))).toEqual([['modern']])
        expect(meta).toEqual([{name:'modern'}])
        await loadPluginsAfterAuthoritativeRestore(true)
        expect(loadV3Plugins).toHaveBeenCalledOnce()
        expect(meta).toEqual([{name:'modern'}])
    })

    it('loads only API 3.0 records and reports the stored older ones', async () => {
        const db = getDatabase()
        db.plugins = [makePlugin('legacy', '2.1'), makePlugin('modern', '3.0')]
        setDatabaseLite(db)

        await loadPlugins()

        expect(vi.mocked(loadV3Plugins)).toHaveBeenCalledTimes(1)
        expect(
            vi.mocked(loadV3Plugins).mock.calls[0][0].map((plugin) => plugin.name),
        ).toEqual(['modern'])
        expect(alertError).toHaveBeenCalledTimes(1)
        expect(alertError.mock.calls[0][0]).toContain('legacy')
        // The unsupported record is neither disabled nor removed.
        expect(
            getDatabase().plugins.map((plugin) => [plugin.name, plugin.enabled]),
        ).toEqual([
            ['legacy', true],
            ['modern', true],
        ])
    })

    it('reports unsupported content only once per session and notices changed scripts', async () => {
        const db = getDatabase()
        db.plugins = [makePlugin('deduplicated-legacy', '2.1')]
        setDatabaseLite(db)
        await loadPlugins()
        await loadPlugins()
        expect(alertError).toHaveBeenCalledTimes(1)
        getDatabase().plugins[0].script = 'changed script'
        await loadPlugins()
        expect(alertError).toHaveBeenCalledTimes(2)
    })

    it('loads without reporting when every enabled record is API 3.0', async () => {
        const db = getDatabase()
        db.plugins = [makePlugin('modern', '3.0')]
        setDatabaseLite(db)

        await loadPlugins()

        expect(alertError).not.toHaveBeenCalled()
        expect(
            vi.mocked(loadV3Plugins).mock.calls[0][0].map((plugin) => plugin.name),
        ).toEqual(['modern'])
    })

    it('refuses to install a bundle that declares API 2.1', async () => {
        await importPlugin('//@name legacy-bundle\n//@api 2.1\n\nconsole.log("x")')

        expect(alertError).toHaveBeenCalledTimes(1)
        expect(alertError.mock.calls[0][0]).toContain('2.1')
        expect(getDatabase().plugins).toHaveLength(0)
        expect(vi.mocked(loadV3Plugins)).not.toHaveBeenCalled()
    })

    it('refuses to install a bundle without an API banner', async () => {
        await importPlugin('//@name bannerless\n\nconsole.log("x")')

        expect(alertError).toHaveBeenCalledTimes(1)
        expect(getDatabase().plugins).toHaveLength(0)
    })

    it('installs a bundle that declares API 3.0', async () => {
        await importPlugin('//@name modern-bundle\n//@api 3.0\n\nconsole.log("x")')

        expect(alertError).not.toHaveBeenCalled()
        expect(getDatabase().plugins.map((plugin) => plugin.name)).toEqual([
            'modern-bundle',
        ])
        expect(getDatabase().plugins[0].version).toBe('3.0')
    })
})


describe('plugin list database mutation policy', () => {
    function access() {
        return createPluginDatabaseAccess({
            owner: 'caller',
            getPersistentRevision:()=>1,
            commitPersistentUnitIntent:async (_reason:string,units:readonly import('./pluginUnitIntents').PluginUnitMutation[])=>{
                let order: string[] | undefined
                for(const unit of units){
                    const key=JSON.parse(unit.key)
                    if(key[0]==='order'&&key[1]==='plugins'){if(unit.type==='set')order=unit.value as string[];continue}
                    if(key[0]!=='record'||key[1]!=='plugins')throw new Error(`Unexpected plugin list unit ${unit.key}`)
                    const plugins=getDatabase().plugins
                    const index=plugins.findIndex(plugin=>plugin.name===key[2])
                    if(unit.type==='delete'){if(index>=0)plugins.splice(index,1)}
                    else if(index>=0)plugins[index]=structuredClone(unit.value) as RisuPlugin
                    else plugins.push(structuredClone(unit.value) as RisuPlugin)
                }
                if(order){
                    const current=getDatabase().plugins
                    getDatabase().plugins=[...order.flatMap(name=>{const plugin=current.find(value=>value.name===name);return plugin?[plugin]:[]}),...current.filter(plugin=>!order!.includes(plugin.name))]
                }
            },
            getCompatibilityDatabase: getDatabase,
            assertPersistentMutationAllowed: () => undefined,
            getStorageAuthorityEpoch: () => 0,
            getNavigationGeneration: () => 0,
            snapshot: (value: unknown) => JSON.parse(JSON.stringify(value)),
            applyCompatibilityDatabaseLite: (value: Record<string, unknown>) => applyPreparedPluginDatabaseUpdate(value, true),
            mutatePluginStorage: async () => undefined,
            flushPendingData: async () => undefined,
            prepareAuthoritativeDatabaseUpdate: async (value: Record<string, unknown>) => ({
                ...value, ...(value.plugins ? { plugins: await handlePluginInstallViaPlugin(value.plugins as RisuPlugin[]) } : {}),
            }),
        } as any)
    }
    it.each(['lite', 'async'])('preserves every installed record on %s round trips, omissions and replacement attempts', async (kind) => {
        const installed = [makePlugin('caller', '3.0'), makePlugin('other', '3.0')]
        installed[1].realArg = { marker: 'private' }
        getDatabase().plugins = installed
        const before = JSON.parse(JSON.stringify(installed))
        const confirm = vi.spyOn(alertModule, 'alertConfirm').mockResolvedValue(false)
        const api = access()
        const write = (plugins: RisuPlugin[]) => kind === 'lite'
            ? api.setDatabaseLite({ plugins }, allowedDbKeys)
            : api.setDatabase({ plugins }, allowedDbKeys)
        await write(before)
        expect(getDatabase().plugins).toEqual(before)
        await write([{ ...before[1], script: 'replacement', enabled: false, realArg: { marker: 'changed' }, allowedIPC: ['all'] }])
        expect(getDatabase().plugins).toEqual(before)
        expect(confirm).not.toHaveBeenCalled()
    })
    it('does not disclose existing plugin arguments through the installation helper', async () => {
        getDatabase().plugins = [{ ...makePlugin('other', '3.0'), realArg: { marker: 'private' } }]
        expect(await handlePluginInstallViaPlugin([])).toEqual([])
        expect(await handlePluginInstallViaPlugin([makePlugin('other', '3.0')])).toEqual([])
    })
    it('requires async approval for additions and never runs lite additions', async () => {
        const original = makePlugin('original', '3.0')
        getDatabase().plugins = [original]
        const proposed = makePlugin('new', '3.0')
        const confirm = vi.spyOn(alertModule, 'alertConfirm').mockResolvedValue(false)
        const api = access()
        await api.setDatabaseLite({ plugins: [proposed] }, allowedDbKeys)
        expect(confirm).not.toHaveBeenCalled()
        expect(getDatabase().plugins).toEqual([original])
        await api.setDatabase({ plugins: [proposed] }, allowedDbKeys)
        expect(confirm).toHaveBeenCalledTimes(1)
        expect(getDatabase().plugins).toEqual([original])
        confirm.mockResolvedValue(true)
        await api.setDatabase({ plugins: [proposed, proposed] }, allowedDbKeys)
        expect(confirm).toHaveBeenCalledTimes(2)
        expect(getDatabase().plugins).toEqual([original, proposed])
        await loadPlugins()
        expect(vi.mocked(loadV3Plugins).mock.calls.at(-1)![0].map((plugin) => plugin.name)).toEqual(['original', 'new'])
    })
})
