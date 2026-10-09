import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SyncBindingTransport, SyncBindingDependencies, SyncBindingNative } from './bindingFlow'
import { languageKorean } from 'src/lang/ko'
import { serverSyncProgressView, serverSyncRoutineView } from './serverSyncProgress'

const f = vi.hoisted(() => ({
    native: true,
    selectedIndex: 0,
    database: { characters: [{ chaId: 'selected-stable-id' }] },
    invoke: vi.fn(),
    install: vi.fn(),
    switchTarget: vi.fn(),
    register: vi.fn(),
    resumeCurrent: vi.fn(),
    viewport: undefined as undefined | ((source?: unknown) => void),
    revision: undefined as undefined | ((revision: number, cause?: string) => void),
    conversation: null as null | { characterId: string; conversationId: string },
    mobile: vi.fn(),
    flush: vi.fn(),
    paused: vi.fn(),
    guard: vi.fn(),
    refresh: vi.fn(),
    apply: vi.fn(),
    dispose: vi.fn(),
    continuation: vi.fn(),
    generating: [] as Array<{ characterId: string; conversationId: string }>,
    runtime: {
        revision: 31,
        getStorageAuthorityEpoch: vi.fn(() => 'storage-epoch'),
        subscribeActiveConversationViewportSource: (callback: (source: unknown) => void) => { f.viewport = callback; return () => {} },
        captureSelectedConversationTarget: () => f.conversation,
        setActivatedLibraryRecoveryLifecycle: vi.fn(),
        markCommittedWorkingSetRefreshRequired: vi.fn(),
    },
    transport: undefined as SyncBindingTransport | undefined,
    dependencies: undefined as (Pick<SyncBindingDependencies, 'withPausedWrites' | 'beginActivatedLibraryGuard' | 'refreshActivatedLibrary' | 'recovery'>
        & { native: SyncBindingNative; whileAsking<T>(ask: () => Promise<T>): Promise<T> }) | undefined,
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: f.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
vi.mock('src/ts/platform', () => ({ get isTauri() { return f.native } }))
vi.mock('src/ts/stores.svelte', () => ({ selectedCharID: { subscribe: (run: (value: number) => void) => { run(f.selectedIndex); return () => {} } } }))
vi.mock('../database.svelte', () => ({ getDatabase: () => f.database }))
vi.mock('./bindingProduction', async importOriginal => ({
    ...await importOriginal<typeof import('./bindingProduction')>(),
    installSyncBindingFlow: f.install,
}))
vi.mock('../committedWorkingSetContinuation', () => ({ registerCommittedWorkingSetContinuation: f.continuation }))
vi.mock('./bindingDialog', () => ({ confirmSyncBindingReplacement: vi.fn(), confirmPreviousStorageFiles: vi.fn(async () => 'connect'), downloadPreviousStorageFiles: vi.fn() }))
vi.mock('./bindingLocalData', () => ({ hasLocalBindingData: vi.fn(), hasLocalSharedBindingData: vi.fn() }))
vi.mock('./bindingRegistry', () => ({ registerSyncBindingTransport: f.register, resumeCurrentSyncBinding: f.resumeCurrent, getSyncBindingTransport: vi.fn(), bindSyncTarget: vi.fn(), unbindSyncTarget: vi.fn() }))
vi.mock('./bindingNative', () => ({ createNativeSyncBindingBridge: () => ({ state: vi.fn(), assertAuthority: vi.fn(), switchTarget: f.switchTarget }), replaceNativeSyncBinding: vi.fn(), replaceNativeSyncBindingAsNewDevice: vi.fn() }))
vi.mock('../persistentDataRuntime.svelte', () => ({
    withPausedPersistentWrites: f.paused,
    beginActivatedLibraryGuard: f.guard,
    refreshActivatedLibraryUnderPause: f.refresh,
    applyPersistentLwwReceive: f.apply,
    flushPendingDataLocally: f.flush,
    getPersistentDataRuntime: () => f.runtime,
}))
vi.mock('../../mobileBackgroundTask', () => ({ runWithMobileBackgroundTask: f.mobile, beginMobileBackgroundTask: vi.fn() }))
vi.mock('src/ts/plugins/apiV3/v3.svelte', () => ({ fencePluginExecutionForAuthorityReplacement: vi.fn(), invalidatePluginCachesAfterAuthorityReplacement: vi.fn(), restartPluginsAfterAuthorityReplacement: vi.fn() }))
vi.mock('../persistentRevisionEvents', () => ({ subscribeLocalPersistentRevision: (callback: (revision: number, cause?: string) => void) => { f.revision = callback; return () => {} } }))
vi.mock('../generatingConversationRegistry', () => ({ generatingConversations: { snapshot: () => f.generating } }))

let production: typeof import('./serverSyncProduction')
beforeEach(async () => {
    vi.resetModules(); vi.clearAllMocks(); vi.useFakeTimers()
    f.native = true; f.transport = undefined; f.dependencies = undefined; f.generating = []; f.selectedIndex = 0; f.viewport = undefined; f.revision = undefined; f.conversation = null
    f.mobile.mockImplementation(async (_kind: string, operation: (task: object) => Promise<unknown>) => operation({ progress() {}, async dispose() {} }))
    f.flush.mockResolvedValue(undefined)
    const document = new EventTarget() as EventTarget & { visibilityState: string }
    document.visibilityState = 'hidden'
    vi.stubGlobal('document', document)
    f.invoke.mockImplementation(async command => command === 'server_sync_status'
        ? { configured: false, writerId: 'writer', bindingAuthority: '0' }
        : command === 'pds_lww_binding_state'
            ? { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: '0', libraryId: null, progress: null }
            : command === 'server_sync_lww_pull' ? emptyReceive() : null)
    f.apply.mockReset(); f.apply.mockResolvedValue({ revision: 31, affectedKeys: [], heldKeys: [], deferredKeys: [] })
    f.install.mockImplementation(dependencies => { f.dependencies = dependencies; return { dispose: f.dispose } })
    f.register.mockImplementation((_target, transport) => { f.transport = transport; return () => {} })
    f.refresh.mockResolvedValue({ projection: 'applied' })
    production = await import('./serverSyncProduction')
})
afterEach(() => { production.disposeNativeSyncBindings(); vi.useRealTimers(); vi.unstubAllGlobals() })
const emptyReceive = () => ({ bindingAuthority: '0', requestId: 'empty', changes: [], progress: { kind: 'server', cursor: '0' }, admittedTimeUpperMs: '0' })
const bindingContext = () => ({ state: { target: { kind: 'server' as const, connectionId: 'server' }, targetAuthority: '0', selectionEpoch: '0', libraryId: 'library', progress: null }, signal: new AbortController().signal })
const visible = (value: boolean) => { Object.defineProperty(document, 'visibilityState', { value: value ? 'visible' : 'hidden', configurable: true }); document.dispatchEvent(new Event('visibilitychange')) }
const settle = async () => { for (let i = 0; i < 40; i++) await Promise.resolve() }
const hydrationCalls = () => f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_hydrate')
const pullCalls = () => f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_pull').length
const openConversation = (conversationId: string) => { f.conversation = { characterId: 'selected-stable-id', conversationId }; f.viewport!({}) }
const callOrder = (command: string) => f.invoke.mock.invocationCallOrder[f.invoke.mock.calls.findIndex(([name]) => name === command)]

describe('production server LWW composition', () => {
    it('shows why sync stopped when a committed switch cannot resume', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        f.transport!.reportStopped!({ code: 'server-unreachable' })
        expect(production.getServerSyncController().snapshot().error).toBe('server-unreachable')
        f.transport!.reportStopped!(new AggregateError([{ code: 'unauthorized' }], 'stopped'))
        expect(production.getServerSyncController().snapshot().error).toBe('unauthorized')
        f.transport!.reportStopped!(new Error('no code'))
        expect(production.getServerSyncController().snapshot().error).toBe('server-unreachable')
    })
    it('exposes native configuration without starting a second binding', async () => {
        const config = { endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }
        await production.configureServerSyncConnection(config)
        expect(f.invoke).toHaveBeenCalledExactlyOnceWith('server_sync_configure', { config })
        const { bindSyncTarget } = await import('./bindingRegistry')
        expect(bindSyncTarget).not.toHaveBeenCalled()
    })
    it('retains settings configuration then its single shared binding action', async () => {
        const config = { endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }
        await production.connectServerSync(config, true)
        expect(f.invoke.mock.calls[0]).toEqual(['server_sync_configure', { config }])
        const { bindSyncTarget } = await import('./bindingRegistry')
        expect(bindSyncTarget).toHaveBeenCalledExactlyOnceWith({ kind: 'server', connectionId: 'server' }, { mode: 'new-device' })
    })

    it('resumes the persisted server binding through shared flow ownership at startup', async () => {
        const state = bindingContext().state
        f.invoke.mockImplementation(async command => command === 'server_sync_status' ? { configured: true, writerId: 'writer', bindingAuthority: '0' } : command === 'pds_lww_binding_state' ? state : null)
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        expect(f.resumeCurrent).toHaveBeenCalledExactlyOnceWith(state.target)
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_lww_activate')).toBe(false)
    })
    it.each([true, false])('registers the server transport but leaves a bound server stopped when the start left sync off (configured=%s)', async configured => {
        const state = bindingContext().state
        f.invoke.mockImplementation(async command => command === 'server_sync_status' ? { configured, writerId: 'writer', bindingAuthority: '0' } : command === 'pds_lww_binding_state' ? state : command === 'server_sync_lww_pending_binding' ? { endpoint: 'synthetic', libraryId: 'library', epoch: '1', serverEmpty: true } : null)
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction({ resumeBound: false }); await settle()
        expect(f.register).toHaveBeenCalledOnce()
        expect(f.resumeCurrent).not.toHaveBeenCalled()
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_lww_pending_binding')).toBe(false)
        expect(production.getServerSyncController().snapshot().status.bound).toBe(true)
        expect(production.getServerSyncController().snapshot().paused).toBe(true)
    })
    it('reads the server status again once another sync target takes over', async () => {
        const state = bindingContext().state
        let target: { kind: string; connectionId?: string } = state.target
        f.invoke.mockImplementation(async command => command === 'server_sync_status' ? { configured: true, writerId: 'writer', bindingAuthority: '0' } : command === 'pds_lww_binding_state' ? { ...state, target } : null)
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const bound: boolean[] = []
        production.getServerSyncController().subscribe(view => { bound.push(view.status.bound) })
        expect(bound.at(-1)).toBe(true)
        target = { kind: 'external', connectionId: 'synthetic-connection' }
        const { notifySyncBindingChanged } = await import('./bindingChanges')
        notifySyncBindingChanged(); await settle()
        expect(bound.at(-1)).toBe(false)
    })
    it('reads the stored asset policy with every status request', async () => {
        let assetPolicy = 'remote'
        f.invoke.mockImplementation(async command => command === 'server_sync_status' ? { configured: false, writerId: 'writer', bindingAuthority: '0', assetPolicy }
            : command === 'pds_lww_binding_state' ? { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: '0', libraryId: null, progress: null } : null)
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const controller = production.getServerSyncController()
        expect(controller.snapshot().status.assetPolicy).toBe('remote')
        assetPolicy = 'full'
        await controller.ensureStatus()
        expect(controller.snapshot().status.assetPolicy).toBe('full')
    })
    it('resumes interrupted Full hydration on foreground after the cancelled lane settles', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let reject!: (error: unknown) => void
        f.invoke.mockImplementation(async command => command === 'server_sync_lww_hydrate' ? new Promise<void>((_resolve, fail) => { reject = fail }) : command === 'server_sync_lww_pull' ? emptyReceive() : null)
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(hydrationCalls()).toHaveLength(1)
        visible(false); await settle(); expect(f.invoke).toHaveBeenCalledWith('server_sync_cancel')
        visible(true); await settle(); expect(hydrationCalls()).toHaveLength(1)
        reject({ code: 'cancelled' }); await settle()
        expect(hydrationCalls()).toHaveLength(2)
        expect(production.getServerSyncController().snapshot().error).toBe('')
    })
    it('retains a transient hydration failure and retries on conversation open without a retry timer', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let failures = 1
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_hydrate' && failures--) throw { code: 'server-unreachable' }
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(production.getServerSyncController().snapshot().error).toBe('server-unreachable')
        await vi.advanceTimersByTimeAsync(120_000); expect(hydrationCalls()).toHaveLength(1)
        openConversation('opened'); await settle()
        expect(hydrationCalls()).toHaveLength(2); expect(production.getServerSyncController().snapshot().error).toBe('')
    })
    it('coalesces newly received asset work without cancelling an in-flight hydration', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let finish!: () => void
        let assetPage = false
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_hydrate') return new Promise<void>(resolve => { finish = resolve })
            if (command === 'server_sync_lww_pull') {
                if (assetPage) { assetPage = false; return { ...emptyReceive(), requestId: 'asset-page', changes: [{ key: JSON.stringify(['asset', 'assets/new.png']), stamp: { physicalMs: '0', logical: '0', writerId: 'writer' }, value: { kind: 'inline', bytes: 'e30=' } }] } }
                return emptyReceive()
            }
            return null
        })
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle(); f.invoke.mockClear()
        assetPage = true; await production.receiveAvailableServerChanges()
        assetPage = true; await production.receiveAvailableServerChanges()
        expect(hydrationCalls()).toHaveLength(0)
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_cancel')).toBe(false)
        finish(); await settle(); expect(hydrationCalls()).toHaveLength(1)
    })
    it('does not rescan Full residency for empty or message-control-only routine receives', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle(); f.invoke.mockClear()
        await production.receiveAvailableServerChanges(); await settle(); expect(hydrationCalls()).toHaveLength(0)
        let messagePage = true
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_pull' && messagePage) { messagePage = false; return { ...emptyReceive(), requestId: 'messages', changes: [{ key: JSON.stringify(['messages', 'character', 'conversation']), stamp: { physicalMs: '0', logical: '0', writerId: 'writer' }, value: { kind: 'object', descriptorHash: 'control', descriptor: { dependencies: ['page-control'] } } }] } }
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        await production.receiveAvailableServerChanges(); await settle(); expect(hydrationCalls()).toHaveLength(0)
    })
    const representativeChanges = [
        { key: ['character', 'first', 'image'], value: { kind: 'inline', bytes: btoa(JSON.stringify('assets/main.png')) } },
        { key: ['character', 'first', 'trashTime'], value: { kind: 'deleted' } },
        { key: ['exists', 'character', 'first'], value: { kind: 'inline', bytes: 'dHJ1ZQ==' } },
        { key: ['archive', 'first'], value: { kind: 'deleted' } },
    ]
    const receiveUnder = async (assetPolicy: string, key: string[], value: unknown) => {
        const base = f.invoke.getMockImplementation()!
        f.invoke.mockImplementation(async (command, args) => command === 'server_sync_status'
            ? { configured: true, writerId: 'writer', bindingAuthority: '0', assetPolicy } : base(command, args))
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle(); f.invoke.mockClear()
        let page = true
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_pull' && page) {
                page = false
                return { ...emptyReceive(), changes: [{ key: JSON.stringify(key), value }] }
            }
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        await production.receiveAvailableServerChanges(); await settle()
    }
    it.each(representativeChanges)('refreshes representatives after receiving $key without downloading the full inventory', async ({ key, value }) => {
        await receiveUnder('remote', key, value)
        expect(hydrationCalls()).toEqual([['server_sync_lww_hydrate', {
            request: expect.objectContaining({ bindingAuthority: '0' }), selectedCharacterId: 'selected-stable-id', representativesOnly: true,
        }]])
    })
    it.each(representativeChanges)('asks nothing of a device that keeps every asset after receiving $key', async ({ key, value }) => {
        await receiveUnder('full', key, value)
        expect(hydrationCalls()).toHaveLength(0)
    })
    it('coalesces local remote-policy edits and preserves another edit during representative hydration', async () => {
        const base = f.invoke.getMockImplementation()!
        f.invoke.mockImplementation(async (command, args) => command === 'server_sync_status'
            ? { configured: true, writerId: 'writer', bindingAuthority: '0', assetPolicy: 'remote' } : base(command, args))
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle(); f.invoke.mockClear()
        let finish!: () => void
        const original = f.invoke.getMockImplementation()!
        f.invoke.mockImplementation((command, args) => command === 'server_sync_lww_hydrate'
            ? new Promise<void>(resolve => { finish = resolve }) : original(command, args))
        f.revision!(32); f.revision!(33)
        await vi.advanceTimersByTimeAsync(2000); await settle()
        expect(hydrationCalls()).toHaveLength(1)
        expect(hydrationCalls()[0][1]).toMatchObject({ representativesOnly: true })
        f.revision!(34)
        await vi.advanceTimersByTimeAsync(2000); await settle()
        expect(hydrationCalls()).toHaveLength(1)
        finish(); await settle()
        expect(hydrationCalls()).toHaveLength(2)
        finish(); await settle()
    })
    it('checks character icons once a burst of local saves settles', async () => {
        const base = f.invoke.getMockImplementation()!
        f.invoke.mockImplementation(async (command, args) => command === 'server_sync_status'
            ? { configured: true, writerId: 'writer', bindingAuthority: '0', assetPolicy: 'remote' } : base(command, args))
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle(); f.invoke.mockClear()
        for (const revision of [32, 33, 34]) { f.revision!(revision); await vi.advanceTimersByTimeAsync(1500); await settle() }
        expect(hydrationCalls()).toHaveLength(0)
        await vi.advanceTimersByTimeAsync(500); await settle()
        expect(hydrationCalls()).toEqual([['server_sync_lww_hydrate', {
            request: expect.objectContaining({ bindingAuthority: '0' }), selectedCharacterId: 'selected-stable-id', representativesOnly: true,
        }]])
    })
    it('does not scan the full asset inventory on local edits in full mode', async () => {
        const base = f.invoke.getMockImplementation()!
        f.invoke.mockImplementation(async (command, args) => command === 'server_sync_status'
            ? { configured: true, writerId: 'writer', bindingAuthority: '0', assetPolicy: 'full' } : base(command, args))
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle(); f.invoke.mockClear()
        f.revision!(32)
        await vi.advanceTimersByTimeAsync(2000); await settle()
        expect(hydrationCalls()).toHaveLength(0)
    })
    it('fills representatives when settings switch an active binding to remote storage', async () => {
        const base = f.invoke.getMockImplementation()!
        let assetPolicy = 'full'
        f.invoke.mockImplementation(async (command, args) => command === 'server_sync_status'
            ? { configured: true, writerId: 'writer', bindingAuthority: '0', assetPolicy } : base(command, args))
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle(); f.invoke.mockClear()
        assetPolicy = 'remote'
        await production.getServerSyncController().ensureStatus(); await settle()
        expect(hydrationCalls()).toHaveLength(1)
        expect(hydrationCalls()[0][1]).toMatchObject({ representativesOnly: true })
    })
    it('rechecks an outgoing clock block through native settlement on open, and leaves accepted correction blocked', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let future = true
        let accepted = false
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_push' && future) { future = false; throw { code: 'clock-skew', retryable: false } }
            if (command === 'server_sync_lww_retry' && accepted) throw { code: 'accepted-clock-correction-required', retryable: false }
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(production.getServerSyncController().snapshot().paused).toBe(true)
        await vi.advanceTimersByTimeAsync(120_000)
        expect(f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_retry')).toHaveLength(0)
        openConversation('first'); await settle()
        expect(f.invoke).toHaveBeenCalledWith('server_sync_lww_retry', { request: { bindingAuthority: '0', requestId: expect.any(String) } })
        expect(production.getServerSyncController().snapshot().paused).toBe(false)
        future = true; accepted = true
        visible(false); await settle(); visible(true); await settle()
        openConversation('second'); await settle()
        expect(production.getServerSyncController().snapshot().paused).toBe(true)
        expect(production.getServerSyncController().snapshot().error).toBe('accepted-clock-correction-required')
        const retries = f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_retry').length
        visible(false); await settle(); visible(true); openConversation('third'); await settle()
        expect(f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_retry')).toHaveLength(retries)
    })
    it('drains and clears the old hydration context before a binding fence permits foreground return', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let stopped!: (error: unknown) => void
        f.invoke.mockImplementation(async command => command === 'server_sync_lww_hydrate' ? new Promise<void>((_resolve, reject) => { stopped = reject }) : command === 'server_sync_lww_pull' ? emptyReceive() : null)
        visible(true); const context = bindingContext(); await f.transport!.resumeBinding(context); await settle()
        let fenced = false
        const fence = f.transport!.fenceOldJobs(context).then(() => { fenced = true })
        await settle(); expect(fenced).toBe(false)
        f.invoke.mockClear(); visible(false); await settle(); visible(true); await settle()
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_notify_start' || command === 'server_sync_lww_push')).toBe(false)
        stopped({ code: 'cancelled' }); await fence
        f.invoke.mockClear(); visible(false); await settle(); visible(true); await settle()
        expect(hydrationCalls()).toHaveLength(0)
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_notify_start' || command === 'server_sync_lww_push')).toBe(false)
        expect(production.getServerSyncController().snapshot().status.bound).toBe(false)
    })
    it('passes the actual stable selected character ID to foreground hydration and null when no character is selected', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const state = { target: { kind: 'server' as const, connectionId: 'server' }, targetAuthority: '0', selectionEpoch: '0', libraryId: 'library', progress: null }
        const context = { state, signal: new AbortController().signal }
        Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true })
        await f.transport!.resumeBinding(context)
        expect(f.invoke).toHaveBeenCalledWith('server_sync_lww_hydrate', { request: expect.objectContaining({ bindingAuthority: '0' }), selectedCharacterId: 'selected-stable-id' })
        f.selectedIndex = -1
        await f.transport!.resumeBinding(context)
        expect(f.invoke).toHaveBeenCalledWith('server_sync_lww_hydrate', { request: expect.objectContaining({ bindingAuthority: '0' }), selectedCharacterId: null })
    })
    it('installs one shared binding flow without any server configuration and carries the exact pause token', async () => {
        const token = Object.freeze({ id: 'exact-token' })
        f.paused.mockImplementation(async (_reason, operation) => operation(token))
        production.initializeNativeSyncBindings(); production.initializeNativeSyncBindings()
        expect(f.install).toHaveBeenCalledTimes(1); expect(f.invoke).not.toHaveBeenCalled()
        await f.dependencies!.withPausedWrites(async () => {
            f.dependencies!.beginActivatedLibraryGuard()
            await f.dependencies!.refreshActivatedLibrary()
        })
        expect(f.guard).toHaveBeenCalledWith(token); expect(f.refresh).toHaveBeenCalledWith(token)
        expect(() => f.dependencies!.beginActivatedLibraryGuard()).toThrow('pause is unavailable')
        f.refresh.mockResolvedValueOnce({ projection: 'superseded' })
        await expect(f.dependencies!.withPausedWrites(() => f.dependencies!.refreshActivatedLibrary())).rejects.toThrow('projection is unavailable')
    })
    it('registers the server adapter with no registration, and waits for native receive application before ACK', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        expect(f.register).toHaveBeenCalledTimes(1)
        const state = { target: { kind: 'server' as const, connectionId: 'server' }, targetAuthority: '0', selectionEpoch: '0', libraryId: 'library', progress: null }
        const context = { state, signal: new AbortController().signal }
        await f.transport!.resumeBinding(context)
        let complete!: () => void
        f.apply.mockImplementationOnce(() => new Promise<void>(resolve => { complete = resolve }))
        f.invoke.mockImplementation(async command => command === 'server_sync_lww_pull'
            ? { bindingAuthority: '0', requestId: 'received-page', changes: [], progress: { kind: 'server', cursor: '0' }, admittedTimeUpperMs: '0' }
            : null)
        const received = production.receiveAvailableServerChanges()
        await vi.advanceTimersByTimeAsync(0)
        expect(f.apply).toHaveBeenCalledTimes(1)
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_lww_ack')).toBe(false)
        complete(); await received
        expect(f.invoke).toHaveBeenCalledWith('server_sync_lww_ack', { request: { bindingAuthority: '0', requestId: 'received-page' } })
    })
    it('composes actual recovery registration with the exact pause token and a critical authority-bound continuation', async () => {
        const token = Object.freeze({ id: 'recovery-token' })
        const lifecycle = { beforeRefresh: vi.fn(async () => {}), afterRefresh: vi.fn(async () => {}) }
        const error = new Error('plugin restart failed')
        const resume = vi.fn(async () => {})
        f.paused.mockImplementation(async (_reason, operation) => operation(token))
        production.initializeNativeSyncBindings()
        await f.dependencies!.withPausedWrites(async () => {
            f.dependencies!.recovery!.setLifecycle(lifecycle)
        })
        expect(f.runtime.setActivatedLibraryRecoveryLifecycle).toHaveBeenCalledWith(token, lifecycle)
        expect(() => f.dependencies!.recovery!.setLifecycle(lifecycle)).toThrow('pause is unavailable')
        f.dependencies!.recovery!.registerFailure(error, resume)
        expect(f.runtime.markCommittedWorkingSetRefreshRequired).toHaveBeenCalledWith(31, error)
        expect(f.continuation).toHaveBeenCalledWith(31, f.runtime, 'storage-epoch', resume, undefined, true)
    })
    it('drains all 640 eligible initial units before starting foreground services', async () => {
        Object.defineProperty(document, 'visibilityState', { value: 'visible' })
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const context = { state: { target: { kind: 'server' as const, connectionId: 'server' }, targetAuthority: '0', selectionEpoch: '0', libraryId: 'library', progress: null }, signal: new AbortController().signal }
        const units = Array.from({ length: 640 }, (_, index) => `synthetic-unit-${index}`)
        const pending: string[] = []
        const batches: number[] = []
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_activate') { pending.push(...units); return null }
            if (command === 'server_sync_lww_push') {
                const batch = pending.splice(0, 256)
                if (!batch.length) return null
                batches.push(batch.length)
                return { operationId: `native-batch-${batches.length}` }
            }
            return null
        })
        f.invoke.mockClear()
        await f.transport!.publishInitialSharedState(context)
        expect(pending).toEqual([]); expect(batches).toEqual([256, 256, 128])
        const commands = f.invoke.mock.calls.map(([command]) => command)
        expect(commands[0]).toBe('server_sync_lww_activate')
        expect(commands.filter(command => command === 'server_sync_lww_push')).toHaveLength(4)
        expect(commands).not.toContain('pds_lww_queue_unit_state_page')
        expect(commands.some(command => command === 'server_sync_lww_pull' || command === 'server_sync_notify_start')).toBe(false)
    })
    it('stops initial publication at a generating-only null batch and leaves service start to G', async () => {
        Object.defineProperty(document, 'visibilityState', { value: 'visible' })
        f.generating = [{ characterId: 'synthetic-character', conversationId: 'synthetic-chat' }]
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const context = { state: { target: { kind: 'server' as const, connectionId: 'server' }, targetAuthority: '0', selectionEpoch: '0', libraryId: 'library', progress: null }, signal: new AbortController().signal }
        f.invoke.mockImplementation(async () => null)
        await f.transport!.publishInitialSharedState(context)
        const pushes = f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_push')
        expect(pushes).toHaveLength(1); expect(pushes[0][1]).toMatchObject({ generating: f.generating })
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_lww_pull' || command === 'server_sync_notify_start')).toBe(false)
    })
    it('clears an old block and its error when a different binding authority is installed, and keeps it for the same authority', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let collide = true
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_push' && collide) { collide = false; throw { code: 'writer-collision', retryable: false } }
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        const pushes = () => f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_push').length
        visible(true); const first = bindingContext(); await f.transport!.resumeBinding(first); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: true, error: 'writer-collision' })
        await f.transport!.fenceOldJobs(first); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: true, error: 'writer-collision' })
        expect(pushes()).toBe(1)
        const other = bindingContext(); other.state = { ...other.state, targetAuthority: '1', selectionEpoch: '1' }
        await f.transport!.fenceOldJobs(bindingContext()); await f.transport!.resumeBinding(other); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: false, error: '' })
        expect(pushes()).toBe(2)
    })
    it('clears a transient sync error once publication and receive succeed again', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let offline = true
        f.invoke.mockImplementation(async command => {
            if ((command === 'server_sync_lww_push' || command === 'server_sync_lww_pull') && offline) throw { code: 'server-unreachable', retryable: true }
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(production.getServerSyncController().snapshot().error).toBe('server-unreachable')
        offline = false
        await vi.advanceTimersByTimeAsync(5000); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: false, error: '' })
    })
    it('never reports a cancelled publication as a sync error', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_push') throw { code: 'cancelled', retryable: true }
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: false, error: '' })
    })
    it('keeps automatic sync stopped through visibility changes until the hold is released', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        const release = await production.holdServerSync(); await settle()
        f.invoke.mockClear()
        const syncCommands = () => f.invoke.mock.calls.map(([command]) => command as string).filter(command => !['server_sync_status', 'pds_lww_binding_state'].includes(command))
        visible(false); await settle(); visible(true); await settle(); visible(false); await settle(); visible(true); await settle()
        await vi.advanceTimersByTimeAsync(120_000); await settle()
        expect(syncCommands()).toEqual([])
        await release(); await settle()
        expect(syncCommands()).toEqual(expect.arrayContaining(['server_sync_notify_start', 'server_sync_lww_pull', 'server_sync_lww_push']))
    })
    it('lifts a hold whose fence fails, so automatic sync resumes', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        const fallback = f.invoke.getMockImplementation()!
        f.invoke.mockImplementationOnce(async command => { if (command === 'server_sync_notify_stop') throw { code: 'server-sync-state-unavailable', retryable: true }; return fallback(command) })
        f.invoke.mockClear()
        await expect(production.holdServerSync()).rejects.toMatchObject({ code: 'server-sync-state-unavailable' })
        await settle()
        const controller = production.getServerSyncController()
        expect(controller.snapshot().replacing).toBe(false)
        expect(() => controller.assertFileOperationAvailable()).not.toThrow()
        visible(false); await settle(); f.invoke.mockClear()
        visible(true); await settle()
        const syncCommands = f.invoke.mock.calls.map(([command]) => command as string)
        expect(syncCommands).toEqual(expect.arrayContaining(['server_sync_notify_start', 'server_sync_lww_pull', 'server_sync_lww_push']))
    })
    it('settles the old registration tolerantly and claims a fresh writer for a new registration without pushing', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        f.invoke.mockClear()
        await f.transport!.fenceOldJobs(bindingContext())
        await f.transport!.fenceOldJobs({ ...bindingContext(), mode: 'fresh-writer' })
        const inspected = { inspectionId: 'synthetic-inspection', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: true, registrationChanged: true, serverRestored: false }
        await f.transport!.prepareFreshWriter!(inspected, { ...bindingContext(), mode: 'fresh-writer' })
        expect(f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_fence').map(([, args]) => args)).toEqual([{ newDevice: false }, { newDevice: true }])
        expect(f.invoke).toHaveBeenCalledWith('server_sync_lww_prepare_fresh_writer', { inspectionId: 'synthetic-inspection', request: { bindingAuthority: '0', requestId: expect.any(String) } })
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_lww_push' || command === 'server_sync_lww_activate')).toBe(false)
    })
    it('pulls when another conversation opens, not when a local commit renews the open one', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        openConversation('first'); await settle()
        const opened = pullCalls()
        f.viewport!({}); f.viewport!({}); f.viewport!({}); f.viewport!(null); await settle()
        expect(pullCalls()).toBe(opened)
        openConversation('second'); await settle()
        expect(pullCalls()).toBe(opened + 1)
    })
    it('stops on a local apply failure instead of pulling the same page again as an unreachable server', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        f.apply.mockRejectedValue(Object.assign(new Error('request-id-integrity'), { code: 'validation' }))
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: true, error: 'local-validation' })
        const pulls = pullCalls()
        await vi.advanceTimersByTimeAsync(120_000); await settle()
        openConversation('other'); await settle()
        expect(pullCalls()).toBe(pulls)
    })
    it('retries a local storage failure during apply', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        f.apply.mockRejectedValue(Object.assign(new Error('disk I/O error'), { code: 'store-error' }))
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: false, error: 'local-storage' })
        f.apply.mockResolvedValue({ revision: 31, affectedKeys: [], heldKeys: [], deferredKeys: [] })
        await vi.advanceTimersByTimeAsync(5000); await settle()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ paused: false, error: '' })
    })
    it('finishes initial publication while the document is hidden', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const pending = Array.from({ length: 300 }, (_, index) => `synthetic-unit-${index}`)
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_push') { const batch = pending.splice(0, 256); return batch.length ? { operationId: 'batch' } : null }
            return null
        })
        await f.transport!.publishInitialSharedState(bindingContext())
        expect(pending).toEqual([])
        expect(f.invoke.mock.calls.some(([command]) => command === 'server_sync_lww_pull' || command === 'server_sync_notify_start')).toBe(false)
    })
    it('binds a server connection under a sync background task', async () => {
        const { bindSyncTarget } = await import('./bindingRegistry')
        let inTask = false
        f.mobile.mockImplementation(async (_kind: string, operation: (task: object) => Promise<unknown>) => { inTask = true; try { return await operation({ progress() {}, async dispose() {} }) } finally { inTask = false } })
        vi.mocked(bindSyncTarget).mockImplementation(async () => { expect(inTask).toBe(true); return { kind: 'cancelled' } })
        await production.connectServerSync({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' })
        await production.completeServerSyncBinding()
        expect(bindSyncTarget).toHaveBeenCalledTimes(2)
        expect(f.mobile.mock.calls.map(([kind]) => kind)).toEqual(['sync', 'sync'])
    })
    it('publishes pending local work under a sync background task before it disconnects on hide', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        let unsent = 2
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_push') return unsent-- > 0 ? { operationId: 'hidden' } : null
            return command === 'server_sync_lww_pull' ? emptyReceive() : null
        })
        f.invoke.mockClear(); f.mobile.mockClear()
        f.revision!(32, 'edit')
        visible(false); await settle()
        expect(f.mobile).toHaveBeenCalledWith('sync', expect.any(Function))
        expect(f.flush).toHaveBeenCalledOnce()
        expect(unsent).toBeLessThan(0)
        expect(callOrder('server_sync_lww_push')).toBeLessThan(callOrder('server_sync_notify_stop'))
        expect(f.flush.mock.invocationCallOrder[0]).toBeLessThan(callOrder('server_sync_lww_push'))
        await vi.advanceTimersByTimeAsync(5000)
        expect(f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_push')).toHaveLength(3)
    })
    it('keeps a failed binding visible as a paused connection that 지금 동기화 resumes', async () => {
        const { bindSyncTarget } = await import('./bindingRegistry')
        const state = bindingContext().state
        let bound = false
        f.invoke.mockImplementation(async command => command === 'server_sync_status' ? { configured: true, writerId: 'writer', bindingAuthority: '0' }
            : command === 'pds_lww_binding_state' ? (bound ? state : { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: '0', libraryId: null, progress: null })
                : command === 'server_sync_lww_pull' ? emptyReceive() : null)
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        vi.mocked(bindSyncTarget).mockImplementation(async () => { bound = true; throw { code: 'server-unreachable', retryable: true } })
        f.resumeCurrent.mockImplementation(async () => { await f.transport!.resumeBinding(bindingContext()) })
        await expect(production.connectServerSync({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' })).rejects.toMatchObject({ code: 'server-unreachable' })
        production.dismissServerSyncConnectionFailure()
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: true, error: 'server-unreachable' })
        await production.retryServerSync()
        expect(f.resumeCurrent).toHaveBeenCalledExactlyOnceWith(state.target)
    })
    it('drops a refused connection that was left without connecting', async () => {
        const { bindSyncTarget } = await import('./bindingRegistry')
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        vi.mocked(bindSyncTarget).mockRejectedValue({ code: 'registration-used', retryable: false })
        await expect(production.connectServerSync({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' })).rejects.toMatchObject({ code: 'registration-used' })
        const views: string[] = []
        production.getServerSyncController().subscribe(view => { views.push(view.error) })
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: false }, error: 'registration-used' })
        production.dismissServerSyncConnectionFailure()
        expect(production.getServerSyncController().snapshot().error).toBe('')
        expect(views).toEqual(['registration-used', ''])
    })
    describe('asset storage chosen with the connection', () => {
        const config = { endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' }
        const residency = (policy: string) => ({ policy, localBytes: 0, remoteBytes: 0, remoteObjects: 0, serverBytes: 0, serverObjects: 0, externalObjects: [], unavailableObjects: 0, evictedBytes: 0 })
        const policyCalls = () => f.invoke.mock.calls.filter(([command]) => command === 'server_sync_asset_policy')
        const contextFor = (libraryId: string) => ({ ...bindingContext(), state: { ...bindingContext().state, libraryId } })
        // The shared flow is mocked; each case drives the transport the way a binding to `libraryId` would.
        async function bindWith(newDevice: boolean, libraryId = 'library') {
            const { bindSyncTarget } = await import('./bindingRegistry')
            vi.mocked(bindSyncTarget).mockImplementation(async () => {
                const context = contextFor(libraryId)
                if (newDevice) await f.transport!.resumeNewDeviceBinding!({ authorizationId: 'authorization', writerId: 'writer' }, { revision: 32, writerId: 'writer', bindingAuthority: '0' }, context)
                else await f.transport!.resumeBinding(context)
                return { kind: 'bound', action: newDevice ? 'new-device' : 'replaced', state: context.state }
            })
        }
        let base: (command: string, args?: unknown) => Promise<unknown>
        beforeEach(async () => {
            base = f.invoke.getMockImplementation()!
            f.invoke.mockImplementation(async (command, args) => command === 'server_sync_asset_policy' ? residency('remote') : base(command, args))
            production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
            visible(true)
        })
        it.each([false, true])('keeps assets on the server from before the first hydration (new device=%s)', async newDevice => {
            await bindWith(newDevice)
            await production.connectServerSync(config, newDevice, 'remote'); await settle()
            expect(policyCalls()).toEqual([['server_sync_asset_policy', { policy: 'remote' }]])
            expect(callOrder(newDevice ? 'server_sync_lww_activate_new_device' : 'server_sync_lww_activate')).toBeLessThan(callOrder('server_sync_asset_policy'))
            expect(callOrder('server_sync_asset_policy')).toBeLessThan(callOrder('server_sync_lww_hydrate'))
        })
        it.each([
            { policy: 'full' as const, newDevice: false },
            { policy: 'full' as const, newDevice: true },
            { policy: undefined, newDevice: false },
        ])('leaves the stored policy to native hydration with $policy (new device=$newDevice)', async ({ policy, newDevice }) => {
            await bindWith(newDevice)
            await production.connectServerSync(config, newDevice, policy); await settle()
            expect(policyCalls()).toHaveLength(0)
            expect(hydrationCalls()).toHaveLength(1)
        })
        it('applies the choice once, so a later resume of the same binding hydrates under the stored policy', async () => {
            await bindWith(false)
            await production.connectServerSync(config, false, 'remote'); await settle()
            await f.transport!.fenceOldJobs(bindingContext()); await f.transport!.resumeBinding(bindingContext()); await settle()
            expect(policyCalls()).toHaveLength(1)
        })
        it('forgets the choice when the binding fails', async () => {
            const { bindSyncTarget } = await import('./bindingRegistry')
            vi.mocked(bindSyncTarget).mockRejectedValue({ code: 'server-unreachable', retryable: true })
            await expect(production.connectServerSync(config, false, 'remote')).rejects.toMatchObject({ code: 'server-unreachable' })
            await f.transport!.resumeBinding(bindingContext()); await settle()
            expect(policyCalls()).toHaveLength(0)
            expect(hydrationCalls()).toHaveLength(1)
        })
        it('forgets the choice when the configuration is refused', async () => {
            f.invoke.mockImplementation(async (command, args) => { if (command === 'server_sync_configure') throw { code: 'unauthorized' }; return base(command, args) })
            await expect(production.connectServerSync(config, false, 'remote')).rejects.toMatchObject({ code: 'unauthorized' })
            await f.transport!.resumeBinding(bindingContext()); await settle()
            expect(policyCalls()).toHaveLength(0)
        })
        it('leaves another library, such as the binding a cancelled connection resumes, on its own policy', async () => {
            await bindWith(false, 'previous-library')
            await production.connectServerSync(config, false, 'remote'); await settle()
            expect(policyCalls()).toHaveLength(0)
            expect(hydrationCalls()).toHaveLength(1)
        })
        it('stops the binding before any hydration when the policy is refused', async () => {
            f.invoke.mockImplementation(async (command, args) => { if (command === 'server_sync_asset_policy') throw { code: 'library-operation-busy' }; return base(command, args) })
            await bindWith(false)
            await expect(production.connectServerSync(config, false, 'remote')).rejects.toMatchObject({ code: 'library-operation-busy' })
            await settle()
            expect(hydrationCalls()).toHaveLength(0)
        })
    })
    it('guards native initialization and transport registration on the web', async () => {
        f.native = false
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        expect(f.install).not.toHaveBeenCalled(); expect(f.register).not.toHaveBeenCalled(); expect(f.invoke).not.toHaveBeenCalled()
    })
})
describe('server sync progress', () => {
    const lanes = (sent: number) => ['send', 'receive', 'hydrate', 'binding', 'assets'].map(lane => ({
        lane, active: lane === 'send', step: lane === 'send' ? 'uploading' : 'idle', listed: 0, listedTotal: 0, itemsDone: 0, itemsTotal: 0, filesDone: 0, filesTotal: 0, bytesDone: 0, bytesTotal: 0, sentBytes: lane === 'send' ? sent : 0, receivedBytes: 0, backlogDone: 0, backlogLeft: 0,
    }))
    const sendLanes = (send: Record<string, unknown>) => ['send', 'receive', 'hydrate', 'binding', 'assets'].map(lane => ({
        lane, active: false, step: 'idle', listed: 0, listedTotal: 0, itemsDone: 0, itemsTotal: 0, filesDone: 0, filesTotal: 0, bytesDone: 0, bytesTotal: 0, sentBytes: 0, receivedBytes: 0, backlogDone: 0, backlogLeft: 0, ...(lane === 'send' ? send : {}),
    }))
    const pendingReads = () => f.invoke.mock.calls.filter(([command]) => command === 'server_sync_lww_pending_count').length
    /** A push that waits; each lanes read takes the next of `reads` and repeats the last. */
    const countedPush = (reads: Record<string, unknown>[], pending: number | null = null) => {
        let finish!: (failure?: unknown) => void
        f.invoke.mockImplementation(async command => command === 'server_sync_lww_push' ? new Promise((resolve, reject) => { finish = failure => failure ? reject(failure) : resolve(null) })
            : command === 'server_sync_lww_pull' ? emptyReceive()
                : command === 'server_sync_progress' ? sendLanes((reads.length > 1 ? reads.shift() : reads[0])!)
                    : command === 'server_sync_lww_pending_count' ? pending : null)
        return (failure?: unknown) => finish(failure)
    }
    const progressReads = () => f.invoke.mock.calls.filter(([command]) => command === 'server_sync_progress').length
    const hangingPush = (sent: number[]) => {
        let finish!: (failure?: unknown) => void
        f.invoke.mockImplementation(async command => command === 'server_sync_lww_push' ? new Promise((resolve, reject) => { finish = failure => failure ? reject(failure) : resolve(null) })
            : command === 'server_sync_lww_pull' ? emptyReceive() : command === 'server_sync_progress' ? lanes(sent.shift() ?? 0) : null)
        return (failure?: unknown) => finish(failure)
    }
    it('records running stages without reading native counts while no view watches', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const finish = hangingPush([])
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        const controller = production.getServerSyncController()
        expect(controller.snapshot().progress?.stages).toEqual(['downloading', 'publishing', 'assets'])
        expect(controller.snapshot().progress?.active).toEqual(['publishing'])
        expect(progressReads()).toBe(0)
        finish(); await settle()
        expect(controller.snapshot().progress).toBeUndefined()
        expect(controller.snapshot().lastSuccessAt).toBe(Date.now())
        expect(progressReads()).toBe(0)
    })
    it('counts native transfers from the first read while a view watches, and stops reading after the attempt', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const finish = hangingPush([1000, 5000])
        const controller = production.getServerSyncController()
        const stop = controller.watchProgress()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        expect(progressReads()).toBe(1)
        expect(controller.snapshot().progress?.lanes?.find(lane => lane.lane === 'send')?.sentBytes).toBe(0)
        await vi.advanceTimersByTimeAsync(500); await settle()
        expect(controller.snapshot().progress?.lanes?.find(lane => lane.lane === 'send')).toMatchObject({ active: true, step: 'uploading', sentBytes: 4000 })
        finish(); await settle()
        const reads = progressReads()
        await vi.advanceTimersByTimeAsync(2000); await settle()
        expect(progressReads()).toBe(reads)
        stop()
    })
    it('treats automatic sync as routine until an asset download joins it', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const finish = hangingPush([])
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        const controller = production.getServerSyncController()
        expect(controller.snapshot().progress?.mode).toBe('routine')
        let downloaded!: () => void
        const download = controller.track('assets', () => new Promise<void>(resolve => { downloaded = resolve }))
        await settle()
        expect(controller.snapshot().progress?.mode).toBe('full')
        finish(); downloaded(); await download; await settle()
        expect(controller.snapshot().progress).toBeUndefined()
        const alone = controller.track('assets', () => new Promise<void>(resolve => { downloaded = resolve }))
        await settle()
        expect(controller.snapshot().progress?.mode).toBe('full')
        downloaded(); await alone
    })
    it('fixes the upload of a watched routine attempt once and shows it finished for a moment', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const finish = countedPush([{}, { active: true, step: 'confirming', itemsDone: 1, itemsTotal: 2 }, { itemsDone: 3, itemsTotal: 3 }], 2)
        const controller = production.getServerSyncController()
        const stop = controller.watchProgress()
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        await vi.advanceTimersByTimeAsync(500); await settle()
        expect(controller.snapshot().progress).toMatchObject({ mode: 'routine', plannedSend: 3, peak: { changes: 1 / 3 } })
        await vi.advanceTimersByTimeAsync(1000); await settle()
        expect(pendingReads()).toBe(1)
        finish(); await settle()
        expect(controller.snapshot().progress).toBeUndefined()
        expect(controller.snapshot().finished).toMatchObject({ mode: 'routine', endedAt: Date.now() })
        expect(controller.snapshot().finished?.lanes?.find(lane => lane.lane === 'send')).toMatchObject({ itemsDone: 3, itemsTotal: 3 })
        expect(controller.snapshot().lastSuccessAt).toBe(Date.now())
        await vi.advanceTimersByTimeAsync(1500); await settle()
        expect(controller.snapshot().finished).toBeUndefined()
        stop()
    })
    it('fixes the upload and the receive of a connection once, as automatic sync does', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const reads: Array<Record<string, Record<string, unknown>>> = [{}, { send: { active: true, step: 'preparing' }, receive: { backlogDone: 40, backlogLeft: 60 } }, { send: { active: true, step: 'confirming', itemsDone: 256, itemsTotal: 256 }, receive: { backlogDone: 90, backlogLeft: 20 } }, { send: { active: true, step: 'confirming', itemsDone: 512, itemsTotal: 512 }, receive: { backlogDone: 150, backlogLeft: 10 } }]
        let finish!: () => void
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_push') return new Promise(resolve => { finish = () => resolve(null) })
            if (command === 'server_sync_progress') { const read = reads.length > 1 ? reads.shift()! : reads[0]; return sendLanes({}).map(lane => ({ ...lane, ...read[lane.lane] })) }
            if (command === 'server_sync_status') return { configured: true, writerId: 'writer', bindingAuthority: '0' }
            if (command === 'pds_lww_binding_state') return bindingContext().state
            return command === 'server_sync_lww_pending_count' ? 600 : null
        })
        const controller = production.getServerSyncController()
        const stop = controller.watchProgress()
        const { bindSyncTarget } = await import('./bindingRegistry')
        vi.mocked(bindSyncTarget).mockImplementation(async () => { await f.transport!.publishInitialSharedState(bindingContext()); return { kind: 'bound' } as never })
        const connecting = production.connectServerSync({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' })
        await settle()
        await vi.advanceTimersByTimeAsync(500); await settle()
        expect(controller.snapshot().progress).toMatchObject({ mode: 'full', plannedSend: 600, plannedReceive: 100 })
        await vi.advanceTimersByTimeAsync(1000); await settle()
        const progress = controller.snapshot().progress!
        expect(progress).toMatchObject({ mode: 'full', plannedSend: 600, plannedReceive: 100 })
        const view = serverSyncProgressView(progress, languageKorean.risuNest.serverSync, Date.now())
        expect(view.detail).toBe('512 / 600')
        expect(view.counters.find(counter => counter.key === 'items')?.value).toBe('612 / 700')
        expect(pendingReads()).toBe(1)
        finish(); await connecting
        stop()
    })
    it.each([
        { name: 'moved nothing', reads: [{}], watched: true, failure: undefined },
        { name: 'failed', reads: [{}, { itemsDone: 1, itemsTotal: 2 }], watched: true, failure: { code: 'server-unreachable', retryable: true } },
        { name: 'was not watched', reads: [{}, { itemsDone: 2, itemsTotal: 2 }], watched: false, failure: undefined },
    ])('shows no finished bar for a routine attempt that $name', async ({ reads, watched, failure }) => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const finish = countedPush([...reads])
        const controller = production.getServerSyncController()
        const stop = watched ? controller.watchProgress() : () => {}
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        await vi.advanceTimersByTimeAsync(500); await settle()
        finish(failure); await settle()
        expect(controller.snapshot().progress).toBeUndefined()
        expect(controller.snapshot().finished).toBeUndefined()
        stop()
    })
    it('estimates the time left of an asset download from its own rate once it ran for five seconds', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        let reads = 0
        f.invoke.mockImplementation(async command => {
            if (command !== 'server_sync_progress') return null
            const index = reads++
            return sendLanes({}).map(lane => lane.lane === 'assets' && index > 0 ? { ...lane, active: true, step: 'downloading', bytesDone: 100 * index, bytesTotal: 10_000, assetScope: { id: 1, done: index, total: 100, settled: false } } : lane)
        })
        const controller = production.getServerSyncController()
        const stop = controller.watchProgress()
        let finish!: () => void
        const download = controller.track('assets', () => new Promise<void>(resolve => { finish = resolve }))
        await settle()
        await vi.advanceTimersByTimeAsync(5000); await settle()
        expect(controller.snapshot().progress?.remainingMs).toBeUndefined()
        await vi.advanceTimersByTimeAsync(500); await settle()
        // 1,000 bytes over five seconds, with 8,900 left, and the files it counts predict the same.
        expect(controller.snapshot().progress?.remainingMs).toBe(44_500)
        finish(); await download
        stop()
    })
    it('shows no finished bar after an asset download', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        countedPush([{}, { itemsDone: 2, itemsTotal: 2 }])
        const controller = production.getServerSyncController()
        const stop = controller.watchProgress()
        await controller.track('assets', async () => { await vi.advanceTimersByTimeAsync(500) }); await settle()
        expect(controller.snapshot().progress).toBeUndefined()
        expect(controller.snapshot().finished).toBeUndefined()
        stop()
    })
    it('shows the pages read after connecting as automatic sync under one stage', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true })
        const pages: Array<() => void> = []
        let served = 0, reads = 0
        const page = (index: number) => ({ bindingAuthority: '0', requestId: `page-${index}`, changes: index < 3 ? [{ key: JSON.stringify(['root', `key-${index}`]), value: { kind: 'inline' } }] : [], progress: { kind: 'server', cursor: String(index) }, admittedTimeUpperMs: '0' })
        f.invoke.mockImplementation(async command => {
            if (command === 'server_sync_lww_pull') { const index = served++; return index < 3 ? new Promise(resolve => pages.push(() => resolve(page(index)))) : page(index) }
            // Each read reports more of a receive whose backlog is known.
            if (command === 'server_sync_progress') return sendLanes({}).map(lane => lane.lane === 'receive' ? { ...lane, active: true, step: 'downloading', backlogDone: 10 * reads++, backlogLeft: 30 } : lane)
            if (command === 'server_sync_status') return { configured: true, writerId: 'writer', bindingAuthority: '0' }
            if (command === 'pds_lww_binding_state') return bindingContext().state
            return command === 'server_sync_lww_pending_count' ? 0 : null
        })
        const controller = production.getServerSyncController()
        const stop = controller.watchProgress()
        const { bindSyncTarget } = await import('./bindingRegistry')
        let bindingMode: string | undefined
        vi.mocked(bindSyncTarget).mockImplementation(async () => {
            const context = bindingContext()
            await f.transport!.inspectTarget(context)
            await f.transport!.resumeBinding(context)
            await settle()
            bindingMode = controller.snapshot().progress?.mode
            return { kind: 'bound' } as never
        })
        await production.connectServerSync({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' })
        expect(bindingMode).toBe('full')
        expect(served).toBe(1)
        const seen: unknown[] = []
        let applying!: () => void
        f.apply.mockImplementationOnce(() => new Promise(resolve => { applying = () => resolve({ revision: 31, affectedKeys: [], heldKeys: [], deferredKeys: [] }) }))
        for (let index = 0; index < 3; index++) {
            await vi.advanceTimersByTimeAsync(500); await settle()
            const progress = controller.snapshot().progress!
            seen.push({ mode: progress.mode, stages: progress.stages, label: serverSyncRoutineView(progress, languageKorean.risuNest.serverSync, false)?.label })
            pages.shift()!(); await settle()
            if (index === 0) { expect(controller.snapshot().progress?.active).toEqual(['downloading']); applying(); await settle() }
        }
        expect(seen).toEqual(Array(3).fill({ mode: 'routine', stages: ['preparing', 'downloading', 'publishing', 'assets'], label: languageKorean.risuNest.serverSync.running }))
        expect(served).toBeGreaterThanOrEqual(4)
        stop()
    })
    it.each(['replaceFromTarget', 'replaceAsNewDevice'] as const)('shows a running %s as applying what was received', async method => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const native = await import('./bindingNative')
        let finish!: () => void
        const replace = vi.mocked<(...args: unknown[]) => Promise<unknown>>(method === 'replaceFromTarget' ? native.replaceNativeSyncBinding : native.replaceNativeSyncBindingAsNewDevice)
        replace.mockImplementationOnce(() => new Promise(resolve => { finish = () => resolve(undefined) }))
        const replaced = (f.transport![method] as (...args: unknown[]) => Promise<unknown>)({}, {}, bindingContext())
        await settle()
        expect(production.getServerSyncController().snapshot().progress).toMatchObject({ stages: ['applying'], active: ['applying'] })
        finish(); await replaced
    })
    it('shows the switch to a received server state as applying it, and a switch with nothing received as no stage', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const { bindSyncTarget } = await import('./bindingRegistry')
        const controller = production.getServerSyncController()
        const state = bindingContext().state
        let switched!: () => void
        f.switchTarget.mockImplementation(() => new Promise(resolve => { switched = () => resolve(state) }))
        const seen: unknown[] = []
        vi.mocked(bindSyncTarget).mockImplementation(async () => {
            await f.transport!.inspectTarget(bindingContext())
            const pending = f.dependencies!.native.switchTarget(state, state.target, null, 'switch', false)
            await settle(); seen.push(controller.snapshot().progress?.active); switched(); await pending
            await f.transport!.pullAvailableState({ inspectionId: 'inspection' } as never, bindingContext())
            const staged = f.dependencies!.native.switchTarget(state, state.target, null, 'switch', false)
            await settle(); seen.push(controller.snapshot().progress?.active); switched(); await staged
            return { kind: 'cancelled' }
        })
        await production.connectServerSync({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' })
        expect(seen).toEqual([[], ['applying']])
        expect(f.switchTarget).toHaveBeenCalledTimes(2)
    })
    it('does not count the time a binding waits for the user to answer as elapsed', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const { bindSyncTarget } = await import('./bindingRegistry')
        const controller = production.getServerSyncController()
        const elapsed = () => serverSyncProgressView(controller.snapshot().progress!, languageKorean.risuNest.serverSync, Date.now()).counters.find(counter => counter.key === 'elapsed')?.value
        let ask!: () => void
        let answer!: (value: boolean) => void
        let work!: () => void
        vi.mocked(bindSyncTarget).mockImplementation(async () => {
            await f.transport!.inspectTarget(bindingContext())
            await new Promise<void>(resolve => { ask = resolve })
            await f.dependencies!.whileAsking(() => new Promise<boolean>(resolve => { answer = resolve }))
            await f.transport!.pullAvailableState({ inspectionId: 'inspection' } as never, bindingContext())
            await new Promise<void>(resolve => { work = resolve })
            return { kind: 'cancelled' }
        })
        const connected = production.connectServerSync({ endpoint: 'https://synthetic.invalid', libraryId: 'library', deviceId: 'registration', token: 'synthetic' })
        await settle()
        await vi.advanceTimersByTimeAsync(3_000)
        expect(elapsed()).toBe('00:03')
        ask(); await settle()
        await vi.advanceTimersByTimeAsync(600_000)
        expect(elapsed()).toBe('00:03')
        const startedAt = controller.snapshot().progress!.startedAt
        answer(true); await settle()
        await vi.advanceTimersByTimeAsync(2_000)
        expect(elapsed()).toBe('00:05')
        // The attempt keeps its start, so the panel that waits for it to run a moment stays shown.
        expect(controller.snapshot().progress!.startedAt).toBe(startedAt)
        // An answer outside a binding runs without touching any attempt.
        await expect(f.dependencies!.whileAsking(async () => 'answer')).resolves.toBe('answer')
        work(); await connected
    })
    it('keeps the last successful time when an attempt fails', async () => {
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        const finish = hangingPush([])
        visible(true); await f.transport!.resumeBinding(bindingContext()); await settle()
        finish({ code: 'server-unreachable', retryable: true }); await settle()
        const controller = production.getServerSyncController()
        expect(controller.snapshot().progress).toBeUndefined()
        expect(controller.snapshot().lastSuccessAt).toBeUndefined()
        expect(controller.snapshot().error).toBe('server-unreachable')
    })
})
