import type { BindingTarget, SyncBindingOptions, SyncBindingState, SyncBindingTransport } from './bindingFlow'
import { createSyncBindingFlow } from './bindingFlow'
import { createNativeSyncBindingBridge } from './bindingNative'
import { notifySyncBindingChanged } from './bindingChanges'

type Flow = ReturnType<typeof createSyncBindingFlow>
let bindingFlow: Flow | undefined
const transports = new Map<string, SyncBindingTransport>()
const pendingReplacements = new Set<SyncBindingState>()
export function isSyncBindingReplacementPending(state?: SyncBindingState): boolean {
    if (!state) return pendingReplacements.size > 0
    return [...pendingReplacements].some(pending => pending.libraryId === state.libraryId &&
        pending.targetAuthority === state.targetAuthority && pending.selectionEpoch === state.selectionEpoch &&
        JSON.stringify(pending.target) === JSON.stringify(state.target))
}
const key = (target: Exclude<BindingTarget, { kind: 'none' }>) => `${target.kind}:${target.connectionId}`
export const getSyncBindingTransport = (target: Exclude<BindingTarget, { kind: 'none' }>) => transports.get(key(target))

export async function resumeCurrentSyncBinding(target: BindingTarget): Promise<void> {
    if (!bindingFlow) throw new Error('Sync binding flow is unavailable')
    try { await bindingFlow.resumeCurrent(target) }
    finally { notifySyncBindingChanged() }
}

/** A bound library cannot be replaced until its sync target delivers the changes available now. */
export class SyncBindingUnavailableError extends Error {
    readonly code = 'sync-unavailable'
    constructor(cause?: unknown) {
        super('Sync is unavailable for library replacement', cause === undefined ? undefined : { cause })
        this.name = 'SyncBindingUnavailableError'
    }
}

export async function prepareBoundLibraryReplacement(options: {
    confirmedCommittedRestore?: boolean
    control?<T>(work: Promise<T>, operation: string): Promise<T>
} = {}) {
    const native = createNativeSyncBindingBridge()
    const control = options.control ?? (<T>(work: Promise<T>) => work)
    const state = await control(native.state(), 'binding state')
    const transport = state.target.kind === 'none' ? undefined : transports.get(key(state.target))
    if (state.target.kind !== 'none' && (!transport || (!options.confirmedCommittedRestore && !transport.receiveAvailableChanges))) throw new SyncBindingUnavailableError()
    const context = { state, signal: new AbortController().signal }
    const assertUnchanged = async () => {
        await control(native.assertAuthority(state), 'binding verification')
        const current = await control(native.state(), 'binding state')
        if (current.libraryId !== state.libraryId || JSON.stringify(current.target) !== JSON.stringify(state.target)) {
            throw new Error('Sync binding changed')
        }
    }
    if (!options.confirmedCommittedRestore && transport?.receiveAvailableChanges) {
        try { await transport.receiveAvailableChanges(context) }
        catch (error) { throw new SyncBindingUnavailableError(error) }
    }
    await assertUnchanged()
    const release = () => { if (pendingReplacements.delete(state)) notifySyncBindingChanged() }
    return {
        state,
        bound: state.target.kind !== 'none',
        async fence() {
            if (transport) pendingReplacements.add(state)
            bindingFlow?.invalidateCurrentResume(state)
            try { await transport?.fenceOldJobs(context); await assertUnchanged() }
            catch (error) { pendingReplacements.delete(state); notifySyncBindingChanged(); throw error }
        },
        assertAuthority: assertUnchanged,
        release,
        resume: async (options: {holdAutomaticSync?: boolean} = {}) => {
            try { await transport?.resumeBinding({...context, ...options}) }
            finally { pendingReplacements.delete(state); notifySyncBindingChanged() }
        },
    }
}

export function registerSyncBindingFlow(flow: Flow): () => void {
    if (bindingFlow && bindingFlow !== flow) throw new Error('Sync binding flow is already registered')
    bindingFlow = flow
    return () => { if (bindingFlow === flow) bindingFlow = undefined }
}

export function registerSyncBindingTransport(target: Exclude<BindingTarget, { kind: 'none' }>, transport: SyncBindingTransport): () => void {
    const id = key(target)
    if (transports.has(id)) throw new Error('Sync binding transport is already registered')
    transports.set(id, transport)
    return () => {
        if (transports.get(id) !== transport) return
        transports.delete(id)
        for (const state of pendingReplacements) if (state.target.kind !== 'none' && key(state.target) === id) pendingReplacements.delete(state)
    }
}

export async function bindSyncTarget(target: Exclude<BindingTarget, { kind: 'none' }>, options: SyncBindingOptions = {}) {
    const transport = transports.get(key(target))
    if (!bindingFlow || !transport) throw new Error('Sync binding transport is unavailable')
    const bind = () => bindingFlow!.bind(target, transport, options)
    try { return await (transport.observeBinding ? transport.observeBinding(bind) : bind()) }
    finally { notifySyncBindingChanged() }
}

export async function unbindSyncTarget() {
    if (!bindingFlow) throw new Error('Sync binding flow is unavailable')
    try { return await bindingFlow.unbind() }
    finally { notifySyncBindingChanged() }
}
