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
import { subscribeSyncBindingChanges } from './bindingChanges'
import type { ServerConfig } from './serverSync'
import { get } from 'svelte/store'
import { selectedCharID } from 'src/ts/stores.svelte'
import { getDatabase } from '../database.svelte'
import { createRateMeter, laneDeltas, readServerSyncLanes, routinePeak, routineWork, type ServerSyncAttempt, type ServerSyncLane, type ServerSyncStage } from './serverSyncProgress'

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
            const refresh = () => refreshActivatedLibraryUnderPause(token())
            const outcome = await (binding ? during('refreshing', refresh) : refresh())
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
// One attempt runs from the first sync step until sending, receiving, asset downloads and binding
// have all stopped. Native counts are read only while a view watches it.
let attempt: ServerSyncAttempt | undefined
let attemptFailed = false
// Read when a watched attempt starts, so even an attempt that ends before its next read has a start to count from.
let baseline: Promise<ServerSyncLane[] | undefined> | undefined
let binding = false
let operations = 0
let lastSuccessAt: number | undefined
// A routine attempt that moved anything stays on screen as finished for a moment.
let finished: ServerSyncAttempt | undefined
let finishedTimer: ReturnType<typeof setTimeout> | undefined
let watchers = 0
let sampling: ReturnType<typeof setInterval> | undefined
const meter = createRateMeter()
const readLanes = () => readServerSyncLanes().catch(() => undefined)
async function sample(): Promise<void> {
    const captured = attempt
    if (!captured) return
    const first = !baseline
    const start = baseline ??= readLanes()
    // The upload a routine bar fills toward is fixed once, when the attempt begins publishing.
    const plan = captured.mode === 'routine' && captured.plannedSend === undefined && captured.stages.includes('publishing')
    const [before, lanes, pending] = await Promise.all([start, first ? start : readLanes(), plan ? controller.pendingChanges().catch(() => undefined) : undefined])
    if (captured !== attempt) return
    if (!before) { if (baseline === start) baseline = undefined; return }
    if (!lanes) return
    captured.lanes = laneDeltas(lanes, before)
    if (typeof pending === 'number') captured.plannedSend = pending + (captured.lanes.find(lane => lane.lane === 'send')?.itemsDone ?? 0)
    captured.peak = routinePeak(captured)
    meter.add(Date.now(), captured.lanes.reduce((total, lane) => total + lane.sentBytes + lane.receivedBytes, 0))
    captured.rate = meter.rate()
    changed()
}
const clearFinished = () => { clearTimeout(finishedTimer); finishedTimer = undefined; finished = undefined }
/** Shows a routine attempt that sent or received anything as finished, from one last read of its counts. */
async function finish(ended: ServerSyncAttempt, start: Promise<ServerSyncLane[] | undefined>): Promise<void> {
    const [before, lanes] = await Promise.all([start, readLanes()])
    if (!before || !lanes || attempt || !watchers) return
    ended.lanes = laneDeltas(lanes, before)
    const work = routineWork(ended)
    if (work.changes.total + work.assets.total === 0) return
    clearFinished()
    ended.endedAt = Date.now()
    finished = ended
    finishedTimer = setTimeout(() => { clearFinished(); changed() }, 1500)
    changed()
}
const watchSamples = () => {
    const wanted = watchers > 0 && !!attempt
    if (wanted && !sampling) { sampling = setInterval(() => { void sample() }, 500); void sample() }
    else if (!wanted && sampling) { clearInterval(sampling); sampling = undefined }
}
/** Shows `stage` as running until `operation` settles. Sending and receiving can run at once. */
async function during<T>(stage: ServerSyncStage, operation: () => Promise<T>): Promise<T> {
    // Connecting and downloading every asset show their steps; a routine attempt they join becomes one of them.
    const mode = binding || operations ? 'full' : 'routine'
    if (!attempt) { attempt = { mode, startedAt: Date.now(), stages: [], active: [], current: stage }; attemptFailed = false; baseline = undefined; meter.reset(); clearFinished(); watchSamples() }
    else if (mode === 'full') attempt.mode = mode
    const entered = attempt
    if (!entered.stages.includes(stage)) entered.stages.push(stage)
    entered.active.push(stage); entered.current = stage
    changed()
    try { return await operation() }
    finally { entered.active.splice(entered.active.lastIndexOf(stage), 1); if (entered === attempt) changed() }
}
const settleAttempt = () => {
    if (!attempt || scheduler.isRunning() || hydrating || binding || operations) return
    const ended = attempt, start = baseline
    if (!attemptFailed) lastSuccessAt = Date.now()
    attempt = undefined
    watchSamples()
    if (!attemptFailed && ended.mode === 'routine' && start) void finish(ended, start)
    changed()
}
/** A cancelled or failed step keeps the attempt from counting as a completed sync. */
async function attempted<T>(operation: () => Promise<T>): Promise<T> {
    try { return await operation() } catch (value) { attemptFailed = true; throw value }
}
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
    hydrating = during('assets', () => invoke('server_sync_lww_hydrate', { request: header(), selectedCharacterId: selectedCharacterId() })).then(() => {
        if (captured === context && !captured.signal.aborted && hydrationError && error === hydrationError) { error = ''; hydrationError = '' }
    }).catch(value => {
        attemptFailed = true
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
        settleAttempt()
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
        const receipt = await during('publishing', () => invoke('server_sync_lww_push', { request: header(), generating: generatingConversations.snapshot() }))
        captured.signal.throwIfAborted()
        if (captured !== context) throw new Error('Sync binding changed')
        if (!foreground && !drain) return
        if (!receipt) break
    }
    changed()
}
const scheduler = createServerSyncScheduler({
    push: () => attempted(async () => { if (context) await pushAvailable(context) }),
    pull: (completeAvailable = false) => attempted(async () => {
        const captured = context
        if (!captured) throw new Error('Sync binding is unavailable')
        let receivedBodies = false
        for (;;) {
            captured.signal.throwIfAborted()
            if (captured !== context) throw new Error('Sync binding changed')
            const request = await during('downloading', () => invoke<LwwStageReceive>('server_sync_lww_pull', { request: header() }))
            captured.signal.throwIfAborted()
            try { await during('applying', () => applyPersistentLwwReceive(request)) }
            catch (value) { throw localApplyFailure(value) }
            checkContext(captured)
            if (receivedBodyReferences(request.changes)) { receivedBodies = true; hydrationPending = true }
            if (!foreground && !completeAvailable) break
            await invoke('server_sync_lww_ack', { request: { bindingAuthority: request.bindingAuthority, requestId: request.requestId } })
            checkContext(captured)
            if (request.changes.length === 0) break
        }
        if (receivedBodies) continueHydration()
    }),
    retryClock: () => retryNativeClock(true),
    publishHidden: () => attempted(async () => {
        const captured = context
        if (!captured || captured.signal.aborted) return
        await runWithMobileBackgroundTask('sync', async () => {
            // A failed local save is reported where it happened; what is already queued still goes out.
            await flushPendingDataLocally('server-sync-hidden').catch(() => {})
            await pushAvailable(captured, true)
        })
    }),
    connect: () => context ? invoke('server_sync_notify_start', { request: header() }) : Promise.resolve(),
    disconnect: async () => { await invoke('server_sync_notify_stop'); await invoke('server_sync_cancel') },
    failed(value) { error = serverSyncErrorCode(value) || 'server-unreachable'; changed() },
    recovered() { if (error && error !== hydrationError) { error = ''; changed() } },
    activity() { settleAttempt(); changed() },
})
// Scheduler blocks and errors belong to one binding authority and never carry over to another.
const adoptSchedulerAuthority = (c: BindingContext) => {
    if (schedulerAuthority === c.state.targetAuthority) return
    scheduler.reset(); error = ''; hydrationError = ''; schedulerAuthority = c.state.targetAuthority
}
const transport: SyncBindingTransport = {
    inspectTarget: c => during('preparing', () => invoke('server_sync_lww_inspect', { request: { bindingAuthority: c.state.targetAuthority, requestId: crypto.randomUUID() } })),
    pullAvailableState: (inspected,c) => during('downloading', () => invoke('server_sync_lww_stage_target', { inspectionId: inspected.inspectionId, request: { bindingAuthority: c.state.targetAuthority, requestId: crypto.randomUUID() } })),
    replaceFromTarget: (...args) => during('applying', () => replaceNativeSyncBinding(...args)),
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
    replaceAsNewDevice: (...args) => during('applying', () => replaceNativeSyncBindingAsNewDevice(...args)),
    prepareFreshWriter: (inspected,c) => invoke('server_sync_lww_prepare_fresh_writer', { inspectionId: inspected.inspectionId, request: { bindingAuthority: c.state.targetAuthority, requestId: crypto.randomUUID() } }),
    async resumeNewDeviceBinding(preparation,result,c) { await invoke('server_sync_lww_activate_new_device', { authorizationId: preparation.authorizationId, writerId: result.writerId, request: { bindingAuthority: result.bindingAuthority, requestId: crypto.randomUUID() } }); context = c; adoptSchedulerAuthority(c); checkContext(c); persistedBinding = c.state; error = ''; hydrationError = ''; hydrationPending = true; await scheduler.retry(); await updateForeground(document.visibilityState !== 'hidden'); scheduler.remoteHint() },
}
export async function receiveAvailableServerChanges(): Promise<void> { await scheduler.receiveAvailableChanges() }
export async function configureServerSyncConnection(config: ServerConfig): Promise<void> { await invoke('server_sync_configure', { config }); error = '' }
// Binding and its initial publication continue while the app is in the background. A failure keeps
// what native code committed visible, so a binding that stopped after its switch can be resumed.
async function bindServer(options: SyncBindingOptions = {}): Promise<BindingOutcome> {
    binding = true
    try { return await runWithMobileBackgroundTask('sync', () => bindSyncTarget({ kind: 'server', connectionId: 'server' }, options), undefined, true) }
    catch (value) {
        attemptFailed = true
        const code = serverSyncErrorCode(failures(value)[0])
        if (code !== 'cancelled') { error = code || 'server-unreachable'; changed() }
        await controller.ensureStatus().catch(() => {})
        throw value
    }
    finally { binding = false; settleAttempt() }
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
/** With `resumeBound` false, the server transport is registered for settings but a bound server stays stopped. */
export async function installServerSyncProduction({ resumeBound = true }: { resumeBound?: boolean } = {}): Promise<void> {
    if (!isTauri || disposeServer) return
    const disposers = [registerSyncBindingTransport({ kind: 'server', connectionId: 'server' }, transport)]
    disposers.push(subscribeLocalPersistentRevision((_revision,cause) => scheduler.localChange(cause === 'generation-complete')))
    // Another sync target taking over fences this one without a status read, so the server card reads it again.
    disposers.push(subscribeSyncBindingChanges(() => { void controller.ensureStatus().catch(() => {}) }))
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
    disposeServer = () => { scheduler.dispose(); for (const dispose of disposers) dispose(); context = undefined; persistedBinding = undefined; foreground = false; hydrationPending = false; hydrationAgain = false; attempt = undefined; clearFinished(); watchSamples() }
    await controller.ensureStatus()
    if (!resumeBound) return
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

const snapshot = () => ({ status: { ...status, bound: persistedBinding?.target.kind === 'server' || !!context && !context.signal.aborted }, running: scheduler.isRunning() || !!hydrating, paused: !context || context.signal.aborted || scheduler.isBlocked(), replacing, draining: false, error, bindingIncomplete, progress: attempt && { ...attempt, stages: [...attempt.stages], active: [...attempt.active] }, finished: finished && { ...finished, stages: [...finished.stages], active: [...finished.active] }, lastSuccessAt })
const controller = {
    snapshot,
    subscribe: (listener: (value: ReturnType<typeof snapshot>) => void) => { listeners.add(listener); listener(controller.snapshot()); return () => { listeners.delete(listener) } },
    /** Reads native transfer counts into `progress` until the returned stop runs. */
    watchProgress: () => {
        let watching = true
        watchers++; watchSamples()
        return () => { if (watching) { watching = false; watchers--; watchSamples() } }
    },
    /** Shows an operation started outside the scheduler, such as downloading every asset, as `stage`. */
    track: async <T>(stage: ServerSyncStage, operation: () => Promise<T>): Promise<T> => {
        operations++
        try { return await attempted(() => during(stage, operation)) } finally { operations--; settleAttempt() }
    },
    /** The changes waiting to be uploaded, or undefined while no server binding is active. */
    pendingChanges: async () => context && !context.signal.aborted ? invoke<number>('server_sync_lww_pending_count', { request: header() }) : undefined,
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
