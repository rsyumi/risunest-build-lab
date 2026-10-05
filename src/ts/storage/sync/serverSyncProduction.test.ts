import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SyncBindingTransport, SyncBindingDependencies } from './bindingFlow'

const f = vi.hoisted(() => ({
    native: true,
    selectedIndex: 0,
    database: { characters: [{ chaId: 'selected-stable-id' }] },
    invoke: vi.fn(),
    install: vi.fn(),
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
    dependencies: undefined as Pick<SyncBindingDependencies, 'withPausedWrites' | 'beginActivatedLibraryGuard' | 'refreshActivatedLibrary' | 'recovery'> | undefined,
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
vi.mock('./bindingNative', () => ({ replaceNativeSyncBinding: vi.fn(), replaceNativeSyncBindingAsNewDevice: vi.fn() }))
vi.mock('../persistentDataRuntime.svelte', () => ({
    withPausedPersistentWrites: f.paused,
    beginActivatedLibraryGuard: f.guard,
    refreshActivatedLibraryUnderPause: f.refresh,
    applyPersistentLwwReceive: f.apply,
    flushPendingDataLocally: f.flush,
    getPersistentDataRuntime: () => f.runtime,
}))
vi.mock('../../mobileBackgroundTask', () => ({ runWithMobileBackgroundTask: f.mobile }))
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
        expect(production.getServerSyncController().snapshot()).toMatchObject({ status: { bound: true }, paused: true, error: 'server-unreachable' })
        await production.retryServerSync()
        expect(f.resumeCurrent).toHaveBeenCalledExactlyOnceWith(state.target)
    })
    it('guards native initialization and transport registration on the web', async () => {
        f.native = false
        production.initializeNativeSyncBindings(); await production.installServerSyncProduction()
        expect(f.install).not.toHaveBeenCalled(); expect(f.register).not.toHaveBeenCalled(); expect(f.invoke).not.toHaveBeenCalled()
    })
})
