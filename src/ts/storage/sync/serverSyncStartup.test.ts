// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SyncBindingState } from './bindingFlow'

const f = vi.hoisted(() => ({
    invoke: vi.fn(), offline: true, repairError: undefined as unknown, activationError: undefined as unknown,
    binding: undefined as unknown as SyncBindingState,
    runtime: { revision: 0, getStorageAuthorityEpoch: () => 'storage', subscribeActiveConversationViewportSource: () => () => {}, setActivatedLibraryRecoveryLifecycle() {}, markCommittedWorkingSetRefreshRequired() {} },
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: f.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {} }))
vi.mock('src/ts/platform', () => ({ isTauri: true }))
vi.mock('src/ts/stores.svelte', () => ({ selectedCharID: { subscribe: (run: (value: number) => void) => { run(-1); return () => {} } } }))
vi.mock('../database.svelte', () => ({ getDatabase: () => ({ characters: [] }) }))
vi.mock('./bindingDialog', () => ({ confirmSyncBindingReplacement: async () => true }))
vi.mock('./bindingLocalData', () => ({ hasLocalBindingData: async () => false, hasLocalSharedBindingData: async () => false }))
vi.mock('../persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: () => f.runtime,
    withPausedPersistentWrites: async (_reason: string, operation: (token: unknown) => Promise<unknown>) => operation({ id: 'paused' }),
    beginActivatedLibraryGuard: () => ({ complete() {}, async abortUnchanged() {} }),
    refreshActivatedLibraryUnderPause: async () => ({ projection: 'applied' }), applyPersistentLwwReceive: async () => {},
}))
vi.mock('../committedWorkingSetContinuation', () => ({ registerCommittedWorkingSetContinuation: vi.fn() }))
vi.mock('src/ts/plugins/apiV3/v3.svelte', () => ({ fencePluginExecutionForAuthorityReplacement: async () => {}, invalidatePluginCachesAfterAuthorityReplacement: async () => {}, restartPluginsAfterAuthorityReplacement: async () => {} }))
vi.mock('../persistentRevisionEvents', () => ({ subscribeLocalPersistentRevision: () => () => {} }))
vi.mock('../generatingConversationRegistry', () => ({ generatingConversations: { snapshot: () => [] } }))
vi.mock('src/ts/alert', () => ({ alertConfirm: async () => false }))
vi.mock('@lucide/svelte', () => ({ LoaderCircleIcon: () => {} }))
vi.mock('./serverSyncRegistrationInbox', () => ({ serverRegistrationInbox: { changed: { subscribe: () => () => {} }, releaseConsumed() {}, take: () => undefined } }))
vi.mock('./serverSyncQr', () => ({ canScanServerRegistration: false, createServerQrScanner: () => ({ cancel() {} }) }))
vi.mock('./serverAssetResidency', () => ({ getAssetResidencyStatus: async () => undefined, setAssetResidencyPolicy: vi.fn(), evictLocalAssets: vi.fn(), cancelAssetResidencyOperation: vi.fn() }))
vi.mock('src/lang', () => ({ language: { loading: 'Loading', lwwSync: { concurrentEditNotice: 'Concurrent edits', clockBlocked: 'Clock blocked', writerCollision: 'Writer blocked' }, risuNest: { serverSync: { title: 'Sync', description: 'Sync library', syncNow: 'Sync now', disconnect: 'Disconnect', disconnected: 'Stopped', ready: 'Ready', errorHelp: 'Connection failed', management: {}, residency: {} } } } }))

let production: typeof import('./serverSyncProduction')
let ui: typeof import('svelte')
let component: ReturnType<(typeof import('svelte'))['mount']> | undefined
const settle = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); await ui.tick() }
const commands = () => f.invoke.mock.calls.map(([command]) => command)
const button = (text: string) => [...document.querySelectorAll('button')].find(value => value.textContent?.trim() === text)
beforeEach(async () => {
    vi.resetModules(); f.invoke.mockReset(); f.offline = true; f.repairError = undefined; f.activationError = undefined
    f.binding = { target: { kind: 'server', connectionId: 'server' }, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true })
    f.invoke.mockImplementation(async (command, args) => {
        if (command === 'pds_lww_binding_state') return structuredClone(f.binding)
        if (command === 'server_sync_status') return { configured: true, writerId: 'writer', bindingAuthority: f.binding.targetAuthority, libraryId: 'library', deviceId: 'device' }
        if (command === 'server_sync_lww_activate' && f.activationError) throw f.activationError
        if (command === 'server_sync_lww_activate' && f.offline) throw { code: 'server-unreachable', status: 503, retryable: true }
        if (command === 'server_sync_lww_retry' && f.repairError) throw f.repairError
        if (command === 'pds_lww_switch_target') {
            expect(args.request).toMatchObject({ bindingAuthority: f.binding.targetAuthority, expectedSelectionEpoch: f.binding.selectionEpoch })
            f.binding = { ...f.binding, target: args.request.target, targetAuthority: String(Number(f.binding.targetAuthority) + 1), selectionEpoch: 'disconnected' }
            return structuredClone(f.binding)
        }
        return null
    })
    production = await import('./serverSyncProduction')
    ui = await import('svelte')
    production.initializeNativeSyncBindings()
})
afterEach(async () => { if (component) await ui.unmount(component); component = undefined; production.disposeNativeSyncBindings(); document.body.replaceChildren() })
async function offlineStartup() {
    await expect(production.installServerSyncProduction()).resolves.toBeUndefined()
    expect(commands()).toContain('server_sync_lww_fence')
    expect(commands()).not.toContain('server_sync_notify_start')
    expect(commands()).not.toContain('server_sync_lww_retry')
    expect(commands()).not.toContain('pds_lww_switch_target')
    expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, running: false, paused: true, error: 'server-unreachable' })
}
async function settings() { const Settings = (await import('src/lib/Setting/Pages/ServerSyncSettings.svelte')).default; component = ui.mount(Settings, { target: document.body }); await settle() }

describe('persisted server offline startup', () => {
    it('keeps real retry and disconnect settings actions after the actual installer fences a failed activation', async () => {
        await offlineStartup(); await settings()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: true, error: 'server-unreachable' })
        expect(button('Sync now')).toBeDefined(); expect(button('Disconnect')).toBeDefined()
        f.offline = false
        button('Sync now')!.click(); await settle()
        expect(commands().filter(command => command === 'server_sync_lww_activate')).toHaveLength(2)
        expect(f.invoke).toHaveBeenCalledWith('server_sync_lww_retry', { request: { bindingAuthority: '4', requestId: expect.any(String) } })
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: false, error: '' })
    })
    it('disconnects the actual persisted binding while activation remains offline, without starting services', async () => {
        await offlineStartup(); await settings()
        button('Disconnect')!.click(); await settle()
        expect(f.binding.target).toEqual({ kind: 'none' })
        expect(production.getServerSyncController().snapshot().status.bound).toBe(false)
        expect(button('Sync now')).toBeUndefined(); expect(button('Disconnect')).toBeUndefined()
        expect(commands()).not.toContain('server_sync_lww_retry'); expect(commands()).not.toContain('server_sync_notify_start')
    })
    it('rejects a different current target before retry can send the old server header', async () => {
        await offlineStartup(); f.invoke.mockClear()
        f.binding = { ...f.binding, target: { kind: 'external', connectionId: 'other' }, targetAuthority: '8', selectionEpoch: 'other' }
        await expect(production.retryServerSync()).rejects.toThrow('Sync binding is unavailable')
        expect(commands()).not.toContain('server_sync_lww_activate'); expect(commands()).not.toContain('server_sync_lww_retry')
    })
    it('keeps a repeated offline retry stopped and recoverable without reinstalling or changing registration', async () => {
        await offlineStartup(); await production.installServerSyncProduction()
        await expect(production.retryServerSync()).rejects.toMatchObject({ code: 'server-unreachable', status: 503, retryable: true })
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: true, error: 'server-unreachable' })
        expect(commands()).not.toContain('server_sync_lww_retry'); expect(commands()).not.toContain('server_sync_configure')
        f.offline = false; await production.retryServerSync()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: false, error: '' })
        expect(commands().filter(command => command === 'server_sync_lww_activate')).toHaveLength(3)
    })
    it('preserves native accepted-clock refusal after resumed activation and does not recheck it on foreground return', async () => {
        await offlineStartup(); f.offline = false; f.repairError = { code: 'accepted-clock-correction-required', retryable: false }
        await expect(production.retryServerSync()).rejects.toEqual(f.repairError)
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: true, error: 'accepted-clock-correction-required' })
        const retried = commands().filter(command => command === 'server_sync_lww_retry').length
        Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true }); document.dispatchEvent(new Event('visibilitychange')); await settle()
        expect(commands().filter(command => command === 'server_sync_lww_retry')).toHaveLength(retried)
        expect(commands()).not.toContain('server_sync_notify_start')
    })
    it.each([
        { code: 'binding-authority-stale', retryable: false },
        { code: 'server-unreachable', retryable: false },
        new Error('Local activation state is invalid'),
    ])('rejects non-recoverable or unknown activation failures after fencing startup jobs (%j)', async failure => {
        f.activationError = failure
        const before = structuredClone(f.binding)
        await expect(production.installServerSyncProduction()).rejects.toBe(failure)
        expect(commands()).toContain('server_sync_lww_fence')
        expect(commands()).not.toContain('server_sync_notify_start')
        expect(commands()).not.toContain('pds_lww_switch_target')
        expect(f.binding).toEqual(before)
    })
})
