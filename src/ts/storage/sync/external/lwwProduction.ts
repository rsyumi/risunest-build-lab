import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { isTauri } from '../../../platform'
import { runWithMobileBackgroundTask } from '../../../mobileBackgroundTask'
import { generatingConversations } from '../../generatingConversationRegistry'
import { flushPendingDataLocally, getPersistentDataRuntime } from '../../persistentDataRuntime.svelte'
import { subscribeLocalPersistentRevision } from '../../persistentRevisionEvents'
import type { LwwStageReceive } from '../../persistentDataStore'
import type { SyncExitDrainAdapter } from '../../syncExitCoordinator'
import type { BindingContext, SyncBindingState, SyncBindingTransport, InspectedSyncTarget, StagedSyncTarget, NewDeviceBindingPreparation } from '../bindingFlow'
import { createNativeSyncBindingBridge, replaceNativeSyncBinding, replaceNativeSyncBindingAsNewDevice } from '../bindingNative'
import { registerSyncBindingTransport, resumeCurrentSyncBinding } from '../bindingRegistry'
import { SERVER_SYNC_DEVICE_CHANGED_EVENT } from '../serverSyncNativeSignals'
import { createLwwScheduler } from './lwwScheduler'
import type { ExternalStorageState } from './types'

interface Adapter { transport: SyncBindingTransport; scheduler: ReturnType<typeof createLwwScheduler>; dispose(): void; state?: SyncBindingState }
const adapters = new Map<string, Adapter>()
const failures = new Map<string, unknown>()
const failureListeners = new Set<(failures: ReadonlyMap<string, unknown>) => void>()
function reportFailure(id: string, error?: unknown): void {
    if (error === undefined) failures.delete(id)
    else failures.set(id, error)
    for (const listener of failureListeners) listener(new Map(failures))
}
export function subscribeExternalLwwFailures(listener: (failures: ReadonlyMap<string, unknown>) => void): () => void {
    failureListeners.add(listener); listener(new Map(failures))
    return () => { failureListeners.delete(listener) }
}
export function supportsExternalLwwNewDevice(id: string): boolean {
    const transport = adapters.get(id)?.transport
    return !!(transport?.prepareNewDeviceBinding && transport.replaceAsNewDevice && transport.resumeNewDeviceBinding)
}
const header = (connectionId: string, context: BindingContext, requestId: string = crypto.randomUUID()) => ({ connectionId, bindingAuthority: context.state.targetAuthority, requestId, generating: generatingConversations.snapshot() })
const native = createNativeSyncBindingBridge()

function createAdapter(connectionId: string): Adapter {
    let active: BindingContext | undefined
    const context = async (): Promise<BindingContext> => {
        const state = await native.state()
        if (state.target.kind !== 'external' || state.target.connectionId !== connectionId) throw new Error('Sync binding changed')
        return { state, signal: active?.signal ?? new AbortController().signal }
    }
    // Native code returns one bounded page per call until none remain.
    const receive = async (binding: BindingContext): Promise<number> => {
        let received = 0
        for (;;) {
            binding.signal.throwIfAborted()
            const request = await invoke<LwwStageReceive | null>('external_lww_receive', { request: header(connectionId, binding) })
            if (!request) return received
            binding.signal.throwIfAborted()
            await getPersistentDataRuntime().applyLwwReceive(request)
            received++
        }
    }
    const publish = async (binding?: BindingContext, flush = true): Promise<void> => {
        const current = binding ?? await context()
        current.signal.throwIfAborted()
        try {
            if (flush) await flushPendingDataLocally('external-lww-publish')
            await invoke('external_lww_publish', { request: header(connectionId, current) })
        } finally {
            current.signal.throwIfAborted()
            const runtime = getPersistentDataRuntime()
            await runtime.runStorageOnlyMutation(async () => {
                current.signal.throwIfAborted()
                const page = await runtime.store.lwwReadOutbox({ bindingAuthority: current.state.targetAuthority, requestId: crypto.randomUUID(), limit: '1' })
                return page.revision
            })
        }
        reportFailure(connectionId)
    }
    const scheduler = createLwwScheduler({
        available: () => !!active && !active.signal.aborted && document.visibilityState !== 'hidden' && navigator.onLine,
        publish: () => publish(),
        receive: async () => receive(await context()),
        maintain: async () => { const current = await context(); current.signal.throwIfAborted(); await invoke('external_lww_maintenance', { request: header(connectionId, current) }) },
        failed: error => {
            reportFailure(connectionId, error)
            if ((error as { kind?: string })?.kind === 'corrupt') scheduler.stop()
        },
    })
    const transport: SyncBindingTransport & { receiveAvailableChanges(context: BindingContext): Promise<void> } = {
        inspectTarget: binding => invoke<InspectedSyncTarget>('external_lww_inspect', { request: header(connectionId, binding) }),
        pullAvailableState: (target, binding) => invoke<StagedSyncTarget>('external_lww_stage_binding', { request: {
            ...header(connectionId, binding), inspectionId: target.inspectionId, targetId: target.targetId, libraryId: target.libraryId,
        } }),
        replaceFromTarget: replaceNativeSyncBinding,
        publishInitialSharedState: binding => publish(binding, false),
        async resumeBinding(binding) {
            binding.signal.throwIfAborted()
            await invoke('pds_lww_finish_initial_publication', { request: { bindingAuthority: binding.state.targetAuthority, requestId: crypto.randomUUID() } })
            await invoke('external_lww_resume', { connectionId })
            reportFailure(connectionId)
            active = binding
            adapter.state = binding.state
            scheduler.start()
        },
        async fenceOldJobs(binding) {
            scheduler.stop()
            active = undefined
            adapter.state = undefined
            const old = binding.state.target
            if (old.kind === 'external') {
                const adapter = adapters.get(old.connectionId)
                adapter?.scheduler.stop()
                if (adapter) adapter.state = undefined
                await invoke('external_lww_fence', { connectionId: old.connectionId, newDevice: binding.mode === 'new-device' })
                await adapter?.scheduler.settled()
            }
            await scheduler.settled()
        },
        prepareNewDeviceBinding: (staged, binding) => invoke<NewDeviceBindingPreparation>('external_lww_prepare_new_device', {
            request: { ...header(connectionId, binding, staged.receiveId), stagingId: staged.stagingId },
        }),
        replaceAsNewDevice: replaceNativeSyncBindingAsNewDevice,
        async resumeNewDeviceBinding(_preparation, _result, binding) { await transport.resumeBinding(binding) },
        async receiveAvailableChanges(binding) { await receive(binding) },
    }
    const unregister = registerSyncBindingTransport({ kind: 'external', connectionId }, transport)
    const adapter: Adapter = { transport, scheduler, dispose() { scheduler.stop(); unregister(); if (adapters.get(connectionId) === adapter) adapters.delete(connectionId) } }
    return adapter
}

export async function refreshExternalLwwAdapters(state: ExternalStorageState): Promise<void> {
    if (!isTauri) return
    const sync = new Set(state.connections.filter(connection => connection.purpose === 'sync' && ['webdav', 's3', 'google_drive', 'onedrive'].includes(connection.providerId)).map(connection => connection.id))
    for (const [id, adapter] of adapters) if (!sync.has(id)) adapter.dispose()
    for (const id of sync) if (!adapters.has(id)) adapters.set(id, createAdapter(id))
    const binding = await native.state()
    for (const [id, adapter] of adapters) if (binding.target.kind !== 'external' || binding.target.connectionId !== id) {
        adapter.scheduler.stop(); adapter.state = undefined
    }
    if (binding.target.kind === 'external') {
        const adapter = adapters.get(binding.target.connectionId)
        if (adapter && adapter.state?.targetAuthority !== binding.targetAuthority) await resumeCurrentSyncBinding(binding.target)
    }
}

export async function installExternalLwwAdapters(state: ExternalStorageState): Promise<() => void> {
    if (!isTauri) return () => {}
    await refreshExternalLwwAdapters(state)
    const dirty = (generation = false) => { for (const adapter of adapters.values()) if (adapter.state) adapter.scheduler.dirty(generation) }
    const disposeRevision = subscribeLocalPersistentRevision((_revision, cause) => dirty(cause === 'generation-complete'))
    const disposeDevice = await listen(SERVER_SYNC_DEVICE_CHANGED_EVENT, () => dirty())
    let lastConversation = ''
    const disposeConversation = getPersistentDataRuntime().subscribeActiveConversationViewportSource(source => {
        if (!source) return
        const value = getPersistentDataRuntime().captureSelectedConversationTarget()
        if (!value) return
        const identity = JSON.stringify([value.characterId, value.conversationId])
        if (identity === lastConversation) return
        lastConversation = identity
        for (const adapter of adapters.values()) if (adapter.state) void adapter.scheduler.conversationOpened().catch(() => {})
    })
    let hiddenFlushed = document.visibilityState === 'hidden'
    const foreground = () => {
        const flushHidden = document.visibilityState === 'hidden' && !hiddenFlushed
        hiddenFlushed = document.visibilityState === 'hidden'
        for (const adapter of adapters.values()) if (adapter.state) {
            if (document.visibilityState === 'hidden' || !navigator.onLine) {
                adapter.scheduler.stop()
                if (flushHidden && navigator.onLine) void runWithMobileBackgroundTask('sync', () => adapter.scheduler.publishNow(true)).catch(() => {})
            }
            else void invoke('external_lww_resume', { connectionId: adapter.state.target.kind === 'external' ? adapter.state.target.connectionId : '' }).then(() => adapter.scheduler.resumeForeground()).catch(() => {})
        }
    }
    document.addEventListener('visibilitychange', foreground)
    window.addEventListener('online', foreground)
    window.addEventListener('offline', foreground)
    return () => {
        disposeRevision(); disposeDevice(); disposeConversation()
        document.removeEventListener('visibilitychange', foreground)
        window.removeEventListener('online', foreground); window.removeEventListener('offline', foreground)
        for (const adapter of [...adapters.values()]) adapter.dispose()
    }
}

export async function requestExternalLwwNow(connectionId: string): Promise<void> {
    const adapter = adapters.get(connectionId)
    if (!adapter) throw new Error('Sync binding transport is unavailable')
    await runWithMobileBackgroundTask('sync', async () => {
        await adapter.scheduler.publishNow(true)
        await adapter.scheduler.resumeForeground(false)
    })
}

export function externalLwwExitDrain(connectionId: string, selectionEpoch: string, fenced = false): SyncExitDrainAdapter | null {
    const adapter = adapters.get(connectionId)
    if (!adapter) return null
    return {
        id: `external:${connectionId}:${selectionEpoch}`,
        async drain(target, signal) {
            signal.throwIfAborted()
            adapter.scheduler.stop()
            if (target.selectionId !== `external:${connectionId}:${selectionEpoch}` || target.selectionEpoch !== selectionEpoch) return { kind: 'blocked', reason: 'sync-binding-changed' }
            const binding = await native.state()
            if (binding.target.kind !== 'external' || binding.target.connectionId !== connectionId || binding.selectionEpoch !== selectionEpoch) return { kind: 'blocked', reason: 'sync-binding-changed' }
            try {
                await invoke('external_lww_publish', { request: { ...header(connectionId, { state: binding, signal }), exitTarget: { revision: String(target.revision), libraryEpoch: target.libraryEpoch, selectionEpoch } } })
                signal.throwIfAborted()
                if (!fenced) await adapter.scheduler.receiveNow(true)
                return { kind: 'complete' }
            } catch (error) { return { kind: 'blocked', reason: (error as { kind?: string })?.kind ?? 'sync-unavailable' } }
        },
        async cancel() { adapter.scheduler.stop(); await invoke('external_lww_fence', { connectionId, newDevice: false }); await adapter.scheduler.settled() },
        async resumeAfterExitCancel() { await invoke('external_lww_resume', { connectionId }); adapter.scheduler.start() },
    }
}
