// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SyncBindingState } from './bindingFlow'

const f = vi.hoisted(() => ({
    invoke: vi.fn(), offline: true, repairError: undefined as unknown, activationError: undefined as unknown, fenceError: undefined as unknown,
    configured: true, pending: null as unknown, inspectError: undefined as unknown,
    sharedData: false, marker: false, queued: 0, outbox: 0, pushed: 0,
    binding: undefined as unknown as SyncBindingState,
    runtime: { revision: 0, getStorageAuthorityEpoch: () => 'storage', subscribeActiveConversationViewportSource: () => () => {}, captureSelectedConversationTarget: () => null, setActivatedLibraryRecoveryLifecycle() {}, markCommittedWorkingSetRefreshRequired() {} },
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: f.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {} }))
vi.mock('src/ts/platform', () => ({ isTauri: true }))
vi.mock('src/ts/stores.svelte', () => ({ selectedCharID: { subscribe: (run: (value: number) => void) => { run(-1); return () => {} } } }))
vi.mock('../database.svelte', () => ({ getDatabase: () => ({ characters: [] }) }))
vi.mock('./bindingDialog', () => ({ confirmSyncBindingReplacement: async () => true, confirmPreviousStorageFiles: async () => 'connect', downloadPreviousStorageFiles: async () => {} }))
vi.mock('./bindingLocalData', () => ({ hasLocalBindingData: async () => false, hasLocalSharedBindingData: async () => f.sharedData }))
vi.mock('../persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: () => f.runtime,
    withPausedPersistentWrites: async (_reason: string, operation: (token: unknown) => Promise<unknown>) => operation({ id: 'paused' }),
    beginActivatedLibraryGuard: () => ({ complete() {}, async abortUnchanged() {} }),
    refreshActivatedLibraryUnderPause: async () => ({ projection: 'applied' }), applyPersistentLwwReceive: async () => {}, flushPendingDataLocally: async () => {},
}))
vi.mock('../../mobileBackgroundTask', () => ({ runWithMobileBackgroundTask: (_kind: string, operation: (task: object) => Promise<unknown>) => operation({ progress() {}, async dispose() {} }) }))
vi.mock('../committedWorkingSetContinuation', () => ({ registerCommittedWorkingSetContinuation: vi.fn() }))
vi.mock('src/ts/plugins/apiV3/v3.svelte', () => ({ fencePluginExecutionForAuthorityReplacement: async () => {}, invalidatePluginCachesAfterAuthorityReplacement: async () => {}, restartPluginsAfterAuthorityReplacement: async () => {} }))
vi.mock('../persistentRevisionEvents', () => ({ subscribeLocalPersistentRevision: () => () => {} }))
vi.mock('../generatingConversationRegistry', () => ({ generatingConversations: { snapshot: () => [] } }))
vi.mock('src/ts/alert', () => ({ alertConfirm: async () => false }))
vi.mock('@lucide/svelte', () => ({ CheckIcon: () => {}, LoaderCircleIcon: () => {}, TriangleAlertIcon: () => {} }))
vi.mock('./serverSyncRegistrationInbox', () => ({ serverRegistrationInbox: { changed: { subscribe: () => () => {} }, releaseConsumed() {}, take: () => undefined } }))
vi.mock('./serverSyncQr', () => ({ canScanServerRegistration: false, createServerQrScanner: () => ({ cancel() {} }) }))
vi.mock('./serverAssetResidency', () => ({ getAssetResidencyStatus: async () => undefined, setAssetResidencyPolicy: vi.fn(), evictLocalAssets: vi.fn(), cancelAssetResidencyOperation: vi.fn() }))
vi.mock('src/lang', () => ({ language: { loading: 'Loading', lwwSync: { concurrentEditNotice: 'Concurrent edits', clockBlocked: 'Clock blocked', writerCollision: 'Writer blocked', bindingIncomplete: 'Binding incomplete', registrationRevoked: 'Registration revoked' }, risuNest: { serverSync: { title: 'Sync', description: 'Sync library', connect: 'Connect and sync', syncNow: 'Sync now', disconnect: 'Disconnect', disconnected: 'Stopped', ready: 'Ready', errorHelp: 'Connection failed', credentialUnavailable: 'Credential unavailable', registrationCode: 'Registration code', readRegistration: 'Read', pendingChanges: 'Changes to upload', count: '{0}', lastSuccess: 'Last sync', management: {}, residency: {} } } } }))

let production: typeof import('./serverSyncProduction')
let ui: typeof import('svelte')
let component: ReturnType<(typeof import('svelte'))['mount']> | undefined
const settle = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); await ui.tick() }
const commands = () => f.invoke.mock.calls.map(([command]) => command)
const button = (text: string) => [...document.querySelectorAll('button')].find(value => value.textContent?.trim() === text)
beforeEach(async () => {
    vi.resetModules(); f.invoke.mockReset(); f.offline = true; f.repairError = undefined; f.activationError = undefined; f.fenceError = undefined
    f.configured = true; f.pending = null; f.inspectError = undefined
    f.sharedData = false; f.marker = false; f.queued = 0; f.outbox = 0; f.pushed = 0
    f.binding = { target: { kind: 'server', connectionId: 'server' }, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true })
    f.invoke.mockImplementation(async (command, args) => {
        if (command === 'pds_lww_binding_state') return structuredClone(f.binding)
        if (command === 'server_sync_status') return { configured: f.configured, writerId: 'writer', bindingAuthority: f.binding.targetAuthority, libraryId: 'library', deviceId: 'device' }
        if (command === 'server_sync_lww_activate' && f.activationError) throw f.activationError
        if (command === 'server_sync_lww_activate' && f.offline) throw { code: 'server-unreachable', status: 503, retryable: true }
        if (command === 'server_sync_lww_activate') {
            // Native activation commits the pending registration and queues an initial publication the switch marked.
            f.configured = true; f.pending = null
            if (f.marker) { f.marker = false; f.queued += 1; f.outbox += 2 }
        }
        if (command === 'server_sync_configure') f.pending = { endpoint: args.config.endpoint, libraryId: args.config.libraryId, epoch: 'epoch', serverEmpty: true }
        if (command === 'server_sync_lww_push') { if (!f.outbox) return null; f.outbox -= 1; f.pushed += 1; return { accepted: 1 } }
        if (command === 'server_sync_lww_pull') return { bindingAuthority: f.binding.targetAuthority, requestId: 'pull', changes: [] }
        if (command === 'server_sync_lww_pending_binding') return structuredClone(f.pending)
        if (command === 'server_sync_lww_pending_count') return f.outbox
        if (command === 'server_sync_lww_inspect') {
            if (f.inspectError) throw f.inspectError
            return { inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: true, previouslyBoundLibrary: false, registrationChanged: false, serverRestored: false }
        }
        if (command === 'server_sync_lww_retry' && f.repairError) throw f.repairError
        if (command === 'server_sync_lww_fence' && f.fenceError) throw f.fenceError
        if (command === 'pds_lww_switch_target') {
            expect(args.request).toMatchObject({ bindingAuthority: f.binding.targetAuthority, expectedSelectionEpoch: f.binding.selectionEpoch })
            f.binding = { ...f.binding, target: args.request.target, targetAuthority: String(Number(f.binding.targetAuthority) + 1), selectionEpoch: 'disconnected', libraryId: args.request.target.kind === 'none' ? null : 'library' }
            f.marker = args.request.initialPublication === true
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
        { code: 'server-timeout', status: 503, retryable: true },
        { code: 'directory-unreachable', status: 503, retryable: true },
        { code: 'server-response-error', status: 502, retryable: true },
        { code: 'sync-retry-budget-exhausted', status: 503, retryable: true },
    ])('keeps the fenced binding paused and opens the library when activation fails with transient $code', async failure => {
        f.activationError = failure
        await expect(production.installServerSyncProduction()).resolves.toBeUndefined()
        expect(commands()).toContain('server_sync_lww_fence')
        expect(commands()).not.toContain('server_sync_notify_start'); expect(commands()).not.toContain('pds_lww_switch_target')
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, running: false, paused: true, error: failure.code })
        f.activationError = undefined; f.offline = false
        await production.retryServerSync()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: false, error: '' })
    })
    it('keeps the binding paused when the startup fence cannot reach the server either', async () => {
        f.fenceError = { code: 'server-unreachable', status: 503, retryable: true }
        await expect(production.installServerSyncProduction()).resolves.toBeUndefined()
        expect(commands()).not.toContain('server_sync_notify_start'); expect(commands()).not.toContain('pds_lww_switch_target')
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, running: false, paused: true, error: 'server-unreachable' })
        f.fenceError = undefined; f.offline = false
        await production.retryServerSync()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: false, error: '' })
    })
    it.each([
        { code: 'equal-stamp-integrity', retryable: false },
        { code: 'binding-authority-stale', retryable: false },
    ])('rejects startup when the fence after an offline activation fails with $code', async fenceError => {
        f.fenceError = fenceError
        const before = structuredClone(f.binding)
        await expect(production.installServerSyncProduction()).rejects.toBeInstanceOf(AggregateError)
        expect(commands()).not.toContain('server_sync_notify_start')
        expect(f.binding).toEqual(before)
    })
    it.each([
        [{ code: 'unauthorized', status: 401, retryable: false }, 'Registration revoked'],
        [{ code: 'invalid-device-token', status: 401, retryable: false }, 'Registration revoked'],
        [{ code: 'server-epoch-changed', status: 409, retryable: false }, 'Registration revoked'],
        [{ code: 'device-credential-unavailable', status: 409, retryable: false }, 'Credential unavailable'],
    ])('opens the library and asks for a registration in settings when startup activation fails with %j', async (failure, text) => {
        f.activationError = failure
        await expect(production.installServerSyncProduction()).resolves.toBeUndefined()
        expect(commands()).toContain('server_sync_lww_fence')
        expect(commands()).not.toContain('server_sync_notify_start'); expect(commands()).not.toContain('pds_lww_switch_target')
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, running: false, paused: true, error: failure.code })
        await settings()
        expect(document.body.textContent).toContain(text)
        expect(document.querySelector('#server-registration')).not.toBeNull()
        expect(button('Disconnect')).toBeDefined()
    })
    it.each([
        { code: 'binding-authority-stale', retryable: false },
        { code: 'server-unreachable', retryable: false },
        { code: 'equal-stamp-integrity', retryable: false },
        { code: 'local-metadata', status: 503, retryable: true },
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

describe('first binding stopped after its target switch', () => {
    const pending = (serverEmpty: boolean) => ({ endpoint: 'https://synthetic.invalid', libraryId: 'library', epoch: 'epoch', serverEmpty })
    beforeEach(() => { f.configured = false; f.offline = false })
    it('finishes on its own through the binding flow while the server is still empty', async () => {
        f.pending = pending(true)
        await production.installServerSyncProduction()
        expect(commands()).toContain('server_sync_lww_inspect'); expect(commands()).toContain('server_sync_lww_activate')
        expect(commands()).not.toContain('server_sync_configure'); expect(commands()).not.toContain('pds_lww_switch_target')
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { configured: true, bound: true }, bindingIncomplete: false })
        await settings()
        expect(document.body.textContent).not.toContain('Binding incomplete')
    })
    it('waits for Connect when the server may no longer be the empty one it inspected, and Connect finishes it', async () => {
        f.pending = pending(false)
        await production.installServerSyncProduction()
        expect(commands()).toContain('server_sync_lww_pending_binding')
        expect(commands()).not.toContain('server_sync_lww_inspect'); expect(commands()).not.toContain('server_sync_lww_activate')
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { configured: false }, bindingIncomplete: true })
        await settings()
        expect(document.body.textContent).toContain('Binding incomplete')
        expect(button('Sync now')).toBeUndefined()
        button('Connect and sync')!.click()
        await vi.waitFor(() => expect(document.body.textContent).not.toContain('Binding incomplete'))
        expect(commands()).toContain('server_sync_lww_inspect'); expect(commands()).toContain('server_sync_lww_activate')
        expect(commands()).not.toContain('server_sync_configure'); expect(commands()).not.toContain('pds_lww_switch_target')
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { configured: true }, bindingIncomplete: false })
        expect(document.body.textContent).not.toContain('Binding incomplete')
    })
    it('keeps startup going and waits for Connect when finishing on its own fails', async () => {
        f.pending = pending(true); f.inspectError = { code: 'server-unreachable', status: 503, retryable: true }
        await expect(production.installServerSyncProduction()).resolves.toBeUndefined()
        expect(commands()).not.toContain('server_sync_lww_activate')
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { configured: false }, bindingIncomplete: true })
    })
    it('changes nothing when no first binding stopped', async () => {
        await production.installServerSyncProduction()
        expect(commands()).toContain('server_sync_lww_pending_binding')
        expect(commands()).not.toContain('server_sync_lww_inspect')
        expect(production.getServerSyncController().snapshot().bindingIncomplete).toBe(false)
    })
    it('never looks for a stopped first binding once the server is configured', async () => {
        f.configured = true
        await production.installServerSyncProduction()
        expect(commands()).not.toContain('server_sync_lww_pending_binding')
    })
})

describe('initial publication owed by a first binding that stopped after its switch', () => {
    const config = { endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'device', token: 'synthetic' }
    beforeEach(() => {
        f.binding = { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: 'none', libraryId: null, progress: null }
        f.configured = false; f.offline = false
    })
    async function restartVisible() {
        production.disposeNativeSyncBindings(); vi.resetModules(); f.invoke.mockClear()
        Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true })
        production = await import('./serverSyncProduction'); production.initializeNativeSyncBindings()
        await production.installServerSyncProduction(); await settle()
    }
    it.each([true, false])('carries the decision made at the switch through a restart to one publication (shared data %s)', async shared => {
        f.sharedData = shared
        await production.installServerSyncProduction()
        f.activationError = { code: 'server-unreachable', status: 503, retryable: true }
        await expect(production.connectServerSync(config)).rejects.toMatchObject({ code: 'server-unreachable' })
        const switches = f.invoke.mock.calls.filter(([command]) => command === 'pds_lww_switch_target')
        expect(switches).toHaveLength(1)
        expect(switches[0][1].request).toMatchObject({ target: { kind: 'server', connectionId: 'server' }, initialPublication: shared })
        expect(f.marker).toBe(shared); expect(f.queued).toBe(0); expect(f.pushed).toBe(0)
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: true, error: 'server-unreachable' })

        f.activationError = undefined
        await restartVisible()
        expect(commands()).toContain('server_sync_lww_pending_binding')
        expect(commands()).toContain('server_sync_lww_inspect')
        expect(commands()).not.toContain('pds_lww_switch_target')
        await vi.waitFor(() => expect(f.pushed).toBe(shared ? 2 : 0))
        expect(f.marker).toBe(false); expect(f.queued).toBe(shared ? 1 : 0); expect(f.outbox).toBe(0)
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { configured: true, bound: true }, paused: false, error: '', bindingIncomplete: false })
    })
})
