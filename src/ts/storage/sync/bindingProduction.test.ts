import { expect, it, vi } from 'vitest'
const previousFiles = vi.hoisted(() => ({ confirm: vi.fn(), download: vi.fn() }))
vi.mock('./bindingDialog', () => ({ confirmSyncBindingReplacement: vi.fn(async () => true), confirmPreviousStorageFiles: previousFiles.confirm, downloadPreviousStorageFiles: previousFiles.download }))
vi.mock('./bindingLocalData', () => ({ hasLocalBindingData: vi.fn(async () => true), hasLocalSharedBindingData: vi.fn(async () => true) }))
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { IndexedDbPersistentDataStore } from '../indexedDbPersistentDataStore'
import { createPersistentDataRuntime, capturePersistentRoot, capturePersistentPresets, capturePersistentPluginStorage } from '../persistentDataRuntime'
import type { PersistentMutationToken } from '../saveCoordinator'
import { makeDatabase } from '../saveCoordinator.testSupport'
import { createStorageMutationGate, createInRealmStorageLockManager } from '../storageMutationGate'
import { createSyncBindingFlow, type SyncBindingDependencies, type SyncBindingState, type SyncBindingTransport } from './bindingFlow'
import { createSyncBindingRecoveryRegistration, installSyncBindingFlow } from './bindingProduction'
import { registerSyncBindingTransport, resumeCurrentSyncBinding, bindSyncTarget, unbindSyncTarget } from './bindingRegistry'

it.each(['server', 'external'] as const)('composes persisted %s startup ownership through the actual installer and registry before switch and unbind', async kind => {
    let state: SyncBindingState = { target: { kind, connectionId: 'persisted' }, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
    const oldTarget = state.target as Exclude<SyncBindingState['target'], { kind: 'none' }>
    const incoming = { kind: kind === 'server' ? 'external' as const : 'server' as const, connectionId: 'incoming' }
    const adapter = () => ({
        inspectTarget: vi.fn(async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: true })),
        pullAvailableState: vi.fn(), replaceFromTarget: vi.fn(), publishInitialSharedState: vi.fn(),
        resumeBinding: vi.fn<SyncBindingTransport['resumeBinding']>(async () => {}), fenceOldJobs: vi.fn<SyncBindingTransport['fenceOldJobs']>(async () => {}),
    })
    const old = adapter(), next = adapter()
    const releaseOld = registerSyncBindingTransport(oldTarget, old), releaseNext = registerSyncBindingTransport(incoming, next)
    const switchTarget = vi.fn<SyncBindingDependencies['native']['switchTarget']>(async (_expected, target) => {
        state = { ...state, target, targetAuthority: String(Number(state.targetAuthority) + 1) }; return state
    })
    const installed = installSyncBindingFlow({
        native: { state: async () => structuredClone(state), assertAuthority: async expected => { expect(expected.targetAuthority).toBe(state.targetAuthority) }, switchTarget },
        plugins: { fenceExecution: async () => {}, invalidateCaches: async () => {}, restart: async () => {} },
        withPausedWrites: operation => operation(), beginActivatedLibraryGuard: () => ({ complete() {}, async abortUnchanged() {} }),
        refreshActivatedLibrary: async () => {}, recovery: { setLifecycle() {}, registerFailure() {} },
    })
    try {
        await resumeCurrentSyncBinding(oldTarget)
        expect(old.resumeBinding).toHaveBeenCalledOnce()
        const signal = old.resumeBinding.mock.calls[0][0].signal
        let finish!: () => void
        old.fenceOldJobs.mockImplementationOnce(async context => { expect(context.signal).toBe(signal); expect(signal.aborted).toBe(true); await new Promise<void>(resolve => { finish = resolve }) })
        const switched = bindSyncTarget(incoming)
        for (let i = 0; i < 20; i++) await Promise.resolve()
        expect(old.fenceOldJobs).toHaveBeenCalledOnce(); expect(switchTarget).not.toHaveBeenCalled()
        finish(); await switched
        expect(next.fenceOldJobs).not.toHaveBeenCalled(); expect(next.resumeBinding).toHaveBeenCalledOnce()
        const nextSignal = next.resumeBinding.mock.calls[0][0].signal
        await unbindSyncTarget()
        expect(nextSignal.aborted).toBe(true); expect(next.fenceOldJobs).toHaveBeenCalledOnce()
        expect(state.target).toEqual({ kind: 'none' })
    } finally { installed.dispose(); releaseOld(); releaseNext() }
})

it('asks about files held only by the previous storage before switching to an empty target', async () => {
    let state: SyncBindingState = { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: 'none', libraryId: null, progress: null }
    const incoming = { kind: 'external' as const, connectionId: 'incoming' }
    const adapter = {
        inspectTarget: vi.fn(async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: true, previouslyBoundLibrary: false })),
        pullAvailableState: vi.fn(), replaceFromTarget: vi.fn(), publishInitialSharedState: vi.fn(async () => {}),
        resumeBinding: vi.fn(async () => {}), fenceOldJobs: vi.fn(async () => {}),
    }
    const release = registerSyncBindingTransport(incoming, adapter)
    const switchTarget = vi.fn<SyncBindingDependencies['native']['switchTarget']>(async (_expected, target, inspection) => {
        state = { ...state, target, libraryId: inspection?.libraryId ?? null, targetAuthority: '1' }; return state
    })
    // The question runs inside `whileAsking`, and the download it chose does not.
    let asking = 0
    previousFiles.confirm.mockImplementation(async () => { expect(asking).toBe(1); return 'download-then-connect' })
    previousFiles.download.mockImplementation(async () => { expect(asking).toBe(0) })
    const installed = installSyncBindingFlow({
        native: { state: async () => structuredClone(state), assertAuthority: async () => {}, switchTarget },
        plugins: { fenceExecution: async () => {}, invalidateCaches: async () => {}, restart: async () => {} },
        withPausedWrites: operation => operation(), beginActivatedLibraryGuard: () => ({ complete() {}, async abortUnchanged() {} }),
        refreshActivatedLibrary: async () => {}, recovery: { setLifecycle() {}, registerFailure() {} },
        whileAsking: async ask => { asking++; try { return await ask() } finally { asking-- } },
    })
    try {
        expect(await bindSyncTarget(incoming)).toMatchObject({ kind: 'bound' })
        expect(previousFiles.confirm).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ target: incoming }))
        expect(previousFiles.download).toHaveBeenCalledOnce()
        expect(previousFiles.download.mock.invocationCallOrder[0]).toBeLessThan(switchTarget.mock.invocationCallOrder[0])
    } finally { installed.dispose(); release() }
})

async function harness() {
    let database = makeDatabase()
    const events: string[] = []
    const cursor = vi.fn(async (revision: number) => { events.push(`cursor-${revision}`) })
    const store = Object.assign(new IndexedDbPersistentDataStore('binding-recovery', new IDBFactory(), IDBKeyRange), {
        commitWorkingSetChangeCursor: cursor,
    })
    await store.open()
    await store.replaceFromDatabase(database, (await store.readRoot()).revision)
    const runtime = createPersistentDataRuntime({
        store,
        state: {
            captureRoot: () => capturePersistentRoot(database),
            capturePresets: () => capturePersistentPresets(database),
            capturePluginStorage: () => capturePersistentPluginStorage(database),
            captureSelectedCharacter: () => database.characters[0] ?? null,
            captureCharacter: id => database.characters.find(character => character.chaId === id) ?? null,
            getSelectedCharacterId: () => database.characters[0]?.chaId,
            getSelectedConversationId: () => null,
            replaceDatabase: replacement => { database = replacement; events.push('projection') },
            publishCharacter: () => {}, publishConversation: () => {}, isConversationOperationActive: () => false,
        },
        prepareDatabase: async value => structuredClone(value),
        clock: { setTimeout: () => 1, clearTimeout: () => {} },
    })
    await runtime.initializeActiveWorkingSet(database)
    events.length = 0
    let token: PersistentMutationToken | undefined
    const pausedToken = () => { if (!token) throw Error('pause missing'); return token }
    let state: SyncBindingState = { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: 'old', libraryId: null, progress: null }
    const target = { kind: 'server', connectionId: 'synthetic' } as const
    let switchReceipt: { body: string; state: SyncBindingState } | undefined
    const dependencies: SyncBindingDependencies = {
        native: {
            state: async () => structuredClone(state),
            assertAuthority: async expected => {
                if (expected.targetAuthority !== state.targetAuthority || expected.selectionEpoch !== state.selectionEpoch) throw Error('stale binding')
            },
            switchTarget: async (previous, selected, inspection, requestId) => {
                const body = JSON.stringify({ previous, selected, inspection, requestId })
                if (switchReceipt) {
                    if (body !== switchReceipt.body || state.selectionEpoch !== switchReceipt.state.selectionEpoch) throw Error('replay changed')
                    return structuredClone(switchReceipt.state)
                }
                expect(previous.targetAuthority).toBe('0')
                state = { target: selected, targetAuthority: '1', selectionEpoch: 'new', libraryId: inspection!.libraryId, progress: null }
                switchReceipt = { body, state: structuredClone(state) }
                events.push('switch')
                return structuredClone(state)
            },
        },
        gate: createStorageMutationGate({ locks: createInRealmStorageLockManager() }),
        withPausedWrites: operation => runtime.withPausedPersistentWrites('binding-test', async paused => {
            token = paused
            try { return await operation() } finally { token = undefined; events.push('pause-release') }
        }),
        beginActivatedLibraryGuard: () => runtime.beginActivatedLibraryGuard(pausedToken()),
        refreshActivatedLibrary: async () => { expect((await runtime.refreshActivatedLibraryUnderPause(pausedToken())).projection).toBe('applied') },
        recovery: createSyncBindingRecoveryRegistration(() => runtime, pausedToken),
        hasNonDefaultData: async () => true, hasNonDefaultSharedData: async () => true, confirmReplacement: async () => true,
        plugins: { fenceExecution: async () => {}, invalidateCaches: async () => {}, restart: async () => { events.push('restart') } },
    }
    let replacementRevision: number | undefined
    const transport: SyncBindingTransport = {
        inspectTarget: async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: false }),
        pullAvailableState: vi.fn(async () => ({ targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'receive' })),
        replaceFromTarget: async () => {
            if (replacementRevision === undefined) {
                replacementRevision = (await store.replaceFromDatabase({ ...makeDatabase(), username: 'Activated synthetic target' }, (await store.readRoot()).revision)).revision
                events.push('activate')
            }
        },
        fenceOldJobs: async () => {}, publishInitialSharedState: vi.fn(async () => {}),
        resumeBinding: vi.fn(async () => { events.push('resume') }),
    }
    return { runtime, store, dependencies, transport, target, events, cursor, flow: createSyncBindingFlow(dependencies),
        get database() { return database }, setState: (next: SyncBindingState) => { state = next },
        changeAuthority: () => { state.targetAuthority = 'unrelated'; state.selectionEpoch = 'unrelated' } }
}

it('keeps real runtime writes fenced after partial restart, then ordinary retry restarts and resumes once', async () => {
    const h = await harness()
    let restarts = 0
    h.dependencies.plugins.restart = async () => {
        expect(h.database.username).toBe('Activated synthetic target')
        expect(() => h.runtime.markPersistentDataDirty(1)).toThrow()
        await expect(h.runtime.commitPersistentUnitIntent('plugin-init', [{ type: 'set', key: JSON.stringify(['root', 'username']), value: 'Rejected init' }])).rejects.toThrow()
        h.events.push('restart')
        if (++restarts === 1) throw Error('partial restart failed')
    }
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('partial restart failed')
    expect(() => h.runtime.markPersistentDataDirty(1)).toThrow()
    expect(h.transport.resumeBinding).not.toHaveBeenCalled()
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('applied')
    expect(() => h.runtime.markPersistentDataDirty(1)).not.toThrow()
    expect(restarts).toBe(2)
    expect(h.transport.resumeBinding).toHaveBeenCalledOnce()
    expect(h.events.lastIndexOf('restart')).toBeLessThan(h.events.indexOf('resume'))
    expect(h.events.filter(event => event === 'activate')).toHaveLength(1)
    await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toBeNull()
})

it.each(['resume', 'initial publication'])('stops sync and keeps the library writable when the %s never succeeds after a committed switch', async failing => {
    const h = await harness()
    h.transport.inspectTarget = async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: true, previouslyBoundLibrary: false })
    const fence = vi.fn<SyncBindingTransport['fenceOldJobs']>(async () => {})
    h.transport.fenceOldJobs = fence
    const unreachable = async () => { throw Error('storage unreachable') }
    if (failing === 'resume') h.transport.resumeBinding = vi.fn(unreachable)
    else h.transport.publishInitialSharedState = vi.fn(unreachable)
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('storage unreachable')
    expect(() => h.runtime.markPersistentDataDirty(1)).not.toThrow()
    await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toBeNull()
    expect(fence.mock.lastCall![0].state).toMatchObject({ targetAuthority: '1', selectionEpoch: 'new' })
    expect(fence.mock.lastCall![0].signal.aborted).toBe(true)
    h.transport.resumeBinding = vi.fn(async () => { h.events.push('resume') })
    h.dependencies.resolveTransport = () => h.transport
    await h.flow.resumeCurrent(h.target)
    expect(h.transport.resumeBinding).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ state: expect.objectContaining({ targetAuthority: '1' }) }))
    expect(h.transport.publishInitialSharedState).toHaveBeenCalledOnce()
})

it('lifts the recovery fence once the working set is refreshed, even when the adapter still cannot resume', async () => {
    const h = await harness()
    let restarts = 0
    h.dependencies.plugins.restart = async () => { if (++restarts === 1) throw Error('partial restart failed') }
    const fence = vi.fn<SyncBindingTransport['fenceOldJobs']>(async () => {})
    h.transport.fenceOldJobs = fence
    h.transport.resumeBinding = vi.fn(async () => { throw Error('storage unreachable') })
    const reportStopped = vi.fn<NonNullable<SyncBindingTransport['reportStopped']>>()
    h.transport.reportStopped = reportStopped
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('partial restart failed')
    expect(reportStopped).not.toHaveBeenCalled()
    expect(() => h.runtime.markPersistentDataDirty(1)).toThrow()
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('applied')
    expect(h.database.username).toBe('Activated synthetic target')
    expect(() => h.runtime.markPersistentDataDirty(1)).not.toThrow()
    expect(h.transport.resumeBinding).toHaveBeenCalledOnce()
    expect(fence.mock.lastCall![0].state).toMatchObject({ targetAuthority: '1' })
    expect(reportStopped).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ message: 'storage unreachable' }))
    await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toBeNull()
})

it.each([false, true])('settles lost initialized switch before projection without reset or plugin restart (shared data: %s)', async nonDefault => {
    const h = await harness()
    const username = h.database.username
    h.transport.inspectTarget = async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: true, previouslyBoundLibrary: false })
    h.dependencies.hasNonDefaultSharedData = async () => nonDefault
    const fence = vi.spyOn(h.dependencies.plugins, 'fenceExecution')
    const restart = vi.spyOn(h.dependencies.plugins, 'restart')
    const switchTarget = h.dependencies.native.switchTarget
    const bodies: string[] = []
    h.dependencies.native.switchTarget = async (...args) => {
        bodies.push(JSON.stringify(args))
        const result = await switchTarget(...args)
        if (bodies.length === 1) throw Error('initialized response lost')
        return result
    }
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('initialized response lost')
    expect(h.events).not.toContain('projection')
    expect(h.transport.publishInitialSharedState).not.toHaveBeenCalled()
    expect(h.transport.resumeBinding).not.toHaveBeenCalled()
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('applied')
    expect(bodies[1]).toBe(bodies[0])
    expect(h.database.username).toBe(username)
    expect(h.events).not.toContain('activate')
    expect(fence).not.toHaveBeenCalled()
    expect(restart).not.toHaveBeenCalled()
    expect(h.transport.publishInitialSharedState).toHaveBeenCalledTimes(nonDefault ? 1 : 0)
    expect(h.transport.resumeBinding).toHaveBeenCalledOnce()
})

it.each(['request', 'authority'])('rejects wrong initialized switch %s proof before any projection or publication', async change => {
    const h = await harness()
    h.transport.inspectTarget = async () => ({ inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: true, previouslyBoundLibrary: false })
    const switchTarget = h.dependencies.native.switchTarget
    h.dependencies.native.switchTarget = async (...args) => { await switchTarget(...args); throw Error('response lost') }
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('response lost')
    if (change === 'authority') h.changeAuthority()
    else h.dependencies.native.switchTarget = (previous, target, inspection, _requestId, initialPublication) => switchTarget(previous, target, inspection, 'different-request', initialPublication)
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('refresh-required')
    expect(h.events).not.toContain('projection')
    expect(h.transport.publishInitialSharedState).not.toHaveBeenCalled()
    expect(h.transport.resumeBinding).not.toHaveBeenCalled()
    expect(() => h.runtime.markPersistentDataDirty(1)).toThrow()
})

it.each(['switch', 'replacement'])('settles a lost %s response by exact replay before recovery projection', async phase => {
    const h = await harness()
    const original = phase === 'switch' ? h.dependencies.native.switchTarget : h.transport.replaceFromTarget
    let calls = 0
    const bodies: string[] = []
    if (phase === 'switch') h.dependencies.native.switchTarget = async (...args) => {
        bodies.push(JSON.stringify(args))
        const result = await (original as typeof h.dependencies.native.switchTarget)(...args)
        if (++calls === 1) throw Error('response lost')
        return result
    }
    else h.transport.replaceFromTarget = async (...args) => {
        bodies.push(JSON.stringify(args))
        await (original as typeof h.transport.replaceFromTarget)(...args)
        if (++calls === 1) throw Error('response lost')
    }
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('response lost')
    expect(h.events).not.toContain('projection')
    expect(h.transport.resumeBinding).not.toHaveBeenCalled()
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('applied')
    expect(bodies[1]).toBe(bodies[0])
    expect(h.transport.pullAvailableState).toHaveBeenCalledOnce()
    expect(h.transport.resumeBinding).toHaveBeenCalledOnce()
    expect(h.database.username).toBe('Activated synthetic target')
})

it('does not adopt or resume an uncertain activation after an unrelated native authority transition', async () => {
    const h = await harness()
    const switchTarget = h.dependencies.native.switchTarget
    h.dependencies.native.switchTarget = async (...args) => { await switchTarget(...args); throw Error('response lost') }
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('response lost')
    h.changeAuthority()
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('refresh-required')
    expect(h.events).not.toContain('projection')
    expect(h.transport.resumeBinding).not.toHaveBeenCalled()
    expect(() => h.runtime.markPersistentDataDirty(1)).toThrow()
})

it.each(['request', 'target'])('rejects changed %s in an uncertain switch replay before projection', async change => {
    const h = await harness()
    const switchTarget = h.dependencies.native.switchTarget
    h.dependencies.native.switchTarget = async (...args) => { await switchTarget(...args); throw Error('response lost') }
    await expect(h.flow.bind(h.target, h.transport)).rejects.toThrow('response lost')
    h.dependencies.native.switchTarget = async (...args) => {
        if (change === 'request') return switchTarget(args[0], args[1], args[2], 'changed-request', args[4])
        return { ...await switchTarget(...args), target: { kind: 'external', connectionId: 'unrelated' } }
    }
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('refresh-required')
    expect(h.events).not.toContain('projection')
    expect(h.transport.resumeBinding).not.toHaveBeenCalled()
})

it('settles explicit new-device activation with its original stage and authorization before specialized resume', async () => {
    const h = await harness()
    const replace = h.transport.replaceFromTarget
    const preparation = { authorizationId: 'native-authorization', writerId: 'reserved-writer' }
    h.transport.prepareNewDeviceBinding = vi.fn(async () => preparation)
    const bodies: string[] = []
    h.transport.replaceAsNewDevice = async (stage, receipt, context) => {
        bodies.push(JSON.stringify({ stage, receipt, state: context.state, mode: context.mode }))
        await replace(stage, context)
        h.setState({ target: h.target, targetAuthority: '1', selectionEpoch: 'new', libraryId: 'library', progress: null })
        if (bodies.length === 1) throw Error('response lost')
        return { revision: (await h.store.readRoot()).revision, writerId: 'reserved-writer', bindingAuthority: '1' }
    }
    h.transport.resumeNewDeviceBinding = vi.fn(async (receipt, result, context) => {
        expect(receipt).toEqual(preparation)
        expect(result.writerId).toBe('reserved-writer')
        expect(context.mode).toBe('new-device')
    })
    await expect(h.flow.bind(h.target, h.transport, { mode: 'new-device' })).rejects.toThrow('response lost')
    expect((await h.runtime.retryCommittedWorkingSetRefresh())?.projection).toBe('applied')
    expect(bodies[1]).toBe(bodies[0])
    expect(h.transport.prepareNewDeviceBinding).toHaveBeenCalledOnce()
    expect(h.transport.resumeNewDeviceBinding).toHaveBeenCalledOnce()
    expect(h.transport.resumeBinding).not.toHaveBeenCalled()
    expect(h.transport.publishInitialSharedState).not.toHaveBeenCalled()
})
