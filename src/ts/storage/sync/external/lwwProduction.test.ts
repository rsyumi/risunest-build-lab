// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BindingContext, SyncBindingTransport } from '../bindingFlow'
import type { ExternalStorageState } from './types'
const fixture = vi.hoisted(() => ({
    native: true,
    invoke: vi.fn(), listen: vi.fn(), state: vi.fn(), apply: vi.fn(), flush: vi.fn(), mobile: vi.fn(), outbox: vi.fn(), storageMutation: vi.fn(), resumeCurrent: vi.fn(),
    registrations: new Map<string, SyncBindingTransport>(), viewport: undefined as undefined | ((source: unknown) => void),
    revision: undefined as undefined | ((revision: number, cause: string) => void),
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: fixture.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ listen: fixture.listen }))
vi.mock('../../../platform', () => ({ get isTauri() { return fixture.native } }))
vi.mock('../../../mobileBackgroundTask', () => ({ runWithMobileBackgroundTask: fixture.mobile }))
vi.mock('../../generatingConversationRegistry', () => ({ generatingConversations: { snapshot: () => [] } }))
vi.mock('../../persistentDataRuntime.svelte', () => ({
    flushPendingDataLocally: fixture.flush,
    getPersistentDataRuntime: () => ({ store: { lwwReadOutbox: fixture.outbox }, runStorageOnlyMutation: fixture.storageMutation, applyLwwReceive: fixture.apply, subscribeActiveConversationViewportSource(callback: (source: unknown) => void) { fixture.viewport = callback; return () => {} }, captureSelectedConversationTarget: () => ({ characterId: 'char', conversationId: 'conversation' }) }),
}))
vi.mock('../../persistentRevisionEvents', () => ({ subscribeLocalPersistentRevision(callback: (revision: number, cause: string) => void) { fixture.revision = callback; return () => {} } }))
vi.mock('../bindingNative', () => ({ createNativeSyncBindingBridge: () => ({ state: fixture.state }), replaceNativeSyncBinding: vi.fn(), replaceNativeSyncBindingAsNewDevice: vi.fn() }))
vi.mock('../bindingRegistry', () => ({ resumeCurrentSyncBinding: fixture.resumeCurrent, registerSyncBindingTransport(target: { connectionId: string }, transport: SyncBindingTransport) { fixture.registrations.set(target.connectionId, transport); return () => fixture.registrations.delete(target.connectionId) } }))
import { installExternalLwwAdapters, refreshExternalLwwAdapters, externalLwwExitDrain, requestExternalLwwNow } from './lwwProduction'
const binding = { target: { kind: 'external' as const, connectionId: 'sync' }, targetAuthority: '4', selectionEpoch: 'selection', libraryId: 'library', progress: [] }
const context = (): BindingContext => ({ state: binding, signal: new AbortController().signal })
const state = (providers = ['webdav']): ExternalStorageState => ({ connections: providers.map((providerId, i) => ({ id: i ? providerId : 'sync', providerId, purpose: 'sync' })) } as ExternalStorageState)
let dispose: (() => void) | undefined
beforeEach(() => {
    vi.useFakeTimers(); fixture.native = true; fixture.registrations.clear()
    fixture.invoke.mockReset(); fixture.invoke.mockImplementation(async command => command === 'external_lww_receive' ? [] : undefined)
    fixture.state.mockResolvedValue(binding); fixture.listen.mockResolvedValue(() => {})
    fixture.resumeCurrent.mockReset(); fixture.resumeCurrent.mockImplementation(async target => { const current = await fixture.state(); await fixture.registrations.get(target.connectionId)!.resumeBinding({ state: current, signal: new AbortController().signal }) })
    fixture.apply.mockReset(); fixture.flush.mockReset(); fixture.mobile.mockImplementation(async (_name, work) => work())
    fixture.outbox.mockReset(); fixture.outbox.mockResolvedValue({ revision: 12, entries: [] })
    fixture.storageMutation.mockReset(); fixture.storageMutation.mockImplementation(async work => { await work(12) })
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' })
    Object.defineProperty(navigator, 'onLine', { configurable: true, value: true })
})
afterEach(() => { dispose?.(); dispose = undefined; vi.useRealTimers() })
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve() }
describe('native external LWW adapter', () => {
    it('resumes a persisted external target through shared flow ownership exactly once for the current authority', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        expect(fixture.resumeCurrent).toHaveBeenCalledExactlyOnceWith(binding.target)
        await refreshExternalLwwAdapters(state()); await settle()
        expect(fixture.resumeCurrent).toHaveBeenCalledOnce()
    })
    it('registers four eligible native providers and guards the web', async () => {
        dispose = await installExternalLwwAdapters(state(['webdav', 's3', 'google_drive', 'onedrive', 'mybox', 'github_releases', 'gitlab_packages']))
        expect([...fixture.registrations.keys()]).toEqual(['sync', 's3', 'google_drive', 'onedrive'])
        dispose(); dispose = undefined; fixture.native = false; fixture.invoke.mockClear(); fixture.state.mockClear()
        dispose = await installExternalLwwAdapters(state())
        expect(fixture.registrations.size).toBe(0); expect(fixture.invoke).not.toHaveBeenCalled(); expect(fixture.state).not.toHaveBeenCalled()
    })
    it('receives available changes through runtime admission and propagates failures', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const transport = fixture.registrations.get('sync') as SyncBindingTransport & { receiveAvailableChanges(context: BindingContext): Promise<void> }
        const request = { header: { bindingAuthority: '4', requestId: 'synthetic-receive' }, changes: [] }
        fixture.invoke.mockResolvedValueOnce([request]); await transport.receiveAvailableChanges(context())
        expect(fixture.apply).toHaveBeenCalledWith(request)
        fixture.invoke.mockRejectedValueOnce({ kind: 'corrupt' })
        await expect(transport.receiveAvailableChanges(context())).rejects.toEqual({ kind: 'corrupt' })
    })
    it('keeps checking after clock skew and stops only on corruption', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const receives = () => fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_receive').length
        fixture.invoke.mockImplementation(async command => { if (command === 'external_lww_receive') throw { kind: 'clockSkew' } })
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        const skewed = receives()
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        expect(receives()).toBe(skewed + 1)
        fixture.invoke.mockImplementation(async command => command === 'external_lww_receive' ? [] : undefined)
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        expect(receives()).toBe(skewed + 2)
        fixture.invoke.mockImplementation(async command => { if (command === 'external_lww_receive') throw { kind: 'corrupt' } })
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        const corrupt = receives()
        await vi.advanceTimersByTimeAsync(600_000); await settle()
        expect(receives()).toBe(corrupt)
    })
    it('publishes the actual generation-complete revision cause immediately over a pending ordinary debounce', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        fixture.invoke.mockClear(); fixture.mobile.mockClear(); fixture.flush.mockClear()
        fixture.revision!(12, 'edit')
        await vi.advanceTimersByTimeAsync(1_000)
        expect(fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_publish')).toHaveLength(0)
        const completedAt = Date.now()
        fixture.revision!(13, 'generation-complete'); await settle()
        expect(Date.now()).toBe(completedAt)
        expect(fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_publish')).toHaveLength(1)
        expect(fixture.flush).toHaveBeenCalledWith('external-lww-publish')
        expect(fixture.mobile).not.toHaveBeenCalled()
        await vi.advanceTimersByTimeAsync(15_000); await settle()
        expect(fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_publish')).toHaveLength(1)
    })
    it('flushes once on background entry and suspends all timed work', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
        Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' }); document.dispatchEvent(new Event('visibilitychange'))
        await settle(); expect(fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_publish')).toHaveLength(1)
        document.dispatchEvent(new Event('visibilitychange')); await vi.advanceTimersByTimeAsync(600_000); await settle()
        expect(fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_publish')).toHaveLength(1)
        expect(fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_receive')).toHaveLength(0)
    })
    it('passes frozen exit identity, waits for publication, and blocks stale selection', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const drain = externalLwwExitDrain('sync', 'selection', true)!; expect(drain.id).toBe('external:sync:selection')
        const target = { revision: 12, libraryEpoch: 'epoch', selectionEpoch: 'selection', selectionId: drain.id }
        let finish!: () => void
        fixture.invoke.mockImplementation(async command => { if (command === 'external_lww_publish') await new Promise<void>(resolve => { finish = resolve }) })
        let completed = false; const pending = drain.drain(target, new AbortController().signal).then(result => { completed = true; return result })
        await settle(); expect(completed).toBe(false)
        expect(fixture.invoke).toHaveBeenCalledWith('external_lww_publish', expect.objectContaining({ request: expect.objectContaining({ exitTarget: { revision: '12', libraryEpoch: 'epoch', selectionEpoch: 'selection' } }) }))
        finish(); expect(await pending).toEqual({ kind: 'complete' })
        expect(await drain.drain({ ...target, selectionEpoch: 'changed' }, new AbortController().signal)).toEqual({ kind: 'blocked', reason: 'sync-binding-changed' })
    })
    it('manual Sync Now publishes after local flush and completes available receive', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
        await requestExternalLwwNow('sync')
        expect(fixture.flush).toHaveBeenCalledWith('external-lww-publish')
        expect(fixture.invoke.mock.calls.map(call => call[0])).toEqual(['external_lww_publish', 'external_lww_receive'])
    })
    it('tells the native fence whether a binding change makes this a new device', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const transport = fixture.registrations.get('sync')!
        fixture.invoke.mockClear()
        await transport.fenceOldJobs(context())
        await transport.fenceOldJobs({ ...context(), mode: 'new-device' })
        await externalLwwExitDrain('sync', 'selection', true)!.cancel('cancel-exit')
        expect(fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_fence').map(call => call[1])).toEqual([
            { connectionId: 'sync', newDevice: false }, { connectionId: 'sync', newDevice: true }, { connectionId: 'sync', newDevice: false },
        ])
    })
    it('finishes an owed initial queue natively when the binding resumes and publishes it through the ordinary publication', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const transport = fixture.registrations.get('sync')!
        fixture.invoke.mockClear()
        await transport.resumeBinding(context())
        expect(fixture.invoke.mock.calls.slice(0, 2)).toEqual([
            ['pds_lww_finish_initial_publication', { request: { bindingAuthority: '4', requestId: expect.any(String) } }],
            ['external_lww_resume', { connectionId: 'sync' }],
        ])
        fixture.invoke.mockClear(); fixture.flush.mockClear()
        await transport.publishInitialSharedState(context())
        expect(fixture.invoke.mock.calls.map(call => call[0])).toEqual(['external_lww_publish'])
        expect(fixture.invoke.mock.calls[0][1]).not.toHaveProperty('initial')
        expect(fixture.flush).not.toHaveBeenCalled()
    })
    it('reconciles the authoritative revision when publication fails after native clock repair', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const transport = fixture.registrations.get('sync')!
        fixture.invoke.mockRejectedValueOnce({ kind: 'transient' })
        fixture.outbox.mockResolvedValueOnce({ revision: 13, entries: [] })
        let reconciled = 0
        fixture.storageMutation.mockImplementationOnce(async work => { reconciled = await work(12) })
        await expect(transport.publishInitialSharedState!(context())).rejects.toEqual({ kind: 'transient' })
        expect(reconciled).toBe(13)
        expect(fixture.outbox).toHaveBeenCalledWith(expect.objectContaining({ bindingAuthority: '4', limit: '1' }))
    })
})
