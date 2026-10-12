import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
const f = vi.hoisted(() => ({ invoke: vi.fn(), dialog: vi.fn(), characters: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: f.invoke }))
vi.mock('src/ts/alert', () => ({ alertCheckboxConfirm: f.dialog }))
vi.mock('../libraryCharacters', () => ({ hasLibraryCharacters: f.characters }))
vi.mock('src/ts/process/modules', () => ({ moduleUpdate: vi.fn(), getModules: () => [] }))
vi.mock('src/ts/parser/parser.svelte', () => ({ risuChatParser: (text: string) => text }))
import { language } from 'src/lang'
import { DBState } from 'src/ts/stores.svelte'
import { normalizeDatabaseDefaults, type Database } from '../database.svelte'
import type { InspectedSyncTarget, SyncBindingDependencies, SyncBindingState } from './bindingFlow'
import { installSyncBindingFlow } from './bindingProduction'
import { bindSyncTarget, registerSyncBindingTransport } from './bindingRegistry'

// The binding table composed through the real character check wiring, native content check and dialogs.
const target = { kind: 'server', connectionId: 'server' } as const
const residency = { policy: 'remote', localBytes: 0, remoteBytes: 0, remoteObjects: 0, serverBytes: 0, serverObjects: 0, externalObjects: [], unavailableObjects: 0, previousStorageObjects: 0, evictedBytes: 0 }
const factory = () => structuredClone(normalizeDatabaseDefaults({} as Database))
let state: SyncBindingState
let cleanup: (() => void)[] = []

function content(pluginLocalValueCount: string, pluginLocalParticipating: boolean) {
    return { library: factory(), characterCount: '0', managedAliasCount: '0', ordinaryPluginValueCount: '0', hypaValueCount: '0', pluginLocalValueCount, opaqueSharedUnitCount: '0', sharedVariables: {}, pluginLocalParticipating }
}

function install(native: Omit<ReturnType<typeof content>, 'library'> & { library: object }, empty: boolean, at: typeof target | { kind: 'external'; connectionId: string } = target, inspection: Partial<InspectedSyncTarget> = {}) {
    f.invoke.mockImplementation(async (command: string) => {
        if (command === 'pds_lww_binding_content') return structuredClone(native)
        if (command === 'server_sync_asset_status') return structuredClone(residency)
        throw new Error(`Unexpected command ${command}`)
    })
    const transport = {
        inspectTarget: vi.fn(async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty, previouslyBoundLibrary: false, ...inspection })),
        pullAvailableState: vi.fn(async () => ({ stagingId: 'staging', receiveId: 'receive', targetId: 'target', libraryId: 'library' })),
        replaceFromTarget: vi.fn(async () => {}), publishInitialSharedState: vi.fn(async () => {}),
        resumeBinding: vi.fn(async () => {}), fenceOldJobs: vi.fn(async () => {}),
        prepareNewDeviceBinding: vi.fn(async () => ({ authorizationId: 'authorization', writerId: 'writer' })),
        replaceAsNewDevice: vi.fn(async () => ({ revision: 1, writerId: 'writer', bindingAuthority: '2' })),
        resumeNewDeviceBinding: vi.fn(async () => {}),
    }
    const switchTarget = vi.fn<SyncBindingDependencies['native']['switchTarget']>(async (_expected, next, inspection) => {
        state = { ...state, target: next, libraryId: inspection?.libraryId ?? null, targetAuthority: String(Number(state.targetAuthority) + 1) }
        return structuredClone(state)
    })
    cleanup.push(registerSyncBindingTransport(at, transport))
    const installed = installSyncBindingFlow({
        native: { state: async () => structuredClone(state), assertAuthority: async () => {}, switchTarget },
        plugins: { fenceExecution: async () => {}, invalidateCaches: async () => {}, restart: async () => {} },
        withPausedWrites: operation => operation(), beginActivatedLibraryGuard: () => ({ complete() {}, async abortUnchanged() {} }),
        refreshActivatedLibrary: async () => {}, recovery: { setLifecycle() {}, registerFailure() {} },
    })
    cleanup.push(installed.dispose)
    return { transport, switchTarget }
}

beforeEach(() => {
    f.invoke.mockReset(); f.dialog.mockReset(); f.characters.mockReset().mockResolvedValue(false)
    DBState.db = factory()
    state = { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: 'none', libraryId: null, progress: null }
})
afterEach(() => { for (const dispose of cleanup.reverse()) dispose(); cleanup = [] })

it('asks before replacing a library that holds a character, and declining keeps it', async () => {
    f.characters.mockResolvedValue(true)
    const { transport, switchTarget } = install(content('0', false), false)
    f.dialog.mockResolvedValue({ confirmed: false, checked: false })
    expect(await bindSyncTarget(target)).toEqual({ kind: 'cancelled' })
    expect(f.dialog).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ title: language.lwwSync.replaceTitle, description: language.lwwSync.replaceDescription, requireChecked: true }))
    expect(transport.pullAvailableState).not.toHaveBeenCalled(); expect(switchTarget).not.toHaveBeenCalled()
})

it.each([target, { kind: 'external', connectionId: 'external' } as const])('replaces a library without characters from $kind without asking, whatever else it holds', async at => {
    const { transport, switchTarget } = install({ ...content('2', true), ordinaryPluginValueCount: '3', managedAliasCount: '1', library: { ...factory(), language: 'ko', loreBook: [{ name: 'Lore', data: [] }] } }, false, at)
    expect(await bindSyncTarget(at)).toMatchObject({ kind: 'bound', action: 'replaced' })
    expect(f.dialog).not.toHaveBeenCalled()
    expect(transport.replaceFromTarget).toHaveBeenCalledOnce(); expect(switchTarget).toHaveBeenCalledOnce()
})

it.each([false, true])('publishes plugin-local values to an empty target only while this device takes part in plugin-local sync (%s)', async participating => {
    const { transport, switchTarget } = install(content('2', participating), true)
    expect(await bindSyncTarget(target)).toMatchObject({ kind: 'bound', action: 'initialized' })
    expect(f.dialog).not.toHaveBeenCalled()
    expect(switchTarget).toHaveBeenCalledOnce()
    expect(switchTarget.mock.calls[0][4]).toBe(participating)
    expect(transport.publishInitialSharedState).toHaveBeenCalledTimes(participating ? 1 : 0)
})

describe('a restored server', () => {
    const bound = () => { state = { target, targetAuthority: '1', selectionEpoch: 'bound', libraryId: 'library', progress: null } }
    it('replaces a bound device without characters without asking', async () => {
        bound()
        const { transport } = install(content('0', false), false, target, { previouslyBoundLibrary: true, serverRestored: true })
        transport.replaceAsNewDevice.mockImplementation(async () => {
            state = { ...state, targetAuthority: '2' }
            return { revision: 1, writerId: 'writer', bindingAuthority: '2' }
        })
        expect(await bindSyncTarget(target)).toMatchObject({ kind: 'bound', action: 'new-device' })
        expect(f.dialog).not.toHaveBeenCalled()
        expect(transport.replaceAsNewDevice).toHaveBeenCalledOnce()
    })
    it('asks before it replaces a bound device that holds a character, and declining keeps it', async () => {
        bound()
        f.characters.mockResolvedValue(true)
        const { transport } = install(content('0', false), false, target, { previouslyBoundLibrary: true, serverRestored: true })
        f.dialog.mockResolvedValue({ confirmed: false, checked: false })
        expect(await bindSyncTarget(target)).toEqual({ kind: 'cancelled' })
        expect(f.dialog).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ title: language.lwwSync.replaceTitle, description: language.lwwSync.serverRestoredDescription, requireChecked: true }))
        expect(transport.prepareNewDeviceBinding).not.toHaveBeenCalled(); expect(transport.replaceAsNewDevice).not.toHaveBeenCalled()
    })
})
