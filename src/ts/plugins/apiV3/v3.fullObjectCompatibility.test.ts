import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { Database } from '../../storage/database.svelte'

const fixture = vi.hoisted(() => ({
    api: null as Record<string, (...args: any[]) => any> | null,
    selectedIndex: 0,
    databaseAccessDependencies: null as null | { getSelectedCharacterId(): string | null },
    pluginPermissionReads: vi.fn(),
    permissionValues: new Map<string, unknown>(),
    replacers: new Map<string, Set<Function>>(),
    listeners: new Set<Function>(),
    scopedAccess: {
        getFullObjectSnapshotStream: async (target: { characterIndex?: number; chatIndex?: number }, context: unknown) => {
            const access = fixture.scopedAccess
            const value = await (target.characterIndex === undefined
                ? access.getCurrentCharacter(context)
                : target.chatIndex === undefined
                    ? access.getCharacterFromIndex(target.characterIndex, context)
                    : access.getChatFromIndex(target.characterIndex, target.chatIndex, context))
            return value
        },
        getCurrentCharacter: vi.fn(),
        getCharacterFromIndex: vi.fn(),
        getChatFromIndex: vi.fn(),
        setCurrentCharacter: vi.fn(),
        setCharacterToIndex: vi.fn(),
        setChatToIndex: vi.fn(),
    },
    database: {
        characters: [
            {
                type: 'character', chaId: 'active', name: 'Active', chatPage: 0,
                chats: [{ id: 'active-chat', name: 'Live', message: [{ role: 'char', data: 'a' }] }],
            },
            {
                type: 'character', chaId: 'trashed', name: 'Trash', trashTime: 1, chatPage: 0,
                chats: [{ id: 'trash-chat', name: 'Trash chat', message: [{ role: 'user', data: 'b' }] }],
            },
        ],
        plugins: [{ name: 'contract-plugin', script: '' }],
    } as unknown as Database,
}))

const ownedStorageStub = {
    getItem: vi.fn(), setItem: vi.fn(), removeItem: vi.fn(), clear: vi.fn(),
    key: vi.fn(), keys: vi.fn(), length: vi.fn(), snapshot: vi.fn(async () => ({})),
    mutate: vi.fn(),
}

vi.mock('../plugins.svelte', async () => {
    const { writable } = await import('svelte/store')
    const unrelated = vi.fn()
    const oldApis = new Proxy({
        getChar: () => {
            const character = fixture.database.characters[fixture.selectedIndex]
            return character === undefined ? undefined : structuredClone(character)
        },
        setChar: (character: Database['characters'][number]) => {
            if (fixture.database.characters[fixture.selectedIndex]) {
                fixture.database.characters[fixture.selectedIndex] = character
            }
        },
        addRisuChatListener: (_mode: string, listener: Function) => fixture.listeners.add(listener),
        removeRisuChatListener: (_mode: string, listener: Function) => fixture.listeners.delete(listener),
        getArg: (arg: string) => { const [name, key] = arg.split('::'); return fixture.database.plugins.find((plugin) => plugin.name === name)?.realArg[key] },
        setArg: (arg: string, value: string) => { const [name, key] = arg.split('::'); const plugin = fixture.database.plugins.find((plugin) => plugin.name === name); if (plugin) plugin.realArg[key] = value },
        addRisuReplacer: (name: string, callback: Function) => { const callbacks = fixture.replacers.get(name) ?? new Set(); callbacks.add(callback); fixture.replacers.set(name, callbacks) },
        removeRisuReplacer: (name: string, callback: Function) => fixture.replacers.get(name)?.delete(callback),
        safeLocalStorage: {
            getItem: unrelated,
            setItem: unrelated,
            removeItem: unrelated,
            clear: unrelated,
            key: unrelated,
            keys: unrelated,
            length: unrelated,
        },
    }, { get: (target, property) => Reflect.get(target, property) ?? unrelated })
    return {
        allowedDbKeys: [],
        applyPreparedPluginDatabaseUpdate: vi.fn(),
        customProviderStore: writable<string[]>([]),
        getV2PluginAPIs: () => oldApis,
        handlePluginInstallViaPlugin: vi.fn(),
        pluginStorageStore: {
            forOwner: () => ownedStorageStub,
            ownerOf: () => 'test-plugin',
            invalidateOwner: vi.fn(),
            synchronizeCommittedMutation: vi.fn(),
            snapshot: vi.fn(async () => []), mutate: unrelated, invalidate: unrelated,
            getItem: unrelated, setItem: unrelated, removeItem: unrelated,
            clear: unrelated, key: unrelated, keys: unrelated, length: unrelated,
        },
        pluginV2: { providers: new Map(), providerOptions: new Map() },
    }
})
vi.mock('./factory', () => ({
    SandboxHost: class {
        constructor(api: Record<string, (...args: any[]) => any>) { fixture.api = api }
        run() {}
        terminate() {}
    },
}))
vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => fixture.database }))
vi.mock('../pluginSafeClass', () => ({ SafeLocalPluginStorage: class {}, SafeLocalStorage: class {}, tagWhitelist: [] }))
vi.mock('src/ts/stores.svelte', () => ({
    DBState: {
        get db() { return fixture.database },
        set db(value) { fixture.database = value },
    },
    selectedCharID: {
        subscribe(run: (value: number) => void) {
            run(fixture.selectedIndex)
            return () => undefined
        },
    },
    additionalChatMenu: [], additionalFloatingActionButtons: [], additionalHamburgerMenu: [],
    additionalSettingsMenu: [], bodyIntercepterStore: [], chatPanelStore: [],
}))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(async () => true), alertError: vi.fn(), alertNormal: vi.fn() }))
vi.mock('src/ts/util', () => ({ sleep: vi.fn(async () => undefined) }))
vi.mock('src/lang', () => ({ language: {
    fetchLogConsent: '{}', getFullDatabaseConsent: '{}', mainDomAccessConsent: '{}',
    replacerPermissionConsent: '{}', providerPermissionConsent: '{}', sendChatConsent: '{}',
    inlayPermissionConsent: '{}',
} }))
vi.mock('src/ts/globalApi.svelte', () => ({ checkCharOrder: vi.fn(), forageStorage: {}, getFetchLogs: vi.fn() }))
vi.mock('src/ts/gui/colorscheme', () => ({ changeColorScheme: vi.fn(), updateColorScheme: vi.fn(), updateTextThemeAndCSS: vi.fn() }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('src/ts/process/mcp/pluginmcp', () => ({ registerMCPModule: vi.fn(), unregisterMCPModule: vi.fn() }))
vi.mock('src/ts/process/files/inlays', () => ({ getInlayAsset: vi.fn() }))
vi.mock('src/ts/translator/translator', () => ({ getLLMCache: vi.fn(), searchLLMCache: vi.fn() }))
vi.mock('src/ts/parser/parser.svelte', () => ({ hasher: vi.fn(async (bytes: Uint8Array) => 'hash:' + new TextDecoder().decode(bytes)) }))
vi.mock('localforage', () => ({ default: { createInstance: () => ({
    getItem: fixture.pluginPermissionReads,
    setItem: vi.fn(async (key: string, value: unknown) => { fixture.permissionValues.set(key, value) }),
    clear: vi.fn(async () => fixture.permissionValues.clear()),
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
vi.mock('../pluginDatabaseAccess', () => ({
    createProductionPluginDatabaseAccess: vi.fn((dependencies) => {
        fixture.databaseAccessDependencies = dependencies
        return fixture.scopedAccess
    }),
    linkPluginQueryAbortSignals: vi.fn(),
}))

import { executePluginV3, loadV3Plugins, resetAllPluginPermissions, customV3ProviderMetaStore } from './v3.svelte'
import { alertConfirm } from 'src/ts/alert'
import { customProviderStore, pluginV2 } from '../plugins.svelte'
import { get } from 'svelte/store'
import { getPluginPermissionStore } from 'src/ts/storage/nativePluginPermissions'

function resetDatabase(): void {
    fixture.database = {
        characters: [
            {
                type: 'character', chaId: 'active', name: 'Active', chatPage: 0,
                chats: [{ id: 'active-chat', name: 'Live', message: [{ role: 'char', data: 'a' }] }],
            },
            {
                type: 'character', chaId: 'trashed', name: 'Trash', trashTime: 1, chatPage: 0,
                chats: [{ id: 'trash-chat', name: 'Trash chat', message: [{ role: 'user', data: 'b' }] }],
            },
        ],
        plugins: [{ name: 'contract-plugin', script: '' }],
    } as unknown as Database
}

describe('Plugin v3 full-object access routing', () => {
    beforeEach(async () => {
        vi.clearAllMocks()
        resetDatabase()
        fixture.api = null
        fixture.selectedIndex = 0
        fixture.listeners.clear()
        fixture.scopedAccess.getCurrentCharacter.mockReset()
        fixture.scopedAccess.getCharacterFromIndex.mockReset()
        fixture.scopedAccess.getChatFromIndex.mockReset()
        fixture.scopedAccess.setCurrentCharacter.mockReset()
        fixture.scopedAccess.setCharacterToIndex.mockReset()
        fixture.scopedAccess.setChatToIndex.mockReset()
        await executePluginV3({
            name: `contract-plugin-${crypto.randomUUID()}`,
            script: '',
        } as any)
    })

    it('routes getters through scoped access', async () => {
        const active = structuredClone(fixture.database.characters[0])
        const trashed = structuredClone(fixture.database.characters[1])
        fixture.scopedAccess.getCurrentCharacter.mockResolvedValue(active)
        fixture.scopedAccess.getCharacterFromIndex.mockResolvedValue(trashed)
        fixture.scopedAccess.getChatFromIndex.mockResolvedValue(trashed.chats[0])
        const api = fixture.api!

        await expect(api.getCharacter()).resolves.toMatchObject({ chaId: 'active' })
        await expect(api.getCharacterFromIndex(1)).resolves.toMatchObject({ chaId: 'trashed' })
        await expect(api.getChatFromIndex(1, 0)).resolves.toMatchObject({ id: 'trash-chat' })

        expect(fixture.pluginPermissionReads).not.toHaveBeenCalledWith(
            expect.stringContaining('_db'),
        )

        expect(fixture.scopedAccess.getCurrentCharacter.mock.calls[0][0]).toMatchObject({
            pluginName: expect.stringContaining('contract-plugin-'),
            signal: expect.any(AbortSignal),
        })
        expect(fixture.scopedAccess.getCharacterFromIndex).toHaveBeenCalledWith(
            1,
            expect.objectContaining({ pluginName: expect.stringContaining('contract-plugin-') }),
        )
        expect(fixture.scopedAccess.getChatFromIndex).toHaveBeenCalledWith(
            1,
            0,
            expect.objectContaining({ pluginName: expect.stringContaining('contract-plugin-') }),
        )
    })

    it('resolves a selected character independently of conversation validity', async () => {
        fixture.scopedAccess.getCurrentCharacter.mockResolvedValue(
            structuredClone(fixture.database.characters[0]),
        )
        await fixture.api!.getCharacter()
        fixture.database.characters[0].chats = []
        fixture.database.characters[0].chatPage = 99

        expect(fixture.databaseAccessDependencies?.getSelectedCharacterId()).toBe('active')
    })

    it('routes setters through scoped access', async () => {
        const api = fixture.api!
        const character = structuredClone(fixture.database.characters[1])
        const chat = structuredClone(fixture.database.characters[1].chats[0])

        await api.setChar(character)
        await api.setCharacter(character)
        await api.setCharacterToIndex(1, character)
        await api.setChatToIndex(1, 0, chat)

        expect(fixture.scopedAccess.setCurrentCharacter).toHaveBeenCalledTimes(2)
        expect(fixture.scopedAccess.setCurrentCharacter).toHaveBeenCalledWith(
            character,
            expect.objectContaining({ pluginName: expect.stringContaining('contract-plugin-') }),
        )
        expect(fixture.scopedAccess.setCharacterToIndex).toHaveBeenCalledWith(
            1,
            character,
            expect.objectContaining({ pluginName: expect.stringContaining('contract-plugin-') }),
        )
        expect(fixture.scopedAccess.setChatToIndex).toHaveBeenCalledWith(
            1,
            0,
            chat,
            expect.objectContaining({ pluginName: expect.stringContaining('contract-plugin-') }),
        )
        expect(fixture.database.characters[1].name).toBe('Trash')
        expect(fixture.pluginPermissionReads).not.toHaveBeenCalledWith(
            expect.stringContaining('_db'),
        )
    })
})


describe('Plugin v3 runtime consent and ownership', () => {
    beforeEach(async () => {
        await loadV3Plugins([])
        fixture.permissionValues.clear()
        fixture.pluginPermissionReads.mockImplementation(async (key: string) => fixture.permissionValues.get(key) ?? null)
        vi.mocked(alertConfirm).mockReset().mockResolvedValue(true)
        fixture.replacers.clear()
        fixture.database.plugins = [
            { name: 'owner', script: 'original', realArg: { marker: 'own' } },
            { name: 'other', script: 'other-code', realArg: { marker: 'foreign' } },
        ] as any
    })
    afterEach(async () => { await loadV3Plugins([]); vi.restoreAllMocks() })
    async function start(name = 'owner', script = 'original') {
        await executePluginV3({ name, script } as any)
        return fixture.api!
    }

    it('binds deprecated arguments to the executing owner', async () => {
        const api = await start()
        expect(api.getArg('owner::marker')).toBe('own')
        api.setArg('owner::marker', 'updated')
        expect(api.getArg('owner::marker')).toBe('updated')
        expect(api.getArg('other::marker')).toBeUndefined()
        api.setArg('other::marker', 'stolen')
        expect(fixture.database.plugins[1].realArg.marker).toBe('foreign')
    })

    it('hashes executing code rather than a mutable installed record', async () => {
        const api = await start()
        fixture.database.plugins[0].script = 'granted'
        fixture.permissionValues.set('hash:granted_mainDom', true)
        vi.mocked(alertConfirm).mockResolvedValue(false)
        expect(await api.requestPluginPermission('mainDom')).toBe(false)
        expect(alertConfirm).toHaveBeenCalledOnce()
    })

    it('requires a fresh grant when the same name executes changed source', async () => {
        const first = await start()
        expect(await first.requestPluginPermission('mainDom')).toBe(true)
        await loadV3Plugins([])
        const next = await start('owner', 'changed')
        expect(await next.requestPluginPermission('mainDom')).toBe(true)
        expect(alertConfirm).toHaveBeenCalledTimes(2)
    })

    it('waits for an in-flight reset before reading grants into a new epoch', async () => {
        const api = await start()
        await api.requestPluginPermission('mainDom')
        const store = getPluginPermissionStore()
        const original = store.clearAll.bind(store)
        let release!: () => void
        const gate = new Promise<void>((resolve) => { release = resolve })
        const clear = vi.spyOn(store, 'clearAll').mockImplementation(async () => { await gate; await original() })
        const reset = resetAllPluginPermissions()
        await vi.waitFor(() => expect(clear).toHaveBeenCalledOnce())
        let settled = false
        const permission = api.requestPluginPermission('mainDom').then((value) => { settled = true; return value })
        await new Promise((resolve) => setTimeout(resolve, 0))
        expect(settled).toBe(false)
        release()
        await reset
        expect(await permission).toBe(true)
        expect(alertConfirm).toHaveBeenCalledTimes(2)
    })

    it('drops denied runtime decisions when the same name loads new code', async () => {
        vi.mocked(alertConfirm).mockResolvedValue(false)
        const first = await start()
        expect(await first.requestPluginPermission('mainDom')).toBe(false)
        expect(await first.requestPluginPermission('mainDom')).toBe(false)
        await loadV3Plugins([])
        vi.mocked(alertConfirm).mockResolvedValue(true)
        const next = await start('owner', 'changed')
        expect(await next.requestPluginPermission('mainDom')).toBe(true)
        expect(alertConfirm).toHaveBeenCalledTimes(2)
    })

    it('honors periodic upfront grants across reload and rechecks at the deadline', async () => {
        let now = 1000
        vi.spyOn(Date, 'now').mockImplementation(() => now)
        const first = await start()
        expect(await first.requestPluginPermission('db')).toBe(true)
        await loadV3Plugins([])
        const next = await start()
        expect(await next.requestPluginPermission('db')).toBe(true)
        expect(alertConfirm).toHaveBeenCalledOnce()
        now += 7 * 24 * 60 * 60 * 1000 - 1
        expect(await next.requestPluginPermission('db')).toBe(true)
        expect(alertConfirm).toHaveBeenCalledOnce()
        now += 1
        expect(await next.requestPluginPermission('db')).toBe(true)
        expect(alertConfirm).toHaveBeenCalledTimes(2)
        await resetAllPluginPermissions()
        expect(await next.requestPluginPermission('db')).toBe(true)
        expect(alertConfirm).toHaveBeenCalledTimes(3)
    })

    it('removes both replacer kinds and rejects registrations after unload', async () => {
        const api = await start()
        const callback = vi.fn()
        await api.addRisuReplacer('beforeRequest', callback)
        await api.addRisuReplacer('afterRequest', callback)
        expect(fixture.replacers.get('beforeRequest')?.size).toBe(1)
        await loadV3Plugins([])
        expect(fixture.replacers.get('beforeRequest')?.size).toBe(0)
        expect(fixture.replacers.get('afterRequest')?.size).toBe(0)
        const next = await start()
        let approve!: (value: boolean) => void
        await resetAllPluginPermissions()
        vi.mocked(alertConfirm).mockImplementation(() => new Promise((resolve) => { approve = resolve }))
        const pending = next.addRisuReplacer('beforeRequest', callback)
        await vi.waitFor(() => expect(approve).toBeTypeOf('function'))
        await loadV3Plugins([])
        approve(true)
        await pending
        expect(fixture.replacers.get('beforeRequest')?.size).toBe(0)
    })

    it('replaces provider metadata and cleans up the currently owned registration', async () => {
        const api = await start()
        api.addProvider('provider', vi.fn(), { model: { name: 'First' } })
        api.addProvider('provider', vi.fn(), { model: { name: 'Latest' } })
        expect(get(customProviderStore)).toEqual(['provider'])
        expect(customV3ProviderMetaStore.filter((model) => model.id === 'pluginmodel:::provider').map((model) => model.name)).toEqual(['Latest'])
        await loadV3Plugins([])
        expect(get(customProviderStore)).toEqual([])
        expect(customV3ProviderMetaStore).toEqual([])
        expect(pluginV2.providerOptions.has('provider')).toBe(false)
        expect(pluginV2.providers.has('provider')).toBe(false)
    })
})
