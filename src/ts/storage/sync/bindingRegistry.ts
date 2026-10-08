import type { BindingTarget, SyncBindingOptions, SyncBindingTransport } from './bindingFlow'
import { createSyncBindingFlow } from './bindingFlow'
import { createNativeSyncBindingBridge } from './bindingNative'
import { notifySyncBindingChanged } from './bindingChanges'

type Flow = ReturnType<typeof createSyncBindingFlow>
let bindingFlow: Flow | undefined
const transports = new Map<string, SyncBindingTransport>()
const key = (target: Exclude<BindingTarget, { kind: 'none' }>) => `${target.kind}:${target.connectionId}`
export const getSyncBindingTransport = (target: Exclude<BindingTarget, { kind: 'none' }>) => transports.get(key(target))

export async function resumeCurrentSyncBinding(target: BindingTarget): Promise<void> {
    if (!bindingFlow) throw new Error('Sync binding flow is unavailable')
    await bindingFlow.resumeCurrent(target)
}

/** A bound library cannot be replaced until its sync target delivers the changes available now. */
export class SyncBindingUnavailableError extends Error {
    readonly code = 'sync-unavailable'
    constructor(cause?: unknown) {
        super('Sync is unavailable for library replacement', cause === undefined ? undefined : { cause })
        this.name = 'SyncBindingUnavailableError'
    }
}

export async function prepareBoundLibraryReplacement(options: { confirmedCommittedRestore?: boolean } = {}) {
    const native = createNativeSyncBindingBridge()
    const state = await native.state()
    const transport = state.target.kind === 'none' ? undefined : transports.get(key(state.target))
    if (state.target.kind !== 'none' && (!transport || (!options.confirmedCommittedRestore && !transport.receiveAvailableChanges))) throw new SyncBindingUnavailableError()
    const context = { state, signal: new AbortController().signal }
    const assertUnchanged = async () => {
        await native.assertAuthority(state)
        const current = await native.state()
        if (current.libraryId !== state.libraryId || JSON.stringify(current.target) !== JSON.stringify(state.target)) {
            throw new Error('Sync binding changed')
        }
    }
    if (!options.confirmedCommittedRestore && transport?.receiveAvailableChanges) {
        try { await transport.receiveAvailableChanges(context) }
        catch (error) { throw new SyncBindingUnavailableError(error) }
    }
    await assertUnchanged()
    return {
        state,
        bound: state.target.kind !== 'none',
        async fence() { await transport?.fenceOldJobs(context); await assertUnchanged() },
        assertAuthority: assertUnchanged,
        resume: async () => { await transport?.resumeBinding(context) },
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
    return () => { if (transports.get(id) === transport) transports.delete(id) }
}

export async function bindSyncTarget(target: Exclude<BindingTarget, { kind: 'none' }>, options: SyncBindingOptions = {}) {
    const transport = transports.get(key(target))
    if (!bindingFlow || !transport) throw new Error('Sync binding transport is unavailable')
    try { return await bindingFlow.bind(target, transport, options) }
    finally { notifySyncBindingChanged() }
}

export async function unbindSyncTarget() {
    if (!bindingFlow) throw new Error('Sync binding flow is unavailable')
    try { return await bindingFlow.unbind() }
    finally { notifySyncBindingChanged() }
}
