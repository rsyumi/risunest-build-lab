import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { beforeAll, describe, expect, it, vi } from 'vitest'

const captured = vi.hoisted(() => ({ api: null as Record<string, any> | null }))

vi.mock('./factory', () => ({
    SandboxHost: class {
        constructor(api: Record<string, any>) { captured.api = api }
        run() {}
        terminate() {}
        onScriptSettled() {}
    },
}))
vi.mock('../plugins.svelte', () => {
    const oldApis = new Proxy({}, { get: () => vi.fn() })
    const owned = {
        getItem: vi.fn(), setItem: vi.fn(), removeItem: vi.fn(), clear: vi.fn(), key: vi.fn(),
        keys: vi.fn(), length: vi.fn(), snapshot: vi.fn(async () => ({})), mutate: vi.fn(),
    }
    return {
        allowedDbKeys: [],
        applyPreparedPluginDatabaseUpdate: vi.fn(),
        customProviderStore: { subscribe: (run: (value: string[]) => void) => { run([]); return () => undefined }, set: vi.fn() },
        getV2PluginAPIs: () => oldApis,
        handlePluginInstallViaPlugin: vi.fn(),
        pluginStorageStore: { forOwner: () => owned, ownerOf: () => 'declaration-plugin', invalidateOwner: vi.fn(), synchronizeCommittedMutation: vi.fn() },
        pluginV2: { providers: new Map(), providerOptions: new Map(), chatOutput: new Set() },
    }
})
vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => ({ characters: [], plugins: [] }) }))
vi.mock('../pluginSafeClass', () => ({ SafeLocalPluginStorage: class {}, SafeLocalStorage: class {}, tagWhitelist: [] }))
vi.mock('src/ts/stores.svelte', () => ({
    DBState: { db: { characters: [], plugins: [] } },
    selectedCharID: { subscribe(run: (value: number) => void) { run(0); return () => undefined } },
    additionalChatMenu: [], additionalFloatingActionButtons: [], additionalHamburgerMenu: [],
    additionalSettingsMenu: [], bodyIntercepterStore: [], chatPanelStore: [],
}))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(async () => true), alertError: vi.fn(), alertNormal: vi.fn() }))
vi.mock('src/ts/util', () => ({ sleep: vi.fn(async () => undefined) }))
vi.mock('src/lang', () => ({ language: {} }))
vi.mock('src/ts/globalApi.svelte', () => ({ checkCharOrder: vi.fn(), forageStorage: {}, getFetchLogs: vi.fn() }))
vi.mock('src/ts/gui/colorscheme', () => ({ changeColorScheme: vi.fn(), updateColorScheme: vi.fn(), updateTextThemeAndCSS: vi.fn() }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('src/ts/process/mcp/pluginmcp', () => ({ registerMCPModule: vi.fn(), unregisterMCPModule: vi.fn() }))
vi.mock('src/ts/process/files/inlays', () => ({ getInlayAsset: vi.fn() }))
vi.mock('src/ts/translator/translator', () => ({ getLLMCache: vi.fn(), searchLLMCache: vi.fn() }))
vi.mock('src/ts/parser/parser.svelte', () => ({ hasher: vi.fn(async () => 'hash') }))
vi.mock('localforage', () => ({ default: { createInstance: () => ({ getItem: vi.fn(async () => null), setItem: vi.fn() }) } }))
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
    replacePersistentDatabase: vi.fn(),
}))

vi.mock('../pluginDatabaseAccess', () => ({
    createProductionPluginDatabaseAccess: vi.fn(() => ({})),
    linkPluginQueryAbortSignals: vi.fn(),
}))

import { executePluginV3 } from './v3.svelte'

/** Member names of each interface declared at the top level of the plugin declaration file. */
function declaredInterfaces(): Map<string, ts.InterfaceDeclaration> {
    const path = 'src/ts/plugins/apiV3/risuai.d.ts'
    const source = ts.createSourceFile(path, readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true)
    const interfaces = new Map<string, ts.InterfaceDeclaration>()
    source.forEachChild((node) => {
        if (ts.isInterfaceDeclaration(node)) interfaces.set(node.name.text, node)
    })
    return interfaces
}

const memberNames = (declaration: ts.InterfaceDeclaration) =>
    declaration.members.flatMap((member) => member.name ? [member.name.getText()] : [])

// Host methods upstream RisuAI also leaves out of its declaration file.
const undeclaredUpstreamMethods = ['alert', 'alertConfirm', 'alertError', 'installPlugin', 'risuFetch', 'setChatPanel']
// Defined by the guest script itself rather than by the host.
const guestHelpers = ['unwarpSafeArray']

describe('plugin API declarations', () => {
    const interfaces = declaredInterfaces()
    const declared = memberNames(interfaces.get('RisuaiPluginAPI')!)
    let host: Record<string, any>
    let guestVisible: Set<string>

    beforeAll(async () => {
        await executePluginV3({ name: 'declaration-plugin', script: '' } as any)
        host = captured.api!
        const properties = host._getPropertiesForInitialization()
        guestVisible = new Set([...Object.keys(host), ...properties.list, ...Object.keys(host._getAliases()), ...guestHelpers])
    })

    it('declares only members a plugin can reach, and no private member', () => {
        expect(declared.filter((name) => !guestVisible.has(name))).toEqual([])
        expect(declared.filter((name) => name.startsWith('_') || name.startsWith('risunest'))).toEqual([])
    })

    it('declares every public host method', () => {
        const undeclared = Object.keys(host).filter((name) =>
            !name.startsWith('_') && !name.startsWith('risunest') && !declared.includes(name))
        expect(undeclared.sort()).toEqual(undeclaredUpstreamMethods)
    })

    it('exposes the private RisuNest methods to the guest', () => {
        expect(Object.keys(host).filter((name) => name.startsWith('risunest')).sort()).toEqual([
            'risunestCallHostTool',
            'risunestListHostTools',
            'risunestOnChatView',
            'risunestPatchConversation',
            'risunestReadConversationContext',
        ])
    })

    it('backs every declared storage member with a host method', () => {
        const aliases: Record<string, Record<string, string>> = host._getAliases()
        for (const [property, methods] of Object.entries(aliases)) {
            const member = interfaces.get('RisuaiPluginAPI')!.members
                .find((candidate) => candidate.name?.getText() === property) as ts.PropertySignature
            const declaredType = interfaces.get(member.type!.getText())!
            expect(memberNames(declaredType).filter((name) => !Object.hasOwn(methods, name)), property).toEqual([])
            for (const method of Object.values(methods)) expect(typeof host[method], `${property} -> ${method}`).toBe('function')
        }
    })
})
