import { invoke } from '@tauri-apps/api/core'
import type { SyncExitDrainAdapter } from '../syncExitCoordinator'
import type { RetainableReplacementFence } from '../retainableReplacementFence'
import { isTauri } from 'src/ts/platform'
import { createSyncBindingRecoveryRegistration, installSyncBindingFlow } from './bindingProduction'
import { withPausedPersistentWrites, beginActivatedLibraryGuard, refreshActivatedLibraryUnderPause, applyPersistentLwwReceive, flushPendingDataLocally, getPersistentDataRuntime } from '../persistentDataRuntime.svelte'
import type { PersistentMutationToken } from '../saveCoordinator'
import { RevisionConflictError, type LwwStageReceive, type LwwRemoteChange } from '../persistentDataStore'
import { runWithMobileBackgroundTask } from '../../mobileBackgroundTask'
import { fencePluginExecutionForAuthorityReplacement, invalidatePluginCachesAfterAuthorityReplacement, restartPluginsAfterAuthorityReplacement } from 'src/ts/plugins/apiV3/v3.svelte'
import { registerSyncBindingTransport, resumeCurrentSyncBinding } from './bindingRegistry'
import type { SyncBindingTransport, BindingContext, BindingOutcome, SyncBindingOptions } from './bindingFlow'
import { replaceNativeSyncBinding, replaceNativeSyncBindingAsNewDevice } from './bindingNative'
import { createServerSyncScheduler, integrityCodes, serverSyncErrorCode } from './serverSyncScheduler'
import { subscribeLocalPersistentRevision } from '../persistentRevisionEvents'
import { listen } from '@tauri-apps/api/event'
import { generatingConversations } from '../generatingConversationRegistry'
import { bindSyncTarget, unbindSyncTarget } from './bindingRegistry'
import type { ServerConfig } from './serverSync'
import { get } from 'svelte/store'
import { selectedCharID } from 'src/ts/stores.svelte'
import { getDatabase } from '../database.svelte'

const selectedCharacterId = () => getDatabase().characters[get(selectedCharID)]?.chaId ?? null

let nativeBindings: ReturnType<typeof installSyncBindingFlow> | undefined
let disposeServer: (() => void) | undefined
export function initializeNativeSyncBindings(): () => void {
    if (!isTauri) return () => {}
    if (nativeBindings) return disposeNativeSyncBindings
    let activeToken: PersistentMutationToken | undefined
    const token = () => { if (!activeToken) throw new Error('Sync binding pause is unavailable'); return activeToken }
    nativeBindings = installSyncBindingFlow({
        recovery: createSyncBindingRecoveryRegistration(getPersistentDataRuntime, token),
        withPausedWrites: operation => withPausedPersistentWrites('sync-binding', async pausedToken => {
            activeToken = pausedToken
            try { return await operation() } finally { activeToken = undefined }
        }),
        beginActivatedLibraryGuard: () => beginActivatedLibraryGuard(token()),
        refreshActivatedLibrary: async () => {
            const outcome = await refreshActivatedLibraryUnderPause(token())
            if (outcome.projection !== 'applied') throw new Error('Sync binding projection is unavailable')
        },
        plugins: { fenceExecution: fencePluginExecutionForAuthorityReplacement, invalidateCaches: invalidatePluginCachesAfterAuthorityReplacement, restart: restartPluginsAfterAuthorityReplacement },
    })
    return disposeNativeSyncBindings
}
export function disposeNativeSyncBindings(): void { disposeServer?.(); disposeServer = undefined; nativeBindings?.dispose(); nativeBindings = undefined }

type Status = { configured: boolean; libraryId?: string | null; deviceId?: string | null; writerId: string; bindingAuthority: string }
let status: Status = { configured: false, writerId: '', bindingAuthority: '0' }
type PendingBinding = { endpoint: string; libraryId: string; epoch: string; serverEmpty: boolean }
let bindingIncomplete = false
let persistedBinding: BindingContext['state'] | undefined
let context: BindingContext | undefined
let error = ''
let foreground = false
let replacing = false
let hydrating: Promise<void> | undefined
let hydrationPending = false
let hydrationAgain = false
let hydrationError = ''
let schedulerAuthority: string | undefined
const listeners = new Set<(value: ReturnType<typeof snapshot>) => void>()
const changed = () => { for (const listener of listeners) listener(snapshot()) }
const header = () => {
    if (!context || context.signal.aborted) throw new Error('Sync binding is unavailable')
    return { bindingAuthority: context.state.targetAuthority, requestId: crypto.randomUUID() }
}
const checkContext = (captured: BindingContext) => {
    captured.signal.throwIfAborted()
    if (captured !== context) throw new Error('Sync binding changed')
}
const receivedBodyReferences = (changes: LwwRemoteChange[]) => changes.some(change => {
    const key: string[] = JSON.parse(change.key)
    if (change.value.kind === 'inline') return key[0] === 'asset' || key[0] === 'inlay'
    if (change.value.kind !== 'object' || key[0] === 'messages') return false
    const descriptor = change.value.descriptor as { dependencies?: unknown[]; dependencyRoot?: unknown; relationRoot?: unknown }
    return !!descriptor.dependencies?.length || !!descriptor.dependencyRoot || !!descriptor.relationRoot
})
function continueHydration(): void {
    if (!hydrationPending || !foreground || !context || context.signal.aborted || scheduler.isBlocked()) return
    if (hydrating) { hydrationAgain = true; return }
    const captured = context
    hydrationPending = false
    hydrationAgain = false
    hydrating = invoke('server_sync_lww_hydrate', { request: header(), selectedCharacterId: selectedCharacterId() }).then(() => {
        if (captured === context && !captured.signal.aborted && hydrationError && error === hydrationError) { error = ''; hydrationError = '' }
    }).catch(value => {
        if (captured !== context || captured.signal.aborted) return
        hydrationPending = true
        if (serverSyncErrorCode(value) !== 'cancelled' && foreground && !scheduler.isBlocked()) {
            hydrationError = serverSyncErrorCode(value) || 'server-unreachable'; error = hydrationError
        }
    }).finally(() => {
        hydrating = undefined
        const again = hydrationAgain
        hydrationAgain = false
        changed()
        if (again) continueHydration()
    })
}
async function updateForeground(visible: boolean): Promise<void> {
    foreground = visible && !replacing && !!context && !context.signal.aborted
    if (!foreground && hydrating) hydrationPending = true
    await scheduler.foreground(foreground, !visible)
    continueHydration()
}
async function retryNativeClock(automatic = false): Promise<void> {
    const captured = context
    if (!captured) throw new Error('Sync binding is unavailable')
    checkContext(captured)
    const request = header()
    try { await invoke('server_sync_lww_retry', { request }) }
    catch (value) { if (!automatic) scheduler.reportFailure(value); throw value }
    checkContext(captured)
    if (!automatic || error === 'clock-skew' || error === 'server-unreachable') { error = ''; hydrationError = ''; changed() }
}
const failures = (value: unknown): unknown[] => value instanceof AggregateError ? value.errors : [value]
// Classifies a failed local apply as native code does for its own store failures: an invariant
// failure stops sync until retry, a busy or unavailable store is tried again.
const localApplyFailure = (value: unknown): unknown => {
    const code = serverSyncErrorCode(value)
    if (code === 'cancelled' || integrityCodes.includes(code) || (value instanceof Error && value.name === 'AbortError')) return value
    const local = value instanceof RevisionConflictError ? 'local-revision-changed'
        : code === 'commit-busy' || (value instanceof Error && ['PersistentMutationFencedError', 'SelectedConversationTransitionInProgressError'].includes(value.name)) ? 'library-operation-busy'
            : code === 'store-error' ? 'local-storage' : 'local-validation'
    return Object.assign(new Error(local, { cause: value }), { code: local, retryable: local !== 'local-validation' })
}
const transientTransportFailure = (value: unknown) => {
    const code = serverSyncErrorCode(value)
    return !!code && !code.startsWith('local-') && typeof value === 'object' && value !== null && 'retryable' in value && value.retryable === true
}
// A revoked or replaced registration, a restored server and credentials the OS cannot open are
// resolved in settings, which a failed startup could not reach.
const settingsResolvedFailure = (value: unknown) => ['unauthorized', 'invalid-device-token', 'server-epoch-changed', 'device-credential-unavailable'].includes(serverSyncErrorCode(value))
async function resumeCurrentServerBinding(state: BindingContext['state']): Promise<void> {
    try { await resumeCurrentSyncBinding(state.target) }
    catch (value) { error = serverSyncErrorCode(failures(value)[0]) || 'server-unreachable'; await controller.ensureStatus(); throw value }
}
/** Sends queued pages until none remain; without `drain` it stops when the app leaves the foreground. */
async function pushAvailable(captured: BindingContext, drain = false): Promise<void> {
    for (;;) {
        captured.signal.throwIfAborted()
        if (captured !== context) throw new Error('Sync binding changed')
        if (!foreground && !drain) return
        const receipt = await invoke('server_sync_lww_push', { request: header(), generating: generatingConversations.snapshot() })
        captured.signal.throwIfAborted()
        if (captured !== context) throw new Error('Sync binding changed')
        if (!foreground && !drain) return
        if (!receipt) break
    }
    changed()
}
const scheduler = createServerSyncScheduler({
    async push() { if (context) await pushAvailable(context) },
    async pull(completeAvailable = false) {
        const captured = context
        if (!captured) throw new Error('Sync binding is unavailable')
        let receivedBodies = false
        for (;;) {
            captured.signal.throwIfAborted()
            if (captured !== context) throw new Error('Sync binding changed')
            const request = await invoke<LwwStageReceive>('server_sync_lww_pull', { request: header() })
            captured.signal.throwIfAborted()
            try { await applyPersistentLwwReceive(request) }
            catch (value) { throw localApplyFailure(value) }
            checkContext(captured)
            if (receivedBodyReferences(request.changes)) { receivedBodies = true; hydrationPending = true }
            if (!foreground && !completeAvailable) break
            await invoke('server_sync_lww_ack', { request: { bindingAuthority: request.bindingAuthority, requestId: request.requestId } })
            checkContext(captured)
            if (request.changes.length === 0) break
        }
        if (receivedBodies) continueHydration()
    },
    retryClock: () => retryNativeClock(true),
    async publishHidden() {
        const captured = context
        if (!captured || captured.signal.aborted) return
        await runWithMobileBackgroundTask('sync', async () => {
            // A failed local save is reported where it happened; what is already queued still goes out.
            await flushPendingDataLocally('server-sync-hidden').catch(() => {})
            await pushAvailable(captured, true)
        })
    },
    connect: () => context ? invoke('server_sync_notify_start', { request: header() }) : Promise.resolve(),
    disconnect: async () => { await invoke('server_sync_notify_stop'); await invoke('server_sync_cancel') },
    failed(value) { error = serverSyncErrorCode(value) || 'server-unreachable'; changed() },
    recovered() { if (error && error !== hydrationError) { error = ''; changed() } },
})
// Scheduler blocks and errors belong to one binding authority and never carry over to another.
const adoptSchedulerAuthority = (c: BindingContext) => {
    if (schedulerAuthority === c.state.targetAuthority) return
    scheduler.reset(); error = ''; hydrationError = ''; schedulerAuthority = c.state.targetAuthority
}
const transport: SyncBindingTransport = {
    inspectTarget: c => invoke('server_sync_lww_inspect', { request: { bindingAuthority: c.state.targetAuthority, requestId: crypto.randomUUID() } }),
    pullAvailableState: (inspected,c) => invoke('server_sync_lww_stage_target', { inspectionId: inspected.inspectionId, request: { bindingAuthority: c.state.targetAuthority, requestId: crypto.randomUUID() } }),
    replaceFromTarget: replaceNativeSyncBinding,
    reportStopped(value) {
        const code = serverSyncErrorCode(failures(value)[0])
        if (code !== 'cancelled') { error = code || 'server-unreachable'; changed() }
    },
    async fenceOldJobs(c) { foreground = false; context = undefined; persistedBinding = undefined; await scheduler.fence(); await hydrating; await invoke('server_sync_lww_fence', { newDevice: c.mode === 'new-device' || c.mode === 'fresh-writer' }); hydrationPending = false; hydrationAgain = false },
    async receiveAvailableChanges(c) { c.signal.throwIfAborted(); if (!context || context.state.targetAuthority !== c.state.targetAuthority || context.state.selectionEpoch !== c.state.selectionEpoch) throw new Error('Sync binding changed'); await receiveAvailableServerChanges(); c.signal.throwIfAborted() },
    async publishInitialSharedState(c) {
        context = c
        await invoke('server_sync_lww_activate', { request: header() })
        checkContext(c)
        await pushAvailable(c, true)
    },
    async resumeBinding(c) { context = c; adoptSchedulerAuthority(c); await invoke('server_sync_lww_activate', { request: header() }); checkContext(c); persistedBinding = c.state; hydrationPending = true; await updateForeground(document.visibilityState !== 'hidden'); scheduler.remoteHint() },
    prepareNewDeviceBinding: (staged,c) => invoke('server_sync_lww_prepare_new_device', { stagingId: staged.stagingId, request: { bindingAuthority: c.state.targetAuthority, requestId: staged.receiveId } }),
    replaceAsNewDevice: replaceNativeSyncBindingAsNewDevice,
    prepareFreshWriter: (inspected,c) => invoke('server_sync_lww_prepare_fresh_writer', { inspectionId: inspected.inspectionId, request: { bindingAuthority: c.state.targetAuthority, requestId: crypto.randomUUID() } }),
    async resumeNewDeviceBinding(preparation,result,c) { await invoke('server_sync_lww_activate_new_device', { authorizationId: preparation.authorizationId, writerId: result.writerId, request: { bindingAuthority: result.bindingAuthority, requestId: crypto.randomUUID() } }); context = c; adoptSchedulerAuthority(c); checkContext(c); persistedBinding = c.state; error = ''; hydrationError = ''; hydrationPending = true; await scheduler.retry(); await updateForeground(document.visibilityState !== 'hidden'); scheduler.remoteHint() },
}
export async function receiveAvailableServerChanges(): Promise<void> { await scheduler.receiveAvailableChanges() }
export async function configureServerSyncConnection(config: ServerConfig): Promise<void> { await invoke('server_sync_configure', { config }); error = '' }
// Binding and its initial publication continue while the app is in the background. A failure keeps
// what native code committed visible, so a binding that stopped after its switch can be resumed.
async function bindServer(options: SyncBindingOptions = {}): Promise<BindingOutcome> {
    try { return await runWithMobileBackgroundTask('sync', () => bindSyncTarget({ kind: 'server', connectionId: 'server' }, options), undefined, true) }
    catch (value) {
        const code = serverSyncErrorCode(failures(value)[0])
        if (code !== 'cancelled') { error = code || 'server-unreachable'; changed() }
        await controller.ensureStatus().catch(() => {})
        throw value
    }
}
export async function connectServerSync(config: ServerConfig, newDevice = false): Promise<BindingOutcome> {
    await configureServerSyncConnection(config)
    const outcome = await bindServer(newDevice ? { mode: 'new-device' } : {})
    await controller.ensureStatus()
    return outcome
}
/** Finishes a first binding that stopped after its target switch, with the saved registration. */
export async function completeServerSyncBinding(): Promise<void> { await bindServer(); await controller.ensureStatus() }
export async function disconnectServerSync(): Promise<void> { await unbindSyncTarget(); context = undefined; await controller.ensureStatus() }
/** Stops automatic sync until the returned release runs, so an asset download is not refused as busy. */
export const holdServerSync = () => controller.beginReplacement()
export async function retryServerSync(): Promise<void> {
    await controller.ensureStatus()
    const current = persistedBinding
    if (!current || current.target.kind !== 'server') throw new Error('Sync binding is unavailable')
    if (!context || context.signal.aborted || context.state.targetAuthority !== current.targetAuthority || context.state.selectionEpoch !== current.selectionEpoch || context.state.libraryId !== current.libraryId || context.state.target.kind !== 'server' || context.state.target.connectionId !== current.target.connectionId) {
        await resumeCurrentServerBinding(current)
    }
    foreground = false; await scheduler.fence(); await hydrating; await retryNativeClock(); await scheduler.retry(); await updateForeground(document.visibilityState !== 'hidden'); changed()
}
export async function installServerSyncProduction(): Promise<void> {
    if (!isTauri || disposeServer) return
    const disposers = [registerSyncBindingTransport({ kind: 'server', connectionId: 'server' }, transport)]
    disposers.push(subscribeLocalPersistentRevision((_revision,cause) => scheduler.localChange(cause === 'generation-complete')))
    // The source is renewed on every store revision; only a different conversation counts as opened.
    // The one open at startup is covered by the startup pull.
    const conversationIdentity = () => {
        const target = getPersistentDataRuntime().captureSelectedConversationTarget()
        return target ? JSON.stringify([target.characterId, target.conversationId]) : ''
    }
    let openedConversation = conversationIdentity()
    disposers.push(getPersistentDataRuntime().subscribeActiveConversationViewportSource(source => {
        const identity = source ? conversationIdentity() : ''
        if (!identity || identity === openedConversation) return
        openedConversation = identity
        void scheduler.conversationOpened().then(continueHydration)
    }))
    disposers.push(await listen('risu-server-sync-remote-hint', () => scheduler.remoteHint()))
    disposers.push(await listen<{ connected: boolean }>('risu-server-sync-notification', event => scheduler.socket(event.payload.connected)))
    const visibility = () => { void updateForeground(document.visibilityState !== 'hidden').catch(value => { if (serverSyncErrorCode(value) === 'cancelled') return; error = serverSyncErrorCode(value) || 'server-unreachable'; changed() }) }
    document.addEventListener('visibilitychange', visibility)
    disposers.push(() => document.removeEventListener('visibilitychange', visibility))
    disposeServer = () => { scheduler.dispose(); for (const dispose of disposers) dispose(); context = undefined; persistedBinding = undefined; foreground = false; hydrationPending = false; hydrationAgain = false }
    await controller.ensureStatus()
    const current = await invoke<BindingContext['state']>('pds_lww_binding_state')
    if (current.target.kind === 'server' && status.configured) {
        try { await resumeCurrentServerBinding(current) }
        catch (value) {
            // A fenced binding that cannot reach its server keeps the local library usable until retry.
            if (!failures(value).every(failure => transientTransportFailure(failure) || settingsResolvedFailure(failure))) throw value
        }
    } else if (current.target.kind === 'server') await completeInterruptedBinding()
}
// The user already chose to connect, so a server still as empty as when it was inspected
// is bound without asking again. Anything else waits for Connect in settings.
async function completeInterruptedBinding(): Promise<void> {
    const pending = await invoke<PendingBinding | null>('server_sync_lww_pending_binding')
    if (!pending) return
    if (pending.serverEmpty) {
        try { await completeServerSyncBinding(); if (status.configured) return }
        catch { /* Connect in settings retries with the saved registration and reports the failure. */ }
    }
    bindingIncomplete = true; changed()
}

const snapshot = () => ({ status: { ...status, bound: persistedBinding?.target.kind === 'server' || !!context && !context.signal.aborted }, running: scheduler.isRunning() || !!hydrating, paused: !context || context.signal.aborted || scheduler.isBlocked(), replacing, draining: false, error, bindingIncomplete })
const controller = {
    snapshot,
    subscribe: (listener: (value: ReturnType<typeof snapshot>) => void) => { listeners.add(listener); listener(controller.snapshot()); return () => { listeners.delete(listener) } },
    assertFileOperationAvailable: () => { if (replacing) throw new Error('server-sync-busy') },
    beginReplacement: async () => {
        replacing = true; foreground = false; if (hydrating) hydrationPending = true
        const release = async () => { replacing = false; await updateForeground(document.visibilityState !== 'hidden'); changed() }
        try { await scheduler.fence(); await hydrating } catch (value) { await release().catch(() => {}); throw value }
        changed(); return release
    },
    confirmReplacement: async () => {},
    holdAutomaticSync: () => { foreground = false; if (hydrating) hydrationPending = true; void scheduler.fence() },
    ensureStatus: async () => {
        if (isTauri) { [status,persistedBinding] = await Promise.all([invoke<Status>('server_sync_status'),invoke<BindingContext['state']>('pds_lww_binding_state')]) }
        if (status.configured || persistedBinding?.target.kind !== 'server') bindingIncomplete = false
        changed()
    },
}
export function getServerSyncController() { return controller }
export function createServerSyncExitDrainAdapter(id = 'server', _fence?: RetainableReplacementFence): SyncExitDrainAdapter {
    return { id, async drain(target, signal) {
        const cancel = () => { void invoke('server_sync_cancel') }
        signal.addEventListener('abort', cancel, { once: true })
        try {
            signal.throwIfAborted()
            if (!context || context.state.selectionEpoch !== target.selectionEpoch) return { kind: 'blocked', reason: 'binding-authority-changed' }
            foreground = false; if (hydrating) hydrationPending = true
            await scheduler.fence()
            await hydrating
            await invoke('server_sync_lww_drain', { request: header(), generating: generatingConversations.snapshot() })
            signal.throwIfAborted()
            return { kind: 'complete' }
        } catch (value) { return { kind: 'blocked', reason: typeof value === 'object' && value && 'code' in value ? String(value.code) : 'server-unreachable' } }
        finally { signal.removeEventListener('abort', cancel) }
    }, cancel: async () => { foreground = false; if (hydrating) hydrationPending = true; await scheduler.fence(); await hydrating }, resumeAfterExitCancel: async () => { await updateForeground(document.visibilityState !== 'hidden') } }
}
export function holdServerSyncAfterRestore(): void { controller.holdAutomaticSync() }
export function resumeServerSyncAfterBackup(): void { if (context) void updateForeground(document.visibilityState !== 'hidden') }
export interface ServerSyncCacheUsage {
  /** cacheBytes plus ledgerBytes. */
  totalBytes: number;
  /** The temporary files: protectedBytes plus reclaimableBytes. */
  cacheBytes: number;
  protectedBytes: number;
  reclaimableBytes: number;
  /** The asset residency ledger kept beside the temporary files. */
  ledgerBytes: number;
  /** Part of cacheBytes: what the cache's object database occupies on disk. */
  databaseBytes: number;
  blockedReason: string | null;
}

export const getServerSyncCacheUsage = () => invoke<ServerSyncCacheUsage>('server_sync_cache_usage')
export const cleanupServerSyncCache = () => invoke<ServerSyncCacheUsage>('server_sync_cache_cleanup')
