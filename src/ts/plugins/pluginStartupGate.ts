import { get, writable } from 'svelte/store'
import { getDeviceMarkers } from '../storage/deviceMarkers'
import { subscribeSyncBindingChanges, subscribeSyncReceiveCompleted } from '../storage/sync/bindingChanges'
import { isTauri } from '../platform'
import { isSyncBindingReplacementPending } from '../storage/sync/bindingRegistry'

export const PLUGIN_SYNC_SETTING = 'risuNestPluginSyncSettings'
export function delayPluginLoadingUntilSync(): boolean {
    try {
        const value: unknown = JSON.parse(getDeviceMarkers().getItem(PLUGIN_SYNC_SETTING) ?? 'null')
        return !(typeof value === 'object' && value !== null && 'delayPluginLoadingUntilSync' in value && value.delayPluginLoadingUntilSync === false)
    } catch { return true }
}

export interface PluginStartupContext {
    identity: string
    target?: 'server' | 'external'
    bound: boolean
    heldByRestore?: boolean
    replacementPending?: boolean
    receive(signal: AbortSignal): Promise<void>
    assertCurrent(): Promise<void>
}
export interface PluginStartupState {
    phase: 'released' | 'waiting' | 'failed'
    error?: unknown
    target?: 'server' | 'external'
    since: number
}

export function createPluginStartupGate(dependencies: {
    enabled(): boolean
    delay(): boolean
    syncExcluded?(): boolean
    replacementPending?(): boolean
    context(): Promise<PluginStartupContext>
    start(): Promise<unknown>
}) {
    const state = writable<PluginStartupState>({ phase: 'released', since: 0 })
    let attempt = 0
    let refusals = 0
    let controller: AbortController | undefined
    let ready: string | undefined
    let once = false
    let checking: Promise<boolean> | undefined
    let installingDefaults = false
    let bindingChanged = false
    const release = () => {
        installingDefaults = false
        if (get(state).phase !== 'released') state.set({ phase: 'released', since: 0 })
    }
    const start = () => { void dependencies.start().catch(error => console.error('Plugin startup failed', error)) }
    const ensure = (options: { installingDefaults?: boolean } = {}): Promise<boolean> => {
        if (options.installingDefaults) installingDefaults = true
        const bypass = !dependencies.delay() || dependencies.syncExcluded?.() || once
        if (!dependencies.enabled() && !installingDefaults) {
            controller?.abort(); attempt++; release()
            return Promise.resolve(true)
        }
        if (dependencies.replacementPending?.()) {
            state.update(value => ({ phase: 'waiting', since: value.since || Date.now(), target: value.target }))
            return Promise.resolve(false)
        }
        if (bypass) {
            controller?.abort(); attempt++; release()
            return Promise.resolve(true)
        }
        if (checking) return checking
        if (bindingChanged) return Promise.resolve(false)
        const current = attempt
        const run = (async () => {
            let context: PluginStartupContext
            try { context = await dependencies.context() }
            catch (error) {
                if (current === attempt) {
                    bindingChanged = error instanceof Error && error.message === 'Sync binding changed'
                    const waiting = bindingChanged || dependencies.replacementPending?.() === true
                    state.set({ phase: waiting ? 'waiting' : 'failed', error: waiting ? undefined : error, since: Date.now() })
                }
                return false
            }
            if (current !== attempt) return false
            if (context.replacementPending) {
                state.set({ phase: 'waiting', since: Date.now(), target: context.target })
                return false
            }
            if (!context.bound || context.heldByRestore || ready === context.identity) { release(); return true }
            if (controller && !controller.signal.aborted) return false
            const active = controller = new AbortController()
            state.set({ phase: 'waiting', since: Date.now(), target: context.target })
            void (async () => {
                try {
                    await context.receive(active.signal)
                    await context.assertCurrent()
                    if (current !== attempt || active.signal.aborted) return
                    ready = context.identity
                    controller = undefined
                    release()
                    start()
                } catch (error) {
                    if (current !== attempt || active.signal.aborted) return
                    controller = undefined
                    bindingChanged = error instanceof Error && error.message === 'Sync binding changed'
                    const waiting = bindingChanged || dependencies.replacementPending?.() === true
                    state.update(value => ({ ...value, phase: waiting ? 'waiting' : 'failed', error: waiting ? undefined : error }))
                }
            })()
            return false
        })()
        checking = run.finally(() => { if (checking === pending) checking = undefined })
        const pending = checking
        return pending
    }
    return {
        state,
        get version() { return attempt },
        get refusalCount() { return refusals },
        ensure,
        isWaiting: () => checking !== undefined || get(state).phase !== 'released',
        refuse(): boolean {
            if (get(state).phase === 'released' || !dependencies.enabled()) return false
            refusals++
            return true
        },
        invalidate() {
            const recheck = checking !== undefined && get(state).phase === 'released'
            attempt++; controller?.abort(); controller = undefined; checking = undefined; ready = undefined
            bindingChanged = false
            // Restart consumers whose binding lookup was cancelled before showing a wait.
            if (recheck) state.update(value => ({ ...value }))
        },
        retry() { this.invalidate(); start() },
        loadOnce() { once = true; this.invalidate(); release(); start() },
        preferenceChanged() { if (this.isWaiting()) { this.invalidate(); start() } },
    }
}

let enabled = () => false
let start: () => Promise<unknown> = async () => {}
let syncExcludedForThisStart = false
export function setPluginStartupSyncExcluded(excluded: boolean): void {
    syncExcludedForThisStart = excluded
    pluginStartupGate.invalidate()
}
export const pluginStartupGate = createPluginStartupGate({
    enabled: () => enabled(),
    delay: () => isTauri && delayPluginLoadingUntilSync(),
    syncExcluded: () => syncExcludedForThisStart,
    replacementPending: () => isSyncBindingReplacementPending(),
    start: () => start(),
    async context() {
        const [{ createNativeSyncBindingBridge }, { getSyncBindingTransport, isSyncBindingReplacementPending }] = await Promise.all([
            import('../storage/sync/bindingNative'), import('../storage/sync/bindingRegistry'),
        ])
        const native = createNativeSyncBindingBridge()
        const state = await native.state()
        const transport = state.target.kind === 'none' ? undefined : getSyncBindingTransport(state.target)
        const identity = JSON.stringify([state.libraryId, state.targetAuthority, state.selectionEpoch, state.target])
        return {
            identity,
            target: state.target.kind === 'none' ? undefined : state.target.kind,
            bound: state.target.kind !== 'none',
            heldByRestore: transport?.heldForCommittedRestore?.() ?? false,
            replacementPending: isSyncBindingReplacementPending(state),
            async receive(signal) {
                if (!transport?.receiveAvailableChanges) throw new Error('Sync is unavailable')
                await transport.receiveAvailableChanges({ state, signal })
            },
            async assertCurrent() {
                await native.assertAuthority(state)
                const current = await native.state()
                if (identity !== JSON.stringify([current.libraryId, current.targetAuthority, current.selectionEpoch, current.target])) throw new Error('Sync binding changed')
            },
        }
    },
})
export const pluginStartupState = pluginStartupGate.state
export function configurePluginStartup(isEnabled: () => boolean, load: () => Promise<unknown>): void {
    enabled = isEnabled
    start = load
}
export async function setDelayPluginLoadingUntilSync(value: boolean): Promise<void> {
    const markers = getDeviceMarkers()
    markers.setItem(PLUGIN_SYNC_SETTING, JSON.stringify({ delayPluginLoadingUntilSync: value }))
    await markers.flush()
    pluginStartupGate.preferenceChanged()
}
subscribeSyncBindingChanges(() => {
    const waiting = pluginStartupGate.isWaiting()
    pluginStartupGate.invalidate()
    if (waiting) void start().catch(error => console.error('Plugin startup failed', error))
})

export async function retryPluginStartup(): Promise<void> {
    pluginStartupGate.invalidate()
    if (syncExcludedForThisStart) {
        await start().catch(error => console.error('Plugin startup failed', error))
        return
    }
    try {
        const { createNativeSyncBindingBridge } = await import('../storage/sync/bindingNative')
        const { target } = await createNativeSyncBindingBridge().state()
        if (target.kind === 'server') {
            const { retryServerSync } = await import('../storage/sync/serverSyncProduction')
            await retryServerSync()
        } else if (target.kind === 'external') {
            const { requestExternalLwwNow } = await import('../storage/sync/external/lwwProduction')
            await requestExternalLwwNow(target.connectionId)
        }
    } catch { /* The gate reports the next receive result through its notice. */ }
    await start().catch(error => console.error('Plugin startup failed', error))
}

subscribeSyncReceiveCompleted(() => { if (get(pluginStartupState).phase === 'failed') pluginStartupGate.retry() })
