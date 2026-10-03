import type { StorageMutationGate } from '../storageMutationGate'
import type { ActivatedLibraryRecoveryLifecycle } from '../persistentDataRuntime'

export type BindingTarget = { kind: 'server' | 'external'; connectionId: string } | { kind: 'none' }
export interface SyncBindingState {
    target: BindingTarget
    targetAuthority: string
    selectionEpoch: string
    libraryId: string | null
    progress: unknown
}
export interface InspectedSyncTarget {
    inspectionId: string
    targetId: string
    libraryId: string
    empty: boolean
    previouslyBoundLibrary: boolean
    registrationChanged?: boolean
    serverRestored?: boolean
}
export interface StagedSyncTarget {
    targetId: string
    libraryId: string
    stagingId: string
    receiveId: string
}
export type BindingMode = 'new-device' | 'fresh-writer'
export type ReplacementReason = 'server-restored'
export interface BindingContext {
    state: SyncBindingState
    signal: AbortSignal
    mode?: BindingMode
}
export interface NewDeviceBindingPreparation {
    authorizationId: string
    writerId: string
}
export interface NewDeviceBindingResult {
    revision: number
    writerId: string
    bindingAuthority: string
}
export interface SyncBindingOptions {
    mode?: 'new-device'
}
export interface SyncBindingTransport {
    receiveAvailableChanges?(context: BindingContext): Promise<void>
    inspectTarget(context: BindingContext): Promise<InspectedSyncTarget>
    pullAvailableState(target: InspectedSyncTarget, context: BindingContext): Promise<StagedSyncTarget>
    // Native activation only, called under the exclusive gate. It must not acquire a renderer gate.
    replaceFromTarget(staged: StagedSyncTarget, context: BindingContext): Promise<void>
    publishInitialSharedState(context: BindingContext): Promise<void>
    resumeBinding(context: BindingContext): Promise<void>
    fenceOldJobs(context: BindingContext): Promise<void>
    // Claims a fresh writer for a changed registration to the library this device was bound to, keeping local data.
    prepareFreshWriter?(inspected: InspectedSyncTarget, context: BindingContext): Promise<NewDeviceBindingPreparation>
    // These hooks reserve/register a writer before activation and use it only after activation.
    prepareNewDeviceBinding?(staged: StagedSyncTarget, context: BindingContext): Promise<NewDeviceBindingPreparation>
    replaceAsNewDevice?(staged: StagedSyncTarget, preparation: NewDeviceBindingPreparation, context: BindingContext): Promise<NewDeviceBindingResult>
    resumeNewDeviceBinding?(preparation: NewDeviceBindingPreparation, result: NewDeviceBindingResult, context: BindingContext): Promise<void>
}
export interface SyncBindingNative {
    state(): Promise<SyncBindingState>
    switchTarget(expected: SyncBindingState, target: BindingTarget, inspection: InspectedSyncTarget | null, requestId: string): Promise<SyncBindingState>
    assertAuthority(state: SyncBindingState): Promise<void>
}
export interface BindingPluginLifecycle {
    fenceExecution(): Promise<void>
    invalidateCaches(): Promise<void>
    restart(): Promise<void>
}
export interface BindingActivationGuard {
    complete(): void
    abortUnchanged(): Promise<void>
}
export interface BindingRecoveryRegistration {
    setLifecycle(lifecycle: ActivatedLibraryRecoveryLifecycle): void
    registerFailure(error: unknown, resume: () => Promise<void>): void
}
export interface SyncBindingDependencies {
    native: SyncBindingNative
    resolveTransport?(target: Exclude<BindingTarget, { kind: 'none' }>): SyncBindingTransport | undefined
    gate: StorageMutationGate
    plugins: BindingPluginLifecycle
    withPausedWrites<T>(operation: () => Promise<T>): Promise<T>
    hasNonDefaultData(): Promise<boolean>
    hasNonDefaultSharedData(): Promise<boolean>
    confirmReplacement(reason?: ReplacementReason): Promise<boolean>
    refreshActivatedLibrary(): Promise<void>
    beginActivatedLibraryGuard(): BindingActivationGuard
    recovery?: BindingRecoveryRegistration
}
export type BindingOutcome = { kind: 'cancelled' } | { kind: 'bound'; action: 'initialized' | 'replaced' | 'resumed' | 'new-device'; state: SyncBindingState; newDevice?: NewDeviceBindingResult }

function sameTarget(a: BindingTarget, b: BindingTarget): boolean {
    return a.kind === b.kind && (a.kind === 'none' || (b.kind !== 'none' && a.connectionId === b.connectionId))
}

function bindingContext(state: SyncBindingState, signal: AbortSignal, mode?: BindingMode): BindingContext {
    return { state, signal, ...(mode ? { mode } : {}) }
}

/** A restored server always replaces this device; a new registration to the library it was bound to keeps local data. */
export function bindingMode(explicit: SyncBindingOptions['mode'], inspected: InspectedSyncTarget): BindingMode | undefined {
    if (inspected.serverRestored === true || explicit === 'new-device') return 'new-device'
    if (inspected.previouslyBoundLibrary && inspected.registrationChanged === true) return 'fresh-writer'
    return undefined
}

const supportsNewDevice = (transport: SyncBindingTransport) => !!(transport.prepareNewDeviceBinding && transport.replaceAsNewDevice && transport.resumeNewDeviceBinding)

export function createSyncBindingFlow(dependencies: SyncBindingDependencies) {
    let running = false
    let active: { transport: SyncBindingTransport; context: BindingContext; controller: AbortController; resumed: boolean } | undefined
    const check = async (context: BindingContext) => {
        context.signal.throwIfAborted()
        await dependencies.native.assertAuthority(context.state)
        context.signal.throwIfAborted()
    }
    const adoptPersisted = async (state: SyncBindingState) => {
        if (active && dependencies.resolveTransport && (active.context.state.targetAuthority !== state.targetAuthority ||
            active.context.state.selectionEpoch !== state.selectionEpoch || active.context.state.libraryId !== state.libraryId || !sameTarget(active.context.state.target, state.target))) {
            active.controller.abort()
            await active.transport.fenceOldJobs(active.context)
            active = undefined
        }
        if (active || state.target.kind === 'none' || !dependencies.resolveTransport) return active
        const transport = dependencies.resolveTransport(state.target)
        if (!transport) throw new Error('Sync binding transport is unavailable')
        const controller = new AbortController()
        active = { transport, context: bindingContext(state, controller.signal), controller, resumed: false }
        return active
    }
    const fenceOld = async (transport: SyncBindingTransport, context: BindingContext) => {
        if (active) {
            active.controller.abort()
            await active.transport.fenceOldJobs(bindingContext(active.context.state, active.context.signal, context.mode))
        } else await transport.fenceOldJobs(context)
        await check(context)
    }
    const resumeOld = async (mode?: BindingMode) => {
        if (!active) return
        await dependencies.native.assertAuthority(active.context.state)
        const controller = new AbortController()
        active = { ...active, controller, context: bindingContext(active.context.state, controller.signal) }
        await active.transport.resumeBinding(bindingContext(active.context.state, controller.signal, mode))
        active.resumed = true
    }
    const registerFailure = (error: unknown, resume: () => Promise<void>) => {
        try { dependencies.recovery?.registerFailure(error, resume) }
        catch (registrationError) { throw new AggregateError([error, registrationError], 'Sync binding recovery registration failed') }
    }
    return {
        async resumeCurrent(expectedTarget: BindingTarget): Promise<void> {
            if (running) throw new Error('A sync binding change is already running')
            running = true
            try {
                const state = await dependencies.native.state()
                if (!sameTarget(state.target, expectedTarget)) throw new Error('Sync binding changed')
                if (active?.resumed && !active.context.signal.aborted && active.context.state.targetAuthority === state.targetAuthority &&
                    active.context.state.selectionEpoch === state.selectionEpoch && active.context.state.libraryId === state.libraryId && sameTarget(active.context.state.target, state.target)) return
                if (active) {
                    active.controller.abort()
                    await active.transport.fenceOldJobs(active.context)
                    active = undefined
                }
                const current = await adoptPersisted(state)
                if (current) {
                    try {
                        await check(current.context)
                        await current.transport.resumeBinding(current.context)
                        await check(current.context)
                        current.resumed = true
                    } catch (error) {
                        current.controller.abort()
                        try { await current.transport.fenceOldJobs(current.context) }
                        catch (fenceError) { throw new AggregateError([error, fenceError], 'Sync binding startup failed while fencing jobs') }
                        throw error
                    }
                }
            } finally { running = false }
        },
        async bind(target: Exclude<BindingTarget, { kind: 'none' }>, transport: SyncBindingTransport, options: SyncBindingOptions = {}): Promise<BindingOutcome> {
            if (running) throw new Error('A sync binding change is already running')
            target = structuredClone(target)
            const explicit = options.mode === 'new-device' ? 'new-device' : undefined
            if (explicit && !supportsNewDevice(transport)) throw new Error('New device sync binding is unavailable')
            let mode: BindingMode | undefined = explicit
            running = true
            const controller = new AbortController()
            const switchRequestId = crypto.randomUUID()
            let originalState: SyncBindingState | undefined
            let jobsFenced = false
            let pluginsFenced = false
            let activationGuard: BindingActivationGuard | undefined
            let activationAttempted = false
            let activatedContext: BindingContext | undefined
            let resumeActivated: (() => Promise<void>) | undefined
            try {
                const state = await dependencies.native.state()
                await adoptPersisted(state)
                originalState = state
                const inspectionContext = bindingContext(state, controller.signal, explicit)
                const inspected = structuredClone(await transport.inspectTarget(inspectionContext))
                await check(inspectionContext)
                mode = bindingMode(explicit, inspected)
                const newDevice = mode === 'new-device'
                if (newDevice && !supportsNewDevice(transport)) throw new Error('New device sync binding is unavailable')
                const freshWriter = mode === 'fresh-writer'
                if (freshWriter && !transport.prepareFreshWriter) throw new Error('Sync binding registration change is unavailable')
                const context = bindingContext(state, controller.signal, mode)
                const reason: ReplacementReason | undefined = inspected.serverRestored === true ? 'server-restored' : undefined
                const replace = newDevice || (!inspected.empty && !inspected.previouslyBoundLibrary)
                const switchRequired = replace || !sameTarget(state.target, target) || state.libraryId !== inspected.libraryId
                let acknowledged = false
                if (newDevice || (replace && await dependencies.hasNonDefaultData())) {
                    if (!await dependencies.confirmReplacement(reason)) return { kind: 'cancelled' }
                    acknowledged = true
                }
                const staged = replace ? structuredClone(await transport.pullAvailableState(inspected, context)) : undefined
                await check(context)
                if (staged && (staged.targetId !== inspected.targetId || staged.libraryId !== inspected.libraryId)) {
                    throw new Error('Sync target changed during binding')
                }
                jobsFenced = true
                await fenceOld(transport, context)
                const preparation = newDevice ? structuredClone(await transport.prepareNewDeviceBinding!(staged!, context)) : undefined
                if (freshWriter) await transport.prepareFreshWriter!(inspected, context)
                await check(context)
                if (replace) {
                    pluginsFenced = true
                    await dependencies.plugins.fenceExecution()
                }
                let newDeviceResult: NewDeviceBindingResult | undefined
                const activate = async () => {
                    if (preparation) {
                        newDeviceResult = await transport.replaceAsNewDevice!(staged!, preparation, context)
                        const activated = await dependencies.native.state()
                        if (newDeviceResult.writerId !== preparation.writerId || newDeviceResult.bindingAuthority !== activated.targetAuthority ||
                            !sameTarget(activated.target, target) || activated.libraryId !== staged!.libraryId) {
                            throw new Error('New device sync binding changed')
                        }
                        await check(bindingContext(activated, controller.signal, mode))
                        return activated
                    }
                    const next = !switchRequired
                        ? state : await dependencies.native.switchTarget(state, target, inspected, switchRequestId)
                    if (!sameTarget(next.target, target) || next.libraryId !== inspected.libraryId) throw new Error('Sync target changed during activation')
                    const nextContext = bindingContext(next, controller.signal, mode)
                    await check(nextContext)
                    if (staged) await transport.replaceFromTarget(staged, nextContext)
                    await check(nextContext)
                    return next
                }
                let initialStatePublished = false
                resumeActivated = async () => {
                    if (!activatedContext) throw new Error('Sync binding activation is not settled')
                    await check(activatedContext)
                    if (newDevice) {
                        if (!newDeviceResult) throw new Error('New device sync binding is not settled')
                        await transport.resumeNewDeviceBinding!(preparation!, newDeviceResult, activatedContext)
                    } else {
                        if (!staged && inspected.empty && !initialStatePublished && await dependencies.hasNonDefaultSharedData()) {
                            await check(activatedContext)
                            await transport.publishInitialSharedState(activatedContext)
                            initialStatePublished = true
                        }
                        await check(activatedContext)
                        await transport.resumeBinding(activatedContext)
                    }
                    await check(activatedContext)
                    active = { transport, context: bindingContext(activatedContext.state, controller.signal), controller, resumed: true }
                    jobsFenced = false
                }
                const result: BindingOutcome = await dependencies.withPausedWrites(async () => {
                    await check(context)
                    if (replace && !acknowledged && await dependencies.hasNonDefaultData()) {
                        if (!await dependencies.confirmReplacement(reason)) {
                            pluginsFenced = false
                            await dependencies.plugins.restart()
                            return { kind: 'cancelled' }
                        }
                        acknowledged = true
                    }
                    if (staged || switchRequired) {
                        activationGuard = dependencies.beginActivatedLibraryGuard()
                        dependencies.recovery?.setLifecycle({
                            async beforeRefresh() {
                                if (!activatedContext) {
                                    const next = await dependencies.gate.runTransition(activate)
                                    activatedContext = bindingContext(next, controller.signal, mode)
                                }
                                await check(activatedContext)
                            },
                            async afterRefresh() {
                                await check(activatedContext!)
                                if (staged) {
                                    await dependencies.plugins.invalidateCaches()
                                    await dependencies.plugins.restart()
                                }
                                await check(activatedContext!)
                                pluginsFenced = false
                            },
                        })
                    }
                    const next = await dependencies.gate.runTransition(async () => {
                        await check(context)
                        activationAttempted = true
                        return activate()
                    })
                    const nextContext = bindingContext(next, controller.signal, mode)
                    activatedContext = nextContext
                    if (activationGuard) {
                        if (staged) await dependencies.plugins.invalidateCaches()
                        await dependencies.refreshActivatedLibrary()
                        await check(nextContext)
                        if (staged) await dependencies.plugins.restart()
                        await check(nextContext)
                        activationGuard!.complete()
                        activationGuard = undefined
                        pluginsFenced = false
                    }
                    await check(nextContext)
                    return { kind: 'bound', action: newDevice ? 'new-device' : staged ? 'replaced' : inspected.empty ? 'initialized' : 'resumed', state: next, ...(newDeviceResult ? { newDevice: newDeviceResult } : {}) }
                })
                if (result.kind === 'cancelled') {
                    jobsFenced = false
                    await resumeOld(mode)
                    return result
                }
                await resumeActivated()
                return result
            } catch (error) {
                let unchanged = false
                if (!activatedContext && originalState && (jobsFenced || pluginsFenced)) {
                    try {
                        const current = await dependencies.native.state()
                        unchanged = current.targetAuthority === originalState.targetAuthority && current.selectionEpoch === originalState.selectionEpoch &&
                            sameTarget(current.target, originalState.target) && current.libraryId === originalState.libraryId
                    } catch { /* Recovery requires a verified unchanged native binding. */ }
                }
                if (unchanged) {
                    try {
                        if (activationGuard) { await activationGuard.abortUnchanged(); activationGuard = undefined }
                        if (pluginsFenced) { pluginsFenced = false; await dependencies.plugins.restart() }
                        if (jobsFenced) { jobsFenced = false; await resumeOld(mode) }
                    } catch (recoveryError) {
                        if (activationGuard && activationAttempted && resumeActivated) registerFailure(recoveryError, resumeActivated)
                        throw new AggregateError([error, recoveryError], 'Sync binding failed while resuming local state')
                    }
                } else if (activationAttempted && resumeActivated) registerFailure(error, resumeActivated)
                throw error
            } finally { running = false }
        },
        async unbind(): Promise<SyncBindingState> {
            if (running) throw new Error('A sync binding change is already running')
            running = true
            try {
                const state = await dependencies.native.state()
                await adoptPersisted(state)
                if (active) {
                    active.controller.abort()
                    await active.transport.fenceOldJobs(bindingContext(active.context.state, active.context.signal))
                }
                return await dependencies.withPausedWrites(async () => {
                    const next = await dependencies.gate.runTransition(() => dependencies.native.switchTarget(state, { kind: 'none' }, null, crypto.randomUUID()))
                    active = undefined
                    return next
                })
            } finally { running = false }
        },
    }
}
