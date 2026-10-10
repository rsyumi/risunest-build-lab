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
import { createLwwScheduler, type LwwCall, type LwwOperation, type LwwTurn } from './lwwScheduler'
import type { ExternalStorageState } from './types'
import { beginExternalProgress, clearExternalProgress, clearExternalSyncProgress, externalProgressFailure, type ExternalProgressRun } from './progress'

interface Adapter { transport: SyncBindingTransport; scheduler: ReturnType<typeof createLwwScheduler>; dispose(): void; state?: SyncBindingState; generation: number; foreground?: Promise<void>; recovery?: string }
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
const foregroundAvailable = () => document.visibilityState !== 'hidden' && navigator.onLine
// A start that left sync off registers the transports, so settings can still change the sync target,
// but leaves the bound target stopped until the user starts it.
let resumeBoundTarget = true
let refreshGeneration = 0

function createAdapter(connectionId: string): Adapter {
    let active: BindingContext | undefined
    let bindingProgress: ExternalProgressRun | undefined
    const operationFailures = new Map<LwwOperation, unknown>()
    const reportOperation = (operation: LwwOperation, error?: unknown) => {
        if (error === undefined) operationFailures.delete(operation)
        else operationFailures.set(operation, error)
        reportFailure(connectionId, [...operationFailures.values()].at(-1))
    }
    const context = async (signal?: AbortSignal): Promise<BindingContext> => {
        const state = await native.state()
        if (state.target.kind !== 'external' || state.target.connectionId !== connectionId) throw new Error('Sync binding changed')
        return { state, signal: signal ? AbortSignal.any([signal, ...(active ? [active.signal] : [])]) : active?.signal ?? new AbortController().signal }
    }
    // Native code returns one bounded page per call until none remain.
    const receive = async (binding: BindingContext, turnLimit?: number): Promise<LwwTurn> => {
        const progress = beginExternalProgress(connectionId, 'sync')
        let received = 0
        try {
            for (;;) {
                binding.signal.throwIfAborted()
                const request = await progress.invoke<LwwStageReceive | null>('external_lww_receive', { request: header(connectionId, binding) }, binding.state.targetAuthority)
                binding.signal.throwIfAborted()
                if (!request) { progress.finish('complete', received > 0); return { count: received, more: false } }
                binding.signal.throwIfAborted()
                progress.stage('applying')
                await getPersistentDataRuntime().applyLwwReceive(request)
                received++
                binding.signal.throwIfAborted()
                if (turnLimit !== undefined && received >= turnLimit) { progress.finish('complete', true); return { count: received, more: true } }
            }
        } catch (error) { progress.finish(externalProgressFailure(error)); throw error }
    }
    const publish = async (binding?: BindingContext, flush = true, call?: LwwCall): Promise<LwwTurn> => {
        const current = binding ?? await context(call?.signal)
        current.signal.throwIfAborted()
        const progress = (binding && bindingProgress) || beginExternalProgress(connectionId, 'sync')
        const owned = progress !== bindingProgress
        let result: { segments: string; more: boolean } = { segments: '0', more: false }
        let meaningful = false
        try {
            progress.stage('preparing', current.state.targetAuthority)
            try {
                if (flush) await flushPendingDataLocally('external-lww-publish')
                current.signal.throwIfAborted()
                result = await progress.invoke<{ segments: string; more: boolean }>('external_lww_publish', { request: { ...header(connectionId, current), ...(call?.turnLimit !== undefined ? { turnLimit: call.turnLimit } : {}) } }, current.state.targetAuthority)
                meaningful = Number(result.segments) > 0
            } finally {
                progress.stage('finalizing')
                current.signal.throwIfAborted()
                const runtime = getPersistentDataRuntime()
                await runtime.runStorageOnlyMutation(async () => {
                    current.signal.throwIfAborted()
                    const page = await runtime.store.lwwReadOutbox({ bindingAuthority: current.state.targetAuthority, requestId: crypto.randomUUID(), limit: '1' })
                    return page.revision
                })
            }
            if (owned) progress.finish('complete', meaningful)
        } catch (error) { if (owned) progress.finish(externalProgressFailure(error)); throw error }
        return { count: Number(result.segments), more: result.more }
    }
    const scheduler = createLwwScheduler({
        available: () => !!active && !active.signal.aborted && foregroundAvailable(),
        publish: call => publish(undefined, true, call),
        receive: async call => receive(await context(call.signal), call.turnLimit),
        maintain: async call => { const current = await context(call.signal); current.signal.throwIfAborted(); await invoke('external_lww_maintenance', { request: header(connectionId, current) }); current.signal.throwIfAborted() },
        succeeded: operation => reportOperation(operation),
        failed: (error, operation) => {
            reportOperation(operation, error)
            const kind = (error as { kind?: string })?.kind
            if (kind === 'corrupt' || kind === 'repositoryMismatch') scheduler.stop()
        },
    })
    const transport: SyncBindingTransport & { receiveAvailableChanges(context: BindingContext): Promise<void> } = {
        async observeBinding(operation) {
            if (bindingProgress) return operation()
            const progress = beginExternalProgress(connectionId, 'binding')
            bindingProgress = progress
            try {
                const result = await operation()
                progress.finish(result.kind === 'cancelled' ? 'cancelled' : 'complete')
                return result
            } catch (error) { progress.finish(externalProgressFailure(error)); throw error }
            finally { if (bindingProgress === progress) bindingProgress = undefined }
        },
        setConfirmationPending(waiting) { bindingProgress?.stage(waiting ? 'waiting' : 'checking') },
        inspectTarget: binding => bindingProgress ? bindingProgress.invoke<InspectedSyncTarget>('external_lww_inspect', { request: header(connectionId, binding) }, binding.state.targetAuthority) : invoke<InspectedSyncTarget>('external_lww_inspect', { request: header(connectionId, binding) }),
        pullAvailableState: (target, binding) => {
            const args = { request: { ...header(connectionId, binding), inspectionId: target.inspectionId, targetId: target.targetId, libraryId: target.libraryId } }
            return bindingProgress ? bindingProgress.invoke<StagedSyncTarget>('external_lww_stage_binding', args, binding.state.targetAuthority) : invoke<StagedSyncTarget>('external_lww_stage_binding', args)
        },
        async replaceFromTarget(staged, binding) { bindingProgress?.stage('applying', binding.state.targetAuthority); await replaceNativeSyncBinding(staged, binding) },
        reportStopped(error) {
            const cause = error instanceof AggregateError ? error.errors[0] : error
            if ((cause as { name?: unknown } | null)?.name !== 'AbortError') reportFailure(connectionId, cause)
        },
        publishInitialSharedState: async binding => { await publish(binding, false) },
        async resumeBinding(binding) {
            const generation = ++adapter.generation
            scheduler.stop()
            await scheduler.settled()
            binding.signal.throwIfAborted()
            await invoke('pds_lww_finish_initial_publication', { request: { bindingAuthority: binding.state.targetAuthority, requestId: crypto.randomUUID() } })
            if (generation !== adapter.generation) throw new DOMException('Binding changed', 'AbortError')
            binding.signal.throwIfAborted()
            await invoke('external_lww_resume', { connectionId })
            binding.signal.throwIfAborted()
            if (generation !== adapter.generation) throw new DOMException('Binding changed', 'AbortError')
            operationFailures.clear()
            reportFailure(connectionId)
            active = binding
            adapter.state = binding.state
            scheduler.recovered()
            if (foregroundAvailable()) scheduler.start()
        },
        async fenceOldJobs(binding) {
            adapter.generation++
            scheduler.stop()
            active = undefined
            adapter.state = undefined
            const old = binding.state.target
            if (old.kind === 'external') {
                clearExternalSyncProgress(old.connectionId)
                const adapter = adapters.get(old.connectionId)
                adapter?.scheduler.stop()
                if (adapter) { adapter.generation++; adapter.state = undefined }
                await invoke('external_lww_fence', { connectionId: old.connectionId, newDevice: binding.mode === 'new-device' })
                await adapter?.scheduler.settled()
            }
            await scheduler.settled()
        },
        prepareNewDeviceBinding: (staged, binding) => invoke<NewDeviceBindingPreparation>('external_lww_prepare_new_device', {
            request: { ...header(connectionId, binding, staged.receiveId), stagingId: staged.stagingId },
        }),
        async replaceAsNewDevice(staged, preparation, binding) { bindingProgress?.stage('applying', binding.state.targetAuthority); return replaceNativeSyncBindingAsNewDevice(staged, preparation, binding) },
        async resumeNewDeviceBinding(_preparation, _result, binding) { await transport.resumeBinding(binding) },
        async receiveAvailableChanges(binding) { await receive(binding) },
    }
    const unregister = registerSyncBindingTransport({ kind: 'external', connectionId }, transport)
    const adapter: Adapter = { transport, scheduler, generation: 0, dispose() { adapter.generation++; scheduler.stop(); active = undefined; reportFailure(connectionId); unregister(); clearExternalProgress(connectionId); if (adapters.get(connectionId) === adapter) adapters.delete(connectionId) } }
    return adapter
}

export async function refreshExternalLwwAdapters(state: ExternalStorageState): Promise<void> {
    if (!isTauri) return
    const generation = ++refreshGeneration
    const sync = new Set(state.connections.filter(connection => connection.purpose === 'sync' && ['webdav', 's3', 'google_drive', 'onedrive'].includes(connection.providerId)).map(connection => connection.id))
    for (const [id, adapter] of adapters) if (!sync.has(id)) adapter.dispose()
    for (const id of sync) if (!adapters.has(id)) adapters.set(id, createAdapter(id))
    for (const [id, adapter] of adapters) {
        const connection = state.connections.find(value => value.id === id)
        const recovery = JSON.stringify([connection?.status, connection?.lastError])
        if (adapter.recovery !== undefined && adapter.recovery !== recovery) adapter.scheduler.recovered()
        adapter.recovery = recovery
    }
    const binding = await native.state()
    if (generation !== refreshGeneration) return
    for (const [id, adapter] of adapters) if (binding.target.kind !== 'external' || binding.target.connectionId !== id) {
        adapter.generation++; adapter.scheduler.stop(); adapter.state = undefined
    }
    if (binding.target.kind === 'external' && resumeBoundTarget) {
        const adapter = adapters.get(binding.target.connectionId)
        if (adapter && adapter.state?.targetAuthority !== binding.targetAuthority) await resumeCurrentSyncBinding(binding.target)
    }
}

export async function installExternalLwwAdapters(state: ExternalStorageState, resumeBound = true): Promise<() => void> {
    if (!isTauri) return () => {}
    resumeBoundTarget = resumeBound
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
        for (const adapter of adapters.values()) if (adapter.state) void adapter.scheduler.conversationOpened()
    })
    let hiddenFlushed = document.visibilityState === 'hidden'
    let wasOnline = navigator.onLine
    const foreground = () => {
        const wentOffline = !navigator.onLine && wasOnline
        wasOnline = navigator.onLine
        const flushHidden = document.visibilityState === 'hidden' && !hiddenFlushed
        hiddenFlushed = document.visibilityState === 'hidden'
        for (const adapter of adapters.values()) if (adapter.state) {
            if (document.visibilityState === 'hidden' || !navigator.onLine) {
                if (flushHidden || wentOffline) { adapter.generation++; adapter.scheduler.stop() }
                if (flushHidden && navigator.onLine) void runWithMobileBackgroundTask('sync', task => adapter.scheduler.publishNow(true, task.signal)).catch(() => {})
            }
            else if (!adapter.foreground) {
                adapter.generation++
                adapter.scheduler.pauseAutomatic()
                const generation = adapter.generation
                const authority = adapter.state.targetAuthority
                adapter.foreground = (async () => {
                    await adapter.scheduler.settled()
                    if (adapters.get(adapter.state?.target.kind === 'external' ? adapter.state.target.connectionId : '') !== adapter
                        || generation !== adapter.generation || document.visibilityState === 'hidden' || !navigator.onLine) return
                    const current = await native.state()
                    if (generation !== adapter.generation || current.targetAuthority !== authority || current.target.kind !== 'external') return
                    await invoke('external_lww_resume', { connectionId: current.target.connectionId })
                    if (generation === adapter.generation && foregroundAvailable()) await adapter.scheduler.resumeForeground()
                })().catch(() => {}).finally(() => {
                    adapter.foreground = undefined
                    if (generation !== adapter.generation && adapter.state && foregroundAvailable() && [...adapters.values()].includes(adapter)) foreground()
                })
            }
        }
    }
    document.addEventListener('visibilitychange', foreground)
    window.addEventListener('online', foreground)
    window.addEventListener('offline', foreground)
    return () => {
        disposeRevision(); disposeDevice(); disposeConversation()
        document.removeEventListener('visibilitychange', foreground)
        window.removeEventListener('online', foreground); window.removeEventListener('offline', foreground)
        refreshGeneration++
        for (const adapter of [...adapters.values()]) adapter.dispose()
        resumeBoundTarget = true
    }
}

/** Whether sync with this connection was started in this session. */
export function isExternalLwwRunning(connectionId: string): boolean {
    return !!adapters.get(connectionId)?.state
}

export async function requestExternalLwwNow(connectionId: string): Promise<void> {
    const adapter = adapters.get(connectionId)
    if (!adapter) throw new Error('Sync binding transport is unavailable')
    await runWithMobileBackgroundTask('sync', async task => {
        // A binding the start left stopped resumes before it publishes.
        if (!adapter.state) await resumeCurrentSyncBinding({ kind: 'external', connectionId })
        task.signal?.throwIfAborted()
        if (adapters.get(connectionId) !== adapter || !adapter.state) throw new DOMException('Binding changed', 'AbortError')
        await adapter.scheduler.manualNow(task.signal)
    })
}

export function externalLwwExitDrain(connectionId: string, selectionEpoch: string, fenced = false): SyncExitDrainAdapter | null {
    const adapter = adapters.get(connectionId)
    if (!adapter) return null
    return {
        id: `external:${connectionId}:${selectionEpoch}`,
        async drain(target, signal) {
            signal.throwIfAborted()
            adapter.generation++
            adapter.scheduler.stop()
            await adapter.scheduler.settled()
            signal.throwIfAborted()
            if (target.selectionId !== `external:${connectionId}:${selectionEpoch}` || target.selectionEpoch !== selectionEpoch) return { kind: 'blocked', reason: 'sync-binding-changed' }
            const binding = await native.state()
            if (binding.target.kind !== 'external' || binding.target.connectionId !== connectionId || binding.selectionEpoch !== selectionEpoch) return { kind: 'blocked', reason: 'sync-binding-changed' }
            try {
                await adapter.scheduler.publishNow(true, signal, async call => {
                    call.signal.throwIfAborted()
                    const result = await invoke<{ segments: string; more: boolean }>('external_lww_publish', { request: { ...header(connectionId, { state: binding, signal: call.signal }), exitTarget: { revision: String(target.revision), libraryEpoch: target.libraryEpoch, selectionEpoch } } })
                    call.signal.throwIfAborted()
                    return { count: Number(result.segments), more: result.more }
                })
                signal.throwIfAborted()
                if (!fenced) await adapter.scheduler.receiveNow(true, signal)
                return { kind: 'complete' }
            } catch (error) { return { kind: 'blocked', reason: (error as { kind?: string })?.kind ?? 'sync-unavailable' } }
        },
        async cancel() { adapter.generation++; adapter.scheduler.stop(); await invoke('external_lww_fence', { connectionId, newDevice: false }); await adapter.scheduler.settled() },
        async resumeAfterExitCancel() {
            const generation = ++adapter.generation
            await adapter.scheduler.settled()
            if (adapters.get(connectionId) !== adapter || !adapter.state) return
            const authority = adapter.state.targetAuthority
            const binding = await native.state()
            if (generation !== adapter.generation || binding.target.kind !== 'external' || binding.target.connectionId !== connectionId
                || binding.targetAuthority !== authority || binding.selectionEpoch !== selectionEpoch) return
            await invoke('external_lww_resume', { connectionId })
            if (generation === adapter.generation && adapters.get(connectionId) === adapter && foregroundAvailable()) adapter.scheduler.start()
        },
    }
}
