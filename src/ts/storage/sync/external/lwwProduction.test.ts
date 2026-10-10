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
vi.mock('@tauri-apps/api/core', () => ({ invoke: fixture.invoke, Channel: class { onmessage = (_value: unknown) => {} } }))
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
import { installExternalLwwAdapters, refreshExternalLwwAdapters, externalLwwExitDrain, requestExternalLwwNow, subscribeExternalLwwFailures } from './lwwProduction'
import { externalProgressFor, subscribeExternalProgress, type ExternalOperationProgress } from './progress'
const binding = { target: { kind: 'external' as const, connectionId: 'sync' }, targetAuthority: '4', selectionEpoch: 'selection', libraryId: 'library', progress: [] }
const context = (): BindingContext => ({ state: binding, signal: new AbortController().signal })
const state = (providers = ['webdav']): ExternalStorageState => ({ connections: providers.map((providerId, i) => ({ id: i ? providerId : 'sync', providerId, purpose: 'sync' })) } as ExternalStorageState)
let dispose: (() => void) | undefined
beforeEach(() => {
    vi.useFakeTimers(); fixture.native = true; fixture.registrations.clear()
    fixture.invoke.mockReset(); fixture.invoke.mockImplementation(async command => command === 'external_lww_receive' ? null : command === 'external_lww_publish' ? { segments: '0', more: false } : undefined)
    fixture.state.mockResolvedValue(binding); fixture.listen.mockResolvedValue(() => {})
    fixture.resumeCurrent.mockReset(); fixture.resumeCurrent.mockImplementation(async target => { const current = await fixture.state(); await fixture.registrations.get(target.connectionId)!.resumeBinding({ state: current, signal: new AbortController().signal }) })
    fixture.apply.mockReset(); fixture.flush.mockReset(); fixture.mobile.mockReset(); fixture.mobile.mockImplementation(async (_name, work) => work({ signal: new AbortController().signal }))
    fixture.outbox.mockReset(); fixture.outbox.mockResolvedValue({ revision: 12, entries: [] })
    fixture.storageMutation.mockReset(); fixture.storageMutation.mockImplementation(async work => { await work(12) })
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' })
    Object.defineProperty(navigator, 'onLine', { configurable: true, value: true })
})
afterEach(() => { dispose?.(); dispose = undefined; vi.useRealTimers() })
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve() }
describe('native external LWW adapter', () => {
    it('observes confirmation waiting within the binding attempt', async () => {
        dispose = await installExternalLwwAdapters(state(), false)
        let progress: ReadonlyMap<string, ExternalOperationProgress> = new Map()
        const stop = subscribeExternalProgress(value => progress = value)
        const transport = fixture.registrations.get('sync')!
        await transport.observeBinding!(async () => {
            transport.setConfirmationPending!(true)
            expect(externalProgressFor(progress, 'sync')).toMatchObject({ kind: 'binding', stage: 'waiting', state: 'running' })
            transport.setConfirmationPending!(false)
            expect(externalProgressFor(progress, 'sync')?.stage).toBe('checking')
            return { kind: 'cancelled' }
        })
        expect(externalProgressFor(progress, 'sync')?.state).toBe('cancelled')
        stop()
    })
    it('resumes a persisted external target through shared flow ownership exactly once for the current authority', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        expect(fixture.resumeCurrent).toHaveBeenCalledExactlyOnceWith(binding.target)
        await refreshExternalLwwAdapters(state()); await settle()
        expect(fixture.resumeCurrent).toHaveBeenCalledOnce()
    })
    it('registers the transports but leaves a bound target stopped when the start left sync off', async () => {
        dispose = await installExternalLwwAdapters(state(), false); await settle()
        expect(fixture.registrations.has('sync')).toBe(true)
        await refreshExternalLwwAdapters(state()); await vi.advanceTimersByTimeAsync(60_000); await settle()
        expect(fixture.resumeCurrent).not.toHaveBeenCalled()
        expect(fixture.invoke).not.toHaveBeenCalled()
        // Turning the switch on in settings resumes the target through the registered transport.
        await fixture.registrations.get('sync')!.resumeBinding(context()); await settle()
        expect(fixture.invoke.mock.calls.map(call => call[0])).toContain('external_lww_resume')
        dispose(); dispose = undefined
        dispose = await installExternalLwwAdapters(state()); await settle()
        expect(fixture.resumeCurrent).toHaveBeenCalledExactlyOnceWith(binding.target)
    })
    it('manual Sync Now resumes a bound target the start left stopped before it publishes', async () => {
        dispose = await installExternalLwwAdapters(state(), false); await settle()
        fixture.resumeCurrent.mockClear(); fixture.invoke.mockClear()
        await requestExternalLwwNow('sync')
        expect(fixture.resumeCurrent).toHaveBeenCalledExactlyOnceWith(binding.target)
        const commands = fixture.invoke.mock.calls.map(call => call[0])
        expect(commands.indexOf('external_lww_resume')).toBeGreaterThanOrEqual(0)
        expect(commands.indexOf('external_lww_resume')).toBeLessThan(commands.indexOf('external_lww_publish'))
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
        const next = { header: { bindingAuthority: '4', requestId: 'synthetic-receive-next' }, changes: [] }
        fixture.invoke.mockClear(); fixture.apply.mockClear()
        fixture.invoke.mockResolvedValueOnce(request).mockResolvedValueOnce(next); await transport.receiveAvailableChanges(context())
        expect(fixture.invoke.mock.calls.map(call => call[0])).toEqual(['external_lww_receive', 'external_lww_receive', 'external_lww_receive'])
        expect(fixture.apply.mock.calls).toEqual([[request], [next]])
        fixture.invoke.mockRejectedValueOnce({ kind: 'corrupt' })
        await expect(transport.receiveAvailableChanges(context())).rejects.toEqual({ kind: 'corrupt' })
    })
    it('shows why sync stopped when a committed switch cannot resume', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        let failures: ReadonlyMap<string, unknown> = new Map()
        const unsubscribe = subscribeExternalLwwFailures(value => { failures = value })
        const transport = fixture.registrations.get('sync')!
        transport.reportStopped!(new DOMException('aborted', 'AbortError'))
        expect(failures.has('sync')).toBe(false)
        transport.reportStopped!({ kind: 'previousStorageUnavailable' })
        expect(failures.get('sync')).toEqual({ kind: 'previousStorageUnavailable' })
        transport.reportStopped!(new AggregateError([{ kind: 'corrupt' }], 'stopped'))
        expect(failures.get('sync')).toEqual({ kind: 'corrupt' })
        unsubscribe()
    })
    it('keeps checking after clock skew and stops only on corruption', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const receives = () => fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_receive').length
        fixture.invoke.mockImplementation(async command => { if (command === 'external_lww_receive') throw { kind: 'clockSkew' } })
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        const skewed = receives()
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        expect(receives()).toBe(skewed + 1)
        fixture.invoke.mockImplementation(async command => command === 'external_lww_receive' ? null : command === 'external_lww_publish' ? { segments: '0', more: false } : undefined)
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        expect(receives()).toBe(skewed + 2)
        fixture.invoke.mockImplementation(async command => { if (command === 'external_lww_receive') throw { kind: 'corrupt' } })
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        const corrupt = receives()
        await vi.advanceTimersByTimeAsync(600_000); await settle()
        expect(receives()).toBe(corrupt)
    })
    it('stops when the connection reaches another repository', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const receives = () => fixture.invoke.mock.calls.filter(call => call[0] === 'external_lww_receive').length
        fixture.invoke.mockImplementation(async command => { if (command === 'external_lww_receive') throw { kind: 'repositoryMismatch' } })
        await vi.advanceTimersByTimeAsync(20_000); await settle()
        const refused = receives()
        expect(refused).toBeGreaterThan(0)
        await vi.advanceTimersByTimeAsync(600_000); await settle()
        expect(receives()).toBe(refused)
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
        fixture.invoke.mockImplementation(async command => { if (command === 'external_lww_publish') { await new Promise<void>(resolve => { finish = resolve }); return { segments: '0', more: false } } })
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
        await transport.resumeBinding(context()); await settle()
        expect(fixture.invoke.mock.calls.slice(0, 2)).toEqual([
            ['pds_lww_finish_initial_publication', { request: { bindingAuthority: '4', requestId: expect.any(String) } }],
            ['external_lww_resume', { connectionId: 'sync' }],
        ])
        fixture.invoke.mockClear(); fixture.flush.mockClear()
        await transport.publishInitialSharedState(context())
        expect(fixture.invoke.mock.calls.map(call => call[0])).toEqual(['external_lww_publish'])
        expect(fixture.invoke.mock.calls[0][1]).not.toHaveProperty('initial')
        expect(fixture.invoke.mock.calls[0][1].request).not.toHaveProperty('turnLimit')
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

describe('bounded production calls and lifecycle ownership', () => {
    it('yields receive after four pages so a bounded send can advance', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        const calls: string[] = []; let pages = 0
        fixture.invoke.mockImplementation(async (command, args) => {
            if (command === 'external_lww_receive') {
                calls.push('receive')
                if (++pages === 1) fixture.revision!(14, 'generation-complete')
                return pages <= 9 ? { header: { bindingAuthority: '4', requestId: String(pages) }, changes: [] } : null
            }
            if (command === 'external_lww_publish') {
                calls.push('publish')
                expect(args.request.turnLimit).toBe(4)
                return { segments: '1', more: false }
            }
        })
        fixture.viewport!({}); for (let i = 0; i < 8; i++) await settle()
        expect(calls).toEqual(['receive', 'receive', 'receive', 'receive', 'publish', 'receive', 'receive', 'receive', 'receive', 'receive', 'receive'])
        expect(fixture.apply).toHaveBeenCalledTimes(9)
    })
    it('manual completion drains more than four pages with no publish limit', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
        let received = 0
        fixture.invoke.mockImplementation(async (command, args) => {
            if (command === 'external_lww_publish') { expect(args.request).not.toHaveProperty('turnLimit'); return { segments: '7', more: false } }
            if (command === 'external_lww_receive') return ++received <= 9 ? { header: { bindingAuthority: '4', requestId: String(received) }, changes: [] } : null
        })
        await requestExternalLwwNow('sync')
        expect(received).toBe(10)
        expect(fixture.apply).toHaveBeenCalledTimes(9)
    })
    it('clears a receive failure only after successful receive', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        let failures: ReadonlyMap<string, unknown> = new Map()
        const unsubscribe = subscribeExternalLwwFailures(value => { failures = value })
        const failure = { kind: 'transient', retryAtMs: String(Date.now() + 120_000) }
        fixture.invoke.mockImplementation(async command => {
            if (command === 'external_lww_receive') throw failure
            if (command === 'external_lww_publish') return { segments: '0', more: false }
        })
        fixture.viewport!({}); await settle()
        expect(failures.get('sync')).toEqual(failure)
        fixture.revision!(15, 'generation-complete'); await settle()
        expect(failures.get('sync')).toEqual(failure)
        fixture.invoke.mockImplementation(async command => command === 'external_lww_receive' ? null : { segments: '0', more: false })
        await vi.advanceTimersByTimeAsync(120_000); await settle()
        expect(failures.has('sync')).toBe(false)
        unsubscribe()
    })
    it('keeps a held hide flush alive across duplicate hidden notifications and obeys expiration', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
        const expiration = new AbortController(); let complete!: () => void
        fixture.mobile.mockImplementation(async (_name, work) => work({ signal: expiration.signal }))
        fixture.invoke.mockImplementation(async command => {
            if (command === 'external_lww_publish') { await new Promise<void>(resolve => { complete = resolve }); return { segments: '1', more: false } }
            if (command === 'external_lww_receive') return null
        })
        Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' }); document.dispatchEvent(new Event('visibilitychange')); await settle()
        document.dispatchEvent(new Event('visibilitychange')); await settle()
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_publish')).toHaveLength(1)
        expiration.abort(); complete(); await settle()
        await vi.advanceTimersByTimeAsync(600_000)
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_receive')).toHaveLength(0)
        expect(fixture.mobile).toHaveBeenCalledTimes(1)
    })
    it('expires a hide request waiting for an obsolete receive without admitting publication', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
        let complete!: () => void
        const expiration = new AbortController()
        fixture.mobile.mockImplementation(async (_name, work) => work({ signal: expiration.signal }))
        fixture.invoke.mockImplementation(async command => {
            if (command === 'external_lww_receive') { await new Promise<void>(resolve => { complete = resolve }); return null }
            if (command === 'external_lww_publish') return { segments: '1', more: false }
        })
        fixture.viewport!({}); await settle()
        Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' }); document.dispatchEvent(new Event('visibilitychange')); await settle()
        expiration.abort(); complete(); await settle()
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_publish')).toHaveLength(0)
    })
    it('coalesces foreground notifications and does not revive a removed binding after a late result', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
        let complete!: () => void
        fixture.invoke.mockImplementation(async command => {
            if (command === 'external_lww_receive') { await new Promise<void>(resolve => { complete = resolve }); return null }
            if (command === 'external_lww_publish') return { segments: '0', more: false }
        })
        fixture.viewport!({}); await settle()
        for (let i = 0; i < 20; i++) window.dispatchEvent(new Event('online'))
        await settle()
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_resume')).toHaveLength(0)
        fixture.state.mockResolvedValue({ ...binding, target: { kind: 'none' }, targetAuthority: '5' })
        await refreshExternalLwwAdapters({ connections: [] } as unknown as ExternalStorageState)
        complete(); await settle()
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_resume')).toHaveLength(0)
    })
    it('coalesces foreground notifications into one restart after a held old call', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
        let complete!: () => void; let first = true
        fixture.invoke.mockImplementation(async command => {
            if (command === 'external_lww_receive') {
                if (first) { first = false; await new Promise<void>(resolve => { complete = resolve }) }
                return null
            }
            if (command === 'external_lww_publish') return { segments: '0', more: false }
        })
        fixture.viewport!({}); await settle()
        for (let i = 0; i < 20; i++) window.dispatchEvent(new Event('online'))
        complete(); await settle(); await settle()
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_resume')).toHaveLength(1)
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_publish')).toHaveLength(1)
        expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_receive')).toHaveLength(2)
    })
    it('keeps empty sync progress hidden and exposes meaningful bounded publication', async () => {
        dispose = await installExternalLwwAdapters(state()); await settle()
        let progress: ReadonlyMap<string, ExternalOperationProgress> = new Map()
        const unsubscribe = subscribeExternalProgress(value => { progress = value })
        await requestExternalLwwNow('sync')
        expect(externalProgressFor(progress, 'sync')).toBeUndefined()
        fixture.invoke.mockImplementation(async command => command === 'external_lww_publish' ? { segments: '1', more: false } : null)
        fixture.revision!(20, 'generation-complete'); await settle()
        expect(externalProgressFor(progress, 'sync')).toMatchObject({ state: 'complete', visible: true })
        unsubscribe()
    })
})

it('rechecks foreground intent after hide and show supersede a pending restart', async () => {
    dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
    let complete!: () => void; let first = true
    fixture.invoke.mockImplementation(async command => {
        if (command === 'external_lww_receive') {
            if (first) { first = false; await new Promise<void>(resolve => { complete = resolve }) }
            return null
        }
        if (command === 'external_lww_publish') return { segments: '0', more: false }
    })
    fixture.viewport!({}); await settle()
    window.dispatchEvent(new Event('online')); await settle()
    Object.defineProperty(navigator, 'onLine', { configurable: true, value: false }); window.dispatchEvent(new Event('offline'))
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' }); document.dispatchEvent(new Event('visibilitychange'))
    Object.defineProperty(navigator, 'onLine', { configurable: true, value: true })
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' }); document.dispatchEvent(new Event('visibilitychange'))
    complete(); for (let i = 0; i < 8; i++) await settle()
    expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_resume')).toHaveLength(1)
    expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_publish')).toHaveLength(1)
    expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_receive')).toHaveLength(2)
})

it('does not restart a disposed adapter after exit cancellation resume returns late', async () => {
    dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
    const drain = externalLwwExitDrain('sync', 'selection')!
    let complete!: () => void
    fixture.invoke.mockImplementation(async command => {
        if (command === 'external_lww_resume') await new Promise<void>(resolve => { complete = resolve })
        if (command === 'external_lww_publish') return { segments: '0', more: false }
        if (command === 'external_lww_receive') return null
    })
    const resume = drain.resumeAfterExitCancel!(); await settle()
    dispose!(); dispose = undefined
    complete(); await resume; await vi.advanceTimersByTimeAsync(120_000)
    expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_publish')).toHaveLength(0)
})

it('preserves a valid manual lifetime when foreground notifications arrive during publication', async () => {
    dispose = await installExternalLwwAdapters(state()); await settle(); fixture.invoke.mockClear()
    let complete!: () => void; let first = true
    fixture.invoke.mockImplementation(async command => {
        if (command === 'external_lww_publish') {
            if (first) { first = false; await new Promise<void>(resolve => { complete = resolve }) }
            return { segments: '0', more: false }
        }
        if (command === 'external_lww_receive') return null
    })
    const manual = requestExternalLwwNow('sync'); await settle()
    for (let i = 0; i < 20; i++) window.dispatchEvent(new Event('online'))
    await settle(); complete(); await expect(manual).resolves.toBeUndefined()
    for (let i = 0; i < 4; i++) await settle()
    expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_resume')).toHaveLength(1)
    expect(fixture.invoke.mock.calls.filter(([command]) => command === 'external_lww_receive')).toHaveLength(2)
})
