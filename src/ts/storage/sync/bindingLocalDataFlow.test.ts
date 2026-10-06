import { afterEach, beforeEach, expect, it, vi } from 'vitest'
const f = vi.hoisted(() => ({ invoke: vi.fn(), dialog: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: f.invoke }))
vi.mock('src/ts/alert', () => ({ alertCheckboxConfirm: f.dialog }))
vi.mock('src/ts/process/modules', () => ({ moduleUpdate: vi.fn(), getModules: () => [] }))
vi.mock('src/ts/parser/parser.svelte', () => ({ risuChatParser: (text: string) => text }))
import { language } from 'src/lang'
import { DBState } from 'src/ts/stores.svelte'
import { normalizeDatabaseDefaults, type Database } from '../database.svelte'
import type { SyncBindingDependencies, SyncBindingState } from './bindingFlow'
import { installSyncBindingFlow } from './bindingProduction'
import { bindSyncTarget, registerSyncBindingTransport } from './bindingRegistry'

// The binding table composed through the real native content check, defaults comparison and dialogs.
const target = { kind: 'server', connectionId: 'server' } as const
const residency = { policy: 'remote', localBytes: 0, remoteBytes: 0, remoteObjects: 0, serverBytes: 0, serverObjects: 0, externalObjects: [], unavailableObjects: 0, previousStorageObjects: 0, evictedBytes: 0 }
const factory = () => structuredClone(normalizeDatabaseDefaults({} as Database))
let state: SyncBindingState
let cleanup: (() => void)[] = []

function content(pluginLocalValueCount: string, pluginLocalParticipating: boolean) {
    return { library: factory(), characterCount: '0', managedAliasCount: '0', ordinaryPluginValueCount: '0', hypaValueCount: '0', pluginLocalValueCount, opaqueSharedUnitCount: '0', sharedVariables: {}, pluginLocalParticipating }
}

function install(native: ReturnType<typeof content>, empty: boolean) {
    f.invoke.mockImplementation(async (command: string) => {
        if (command === 'pds_lww_binding_content') return structuredClone(native)
        if (command === 'server_sync_asset_status') return structuredClone(residency)
        throw new Error(`Unexpected command ${command}`)
    })
    const transport = {
        inspectTarget: vi.fn(async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty, previouslyBoundLibrary: false })),
        pullAvailableState: vi.fn(async () => ({ stagingId: 'staging', receiveId: 'receive', targetId: 'target', libraryId: 'library' })),
        replaceFromTarget: vi.fn(async () => {}), publishInitialSharedState: vi.fn(async () => {}),
        resumeBinding: vi.fn(async () => {}), fenceOldJobs: vi.fn(async () => {}),
    }
    const switchTarget = vi.fn<SyncBindingDependencies['native']['switchTarget']>(async (_expected, next, inspection) => {
        state = { ...state, target: next, libraryId: inspection?.libraryId ?? null, targetAuthority: String(Number(state.targetAuthority) + 1) }
        return structuredClone(state)
    })
    cleanup.push(registerSyncBindingTransport(target, transport))
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
    f.invoke.mockReset(); f.dialog.mockReset()
    DBState.db = factory()
    state = { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: 'none', libraryId: null, progress: null }
})
afterEach(() => { for (const dispose of cleanup.reverse()) dispose(); cleanup = [] })

it('asks before replacing a library that holds only plugin-local values, and declining keeps it', async () => {
    const { transport, switchTarget } = install(content('2', false), false)
    f.dialog.mockResolvedValue({ confirmed: false, checked: false })
    expect(await bindSyncTarget(target)).toEqual({ kind: 'cancelled' })
    expect(f.dialog).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ title: language.lwwSync.replaceTitle, description: language.lwwSync.replaceDescription, requireChecked: true }))
    expect(transport.pullAvailableState).not.toHaveBeenCalled(); expect(switchTarget).not.toHaveBeenCalled()
})

it('replaces a factory library from the target without asking', async () => {
    const { transport, switchTarget } = install(content('0', false), false)
    expect(await bindSyncTarget(target)).toMatchObject({ kind: 'bound', action: 'replaced' })
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
