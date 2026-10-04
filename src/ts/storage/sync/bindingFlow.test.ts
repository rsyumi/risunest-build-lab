import { describe, expect, it, vi } from 'vitest'
import { bindingMode, createSyncBindingFlow, type InspectedSyncTarget, type SyncBindingDependencies, type SyncBindingState, type SyncBindingTransport } from './bindingFlow'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { IndexedDbPersistentDataStore } from '../indexedDbPersistentDataStore'
import { createMutationGatedPersistentDataStore } from '../mutationGatedPersistentDataStore'

function setup(options: Partial<InspectedSyncTarget> & { nonDefault?: boolean; confirm?: boolean } = {}) {
    const events: string[] = []
    let state: SyncBindingState = { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: 'old', libraryId: null, progress: null }
    const record = (name: string) => async () => { events.push(name) }
    const deps: SyncBindingDependencies = {
        native: {
            state: async () => structuredClone(state),
            assertAuthority: async expected => { if (expected.targetAuthority !== state.targetAuthority) throw Error('stale authority') },
            switchTarget: async (expected, target, inspection) => {
                if (expected.targetAuthority !== state.targetAuthority) throw Error('stale authority')
                events.push('switch'); state = { ...state, target, libraryId: inspection?.libraryId ?? state.libraryId, targetAuthority: String(Number(state.targetAuthority) + 1) }; return structuredClone(state)
            },
        },
        gate: { runTransition: async f => { events.push('gate'); return f() }, runWrite: f => f(), runKeyedWrite: (_key, f) => f() },
        plugins: { fenceExecution: record('plugin-fence'), invalidateCaches: record('invalidate'), restart: record('restart') },
        withPausedWrites: async operation => { events.push('pause-flush'); return operation() }, hasNonDefaultData: async () => options.nonDefault ?? true,
        hasNonDefaultSharedData: async () => options.nonDefault ?? true,
        confirmReplacement: async reason => { events.push(reason ? `confirm:${reason}` : 'confirm'); return options.confirm ?? true },
        refreshActivatedLibrary: record('refresh'),
        beginActivatedLibraryGuard: () => ({ complete() {}, async abortUnchanged() {} }),
    }
    const target: InspectedSyncTarget = { inspectionId: 'inspected', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: false, registrationChanged: false, serverRestored: false, ...options }
    const transport: SyncBindingTransport = {
        inspectTarget: async () => { events.push('inspect'); return target },
        pullAvailableState: async () => { events.push('stage'); return { targetId: 'target', libraryId: 'library', stagingId: 'validated', receiveId: 'receive' } },
        replaceFromTarget: record('activate-database'), publishInitialSharedState: record('publish'), resumeBinding: record('resume'), fenceOldJobs: record('jobs-fence'),
    }
    return { events, deps, transport, flow: createSyncBindingFlow(deps), changeAuthority: () => { state.targetAuthority = 'changed' }, setState: (next: SyncBindingState) => { state = next } }
}
const target = { kind: 'server', connectionId: 'connection' } as const

function newDeviceTransport(s: ReturnType<typeof setup>) {
    s.transport.prepareNewDeviceBinding = async (_staged, context) => {
        expect(context.state.targetAuthority).toBe('0')
        s.events.push('reserve-register-settle')
        return { authorizationId: 'native-authorization', writerId: 'reserved-writer' }
    }
    s.transport.replaceAsNewDevice = async (_staged, preparation, context) => {
        expect(context.state.targetAuthority).toBe('0')
        expect(preparation.authorizationId).toBe('native-authorization')
        s.events.push('activate-new-device')
        s.setState({ target, targetAuthority: '1', selectionEpoch: 'new', libraryId: 'library', progress: [] })
        return { revision: 42, writerId: 'reserved-writer', bindingAuthority: '1' }
    }
    s.transport.resumeNewDeviceBinding = async (preparation, result, context) => {
        expect(result.writerId).toBe(preparation.writerId)
        expect(context.state.targetAuthority).toBe(result.bindingAuthority)
        s.events.push('resume-new-device')
    }
}

function freshWriterTransport(s: ReturnType<typeof setup>) {
    const prepare = vi.fn<NonNullable<SyncBindingTransport['prepareFreshWriter']>>(async (inspected, context) => {
        expect(inspected.inspectionId).toBe('inspected')
        expect(context.mode).toBe('fresh-writer')
        s.events.push('fresh-writer')
        return { authorizationId: 'fresh-authorization', writerId: 'fresh-writer' }
    })
    s.transport.prepareFreshWriter = prepare
    return prepare
}

describe('binding mode', () => {
    const inspected = (options: Partial<InspectedSyncTarget>): InspectedSyncTarget => ({ inspectionId: 'i', targetId: 't', libraryId: 'l', empty: false, previouslyBoundLibrary: false, registrationChanged: false, serverRestored: false, ...options })
    it('claims a fresh writer only for a new registration to the library this device was bound to', () => {
        expect(bindingMode(undefined, inspected({ previouslyBoundLibrary: true, registrationChanged: true }))).toBe('fresh-writer')
        expect(bindingMode(undefined, inspected({ previouslyBoundLibrary: true, registrationChanged: true, empty: true }))).toBe('fresh-writer')
        expect(bindingMode(undefined, inspected({ previouslyBoundLibrary: false, registrationChanged: true }))).toBeUndefined()
    })
    it('replaces as a new device whenever the server was restored', () => {
        expect(bindingMode(undefined, inspected({ serverRestored: true, registrationChanged: true }))).toBe('new-device')
        expect(bindingMode(undefined, inspected({ serverRestored: true, previouslyBoundLibrary: true, registrationChanged: true }))).toBe('new-device')
    })
    it('keeps the same registration rebind and the explicit new-device request as before', () => {
        expect(bindingMode(undefined, inspected({ previouslyBoundLibrary: true }))).toBeUndefined()
        expect(bindingMode(undefined, inspected({ previouslyBoundLibrary: true, registrationChanged: undefined, serverRestored: undefined }))).toBeUndefined()
        expect(bindingMode('new-device', inspected({ previouslyBoundLibrary: true, registrationChanged: true }))).toBe('new-device')
        expect(bindingMode(undefined, inspected({}))).toBeUndefined()
    })
})

describe('new registration to a previously bound library', () => {
    it('claims a fresh writer without confirmation or staging, then switches back with the retained data', async () => {
        const s = setup({ previouslyBoundLibrary: true, registrationChanged: true })
        const prepare = freshWriterTransport(s)
        const fence = vi.spyOn(s.transport, 'fenceOldJobs')
        const resume = vi.spyOn(s.transport, 'resumeBinding')
        expect(await s.flow.bind(target, s.transport)).toMatchObject({ kind: 'bound', action: 'resumed' })
        expect(s.events).toEqual(['inspect', 'jobs-fence', 'fresh-writer', 'pause-flush', 'gate', 'switch', 'refresh', 'resume'])
        expect(prepare).toHaveBeenCalledOnce()
        expect(fence.mock.calls[0][0]).toHaveProperty('mode', 'fresh-writer')
        expect(resume.mock.calls[0][0].state.targetAuthority).toBe('1')
    })
    it('keeps a current binding to the same library without switching', async () => {
        const s = setup({ previouslyBoundLibrary: true, registrationChanged: true })
        s.setState({ target, targetAuthority: '9', selectionEpoch: 'same', libraryId: 'library', progress: null })
        freshWriterTransport(s)
        await s.flow.bind(target, s.transport)
        expect(s.events).toEqual(['inspect', 'jobs-fence', 'fresh-writer', 'pause-flush', 'gate', 'resume'])
        expect((await s.deps.native.state()).targetAuthority).toBe('9')
    })
    it('resumes the unchanged binding when the fresh writer cannot be claimed', async () => {
        const s = setup({ empty: true }); await s.flow.bind(target, s.transport); s.events.length = 0
        s.transport.inspectTarget = async () => ({ inspectionId: 'inspected', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: true, registrationChanged: true, serverRestored: false })
        s.transport.prepareFreshWriter = async () => { s.events.push('fresh-writer'); throw new Error('registration-used') }
        await expect(s.flow.bind(target, s.transport)).rejects.toThrow('registration-used')
        expect(s.events).toEqual(['jobs-fence', 'fresh-writer', 'resume'])
        expect((await s.deps.native.state()).targetAuthority).toBe('1')
    })
    it('requires the adapter to claim a fresh writer', async () => {
        const s = setup({ previouslyBoundLibrary: true, registrationChanged: true })
        await expect(s.flow.bind(target, s.transport)).rejects.toThrow('Sync binding registration change is unavailable')
        expect(s.events).toEqual(['inspect'])
    })
})

describe('restored server', () => {
    it('replaces as a new device and describes the restore in the confirmation', async () => {
        const s = setup({ serverRestored: true, registrationChanged: true, nonDefault: false }); newDeviceTransport(s)
        const fence = vi.spyOn(s.transport, 'fenceOldJobs')
        expect(await s.flow.bind(target, s.transport)).toMatchObject({ kind: 'bound', action: 'new-device' })
        expect(s.events).toEqual(['inspect', 'confirm:server-restored', 'stage', 'jobs-fence', 'reserve-register-settle', 'plugin-fence', 'pause-flush', 'gate', 'activate-new-device', 'invalidate', 'refresh', 'restart', 'resume-new-device'])
        expect(fence.mock.calls[0][0]).toHaveProperty('mode', 'new-device')
    })
    it('stops at the restore confirmation when it is cancelled', async () => {
        const s = setup({ serverRestored: true, confirm: false }); newDeviceTransport(s)
        expect(await s.flow.bind(target, s.transport)).toEqual({ kind: 'cancelled' })
        expect(s.events).toEqual(['inspect', 'confirm:server-restored'])
    })
    it('requires new-device support once inspection finds the restore', async () => {
        const s = setup({ serverRestored: true })
        await expect(s.flow.bind(target, s.transport)).rejects.toThrow('New device sync binding is unavailable')
        expect(s.events).toEqual(['inspect'])
    })
})

it('passes explicit new-device context mode through inspection, staging, fencing, preparation, activation and resume', async () => {
    const s = setup(); newDeviceTransport(s)
    const phases = [
        vi.spyOn(s.transport, 'inspectTarget'), vi.spyOn(s.transport, 'pullAvailableState'),
        vi.spyOn(s.transport, 'fenceOldJobs'), vi.spyOn(s.transport, 'prepareNewDeviceBinding'),
        vi.spyOn(s.transport, 'replaceAsNewDevice'), vi.spyOn(s.transport, 'resumeNewDeviceBinding'),
    ]
    await s.flow.bind(target, s.transport, { mode: 'new-device' })
    for (const phase of phases) {
        expect(phase).toHaveBeenCalledOnce()
        expect(phase.mock.calls[0].at(-1)).toHaveProperty('mode', 'new-device')
    }
})
it.each([{}, { empty: true }, { previouslyBoundLibrary: true }])('ordinary binding omits context mode in every callback for %s', async inspected => {
    const s = setup(inspected)
    const phases = [
        vi.spyOn(s.transport, 'inspectTarget'), vi.spyOn(s.transport, 'pullAvailableState'),
        vi.spyOn(s.transport, 'fenceOldJobs'), vi.spyOn(s.transport, 'replaceFromTarget'),
        vi.spyOn(s.transport, 'publishInitialSharedState'), vi.spyOn(s.transport, 'resumeBinding'),
    ]
    await s.flow.bind(target, s.transport)
    for (const phase of phases) for (const args of phase.mock.calls) expect(args.at(-1)).not.toHaveProperty('mode')
})
it('explicit new-device operation passes mode when fencing and recovering an already active ordinary binding', async () => {
    const s = setup({ empty: true }); await s.flow.bind(target, s.transport)
    const fence = vi.spyOn(s.transport, 'fenceOldJobs')
    const resume = vi.spyOn(s.transport, 'resumeBinding')
    const prepare = vi.fn<NonNullable<SyncBindingTransport['prepareNewDeviceBinding']>>(async () => { throw new Error('prepare stopped') })
    s.transport.prepareNewDeviceBinding = prepare
    s.transport.replaceAsNewDevice = vi.fn()
    s.transport.resumeNewDeviceBinding = vi.fn()
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow('prepare stopped')
    expect(fence.mock.calls[0][0]).toMatchObject({ mode: 'new-device', state: { targetAuthority: '1' } })
    expect(fence.mock.calls[0][0].signal.aborted).toBe(true)
    expect(prepare.mock.calls[0][1]).toHaveProperty('mode', 'new-device')
    expect(resume.mock.calls[0][0]).toHaveProperty('mode', 'new-device')
    expect(resume.mock.calls[0][0].signal.aborted).toBe(false)
})
it('new-device context mode does not leak into later ordinary binding or unbind', async () => {
    const s = setup(); newDeviceTransport(s)
    await s.flow.bind(target, s.transport, { mode: 'new-device' })
    s.transport.inspectTarget = async () => ({ inspectionId: 'inspected', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: true })
    const fence = vi.spyOn(s.transport, 'fenceOldJobs')
    const resume = vi.spyOn(s.transport, 'resumeBinding')
    await s.flow.bind(target, s.transport)
    await s.flow.unbind()
    expect(fence).toHaveBeenCalledTimes(2)
    for (const args of fence.mock.calls) expect(args[0]).not.toHaveProperty('mode')
    expect(resume.mock.calls[0][0]).not.toHaveProperty('mode')
})

it.each([{ empty: true }, { previouslyBoundLibrary: true }])('explicit new device stages and replaces even when %s', async inspected => {
    const s = setup({ ...inspected, nonDefault: false })
    newDeviceTransport(s)
    const result = await s.flow.bind(target, s.transport, { mode: 'new-device' })
    expect(result).toMatchObject({ kind: 'bound', action: 'new-device', newDevice: { writerId: 'reserved-writer', bindingAuthority: '1' } })
    expect(s.events).toEqual(['inspect', 'confirm', 'stage', 'jobs-fence', 'reserve-register-settle', 'plugin-fence', 'pause-flush', 'gate', 'activate-new-device', 'invalidate', 'refresh', 'restart', 'resume-new-device'])
})
it('cancelled new device consent never reserves, stages, changes writer or clears content', async () => {
    const s = setup({ confirm: false }); newDeviceTransport(s)
    expect(await s.flow.bind(target, s.transport, { mode: 'new-device' })).toEqual({ kind: 'cancelled' })
    expect(s.events).toEqual(['inspect', 'confirm'])
})
it('new device requires a complete adapter capability before inspecting', async () => {
    const s = setup()
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow('New device sync binding is unavailable')
    expect(s.events).toEqual([])
})
it('new device uses fresh registration only after pause and exclusive gate release', async () => {
    const s = setup(); newDeviceTransport(s)
    const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() }); s.deps.gate = gate
    let paused = false
    s.deps.withPausedWrites = async operation => { paused = true; try { return await operation() } finally { paused = false } }
    const prepare = s.transport.prepareNewDeviceBinding!
    s.transport.prepareNewDeviceBinding = async (...args) => { expect(paused).toBe(false); return prepare(...args) }
    const resume = s.transport.resumeNewDeviceBinding!
    s.transport.resumeNewDeviceBinding = async (...args) => { expect(paused).toBe(false); await gate.runWrite(() => resume(...args)) }
    await s.flow.bind(target, s.transport, { mode: 'new-device' })
    expect(s.events).toContain('resume-new-device')
})
it('new device rejects a returned writer different from its native preparation', async () => {
    const s = setup(); newDeviceTransport(s)
    const activate = s.transport.replaceAsNewDevice!
    s.transport.replaceAsNewDevice = async (...args) => ({ ...await activate(...args), writerId: 'different' })
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow('New device sync binding changed')
    expect(s.events).not.toContain('resume-new-device')
    expect(s.events).not.toContain('refresh')
})
it('unsettled old publication cannot enter new device activation or plugin reset', async () => {
    const s = setup(); newDeviceTransport(s)
    s.transport.prepareNewDeviceBinding = async () => { throw new Error('publication outcome unresolved') }
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow('publication outcome unresolved')
    expect(s.events).toEqual(['inspect', 'confirm', 'stage', 'jobs-fence'])
    expect((await s.deps.native.state()).targetAuthority).toBe('0')
})
it.each(['prepare', 'activate'])('resumes unchanged active binding after new device %s failure outside pause and gate', async failure => {
    const s = setup({ empty: true })
    await s.flow.bind(target, s.transport)
    s.events.length = 0
    const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() }); s.deps.gate = gate
    let paused = false
    s.deps.withPausedWrites = async operation => { paused = true; try { return await operation() } finally { paused = false } }
    s.deps.plugins.restart = async () => { expect(paused).toBe(false); await gate.runWrite(async () => { s.events.push('restart-old') }) }
    s.transport.resumeBinding = async context => { expect(paused).toBe(false); expect(context.signal.aborted).toBe(false); expect(context.state.targetAuthority).toBe('1'); await gate.runWrite(async () => { s.events.push('resume-old') }) }
    s.transport.prepareNewDeviceBinding = async () => {
        if (failure === 'prepare') throw new Error('new device failed')
        return { authorizationId: 'native', writerId: 'reserved' }
    }
    s.transport.replaceAsNewDevice = async () => { throw new Error('new device failed') }
    s.transport.resumeNewDeviceBinding = async () => { throw new Error('unexpected new device resume') }
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow('new device failed')
    expect((await s.deps.native.state()).targetAuthority).toBe('1')
    expect(s.events).toContain('resume-old')
    if (failure === 'activate') expect(s.events).toContain('restart-old')
    else expect(s.events).not.toContain('restart-old')
    expect(paused).toBe(false)
})
it('ordinary replacement switch failure restores unchanged plugins and active adapter', async () => {
    const s = setup({ empty: true }); await s.flow.bind(target, s.transport); s.events.length = 0
    s.transport.inspectTarget = async () => ({ inspectionId: 'new', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: false })
    s.deps.native.switchTarget = async () => { throw new Error('native switch failed') }
    await expect(s.flow.bind(target, s.transport)).rejects.toThrow('native switch failed')
    expect(s.events).toContain('restart')
    expect(s.events).toContain('resume')
    expect((await s.deps.native.state()).targetAuthority).toBe('1')
})
it.each(['result', 'cache', 'refresh', 'state'])('postactivation %s failure keeps stale captures fenced and never resumes or publishes', async failure => {
    const s = setup(); newDeviceTransport(s)
    let guarded = false
    let staleWrites = 0
    const complete = vi.fn(() => { guarded = false })
    const abortUnchanged = vi.fn(async () => { guarded = false })
    s.deps.beginActivatedLibraryGuard = () => { guarded = true; return { complete, abortUnchanged } }
    s.deps.withPausedWrites = async operation => { try { return await operation() } finally { if (!guarded) staleWrites++ } }
    if (failure === 'result') {
        const activate = s.transport.replaceAsNewDevice!
        s.transport.replaceAsNewDevice = async (...args) => ({ ...await activate(...args), writerId: 'different' })
    } else if (failure === 'cache') s.deps.plugins.invalidateCaches = async () => { throw new Error('cache failed') }
    else if (failure === 'refresh') s.deps.refreshActivatedLibrary = async () => { throw new Error('refresh failed') }
    else {
        const read = s.deps.native.state; let reads = 0
        s.deps.native.state = async () => { if (++reads > 1) throw Error('native state unavailable'); return read() }
    }
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow()
    if (failure !== 'state') expect((await s.deps.native.state()).targetAuthority).toBe('1')
    expect(guarded).toBe(true)
    expect(staleWrites).toBe(0)
    expect(complete).not.toHaveBeenCalled()
    expect(abortUnchanged).not.toHaveBeenCalled()
    for (const event of ['restart', 'resume', 'resume-new-device', 'publish']) expect(s.events).not.toContain(event)
})
it('only unchanged native failure aborts the activation guard before restarting old state', async () => {
    const s = setup(); newDeviceTransport(s)
    let guarded = false
    const abortUnchanged = vi.fn(async () => { guarded = false })
    s.deps.beginActivatedLibraryGuard = () => { guarded = true; return { complete: vi.fn(), abortUnchanged } }
    s.transport.replaceAsNewDevice = async () => { throw new Error('unchanged native failure') }
    s.deps.plugins.restart = async () => { expect(guarded).toBe(false); s.events.push('restart') }
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow('unchanged native failure')
    expect(abortUnchanged).toHaveBeenCalledOnce()
    expect(guarded).toBe(false)
})
it('unchanged binding with changed native revision cannot release the guard or restart old state', async () => {
    const s = setup(); newDeviceTransport(s)
    let guarded = false
    s.deps.beginActivatedLibraryGuard = () => {
        guarded = true
        return { complete() { guarded = false }, async abortUnchanged() { throw Error('native revision changed') } }
    }
    s.transport.replaceAsNewDevice = async () => { throw new Error('native failure') }
    await expect(s.flow.bind(target, s.transport, { mode: 'new-device' })).rejects.toThrow('Sync binding failed while resuming local state')
    expect(guarded).toBe(true)
    for (const event of ['restart', 'resume', 'resume-new-device', 'publish']) expect(s.events).not.toContain(event)
})
it('successful activated projection holds the guard through fresh plugin restart before publication', async () => {
    const s = setup(); newDeviceTransport(s)
    let guarded = false
    s.deps.beginActivatedLibraryGuard = () => { guarded = true; return { complete() { guarded = false }, async abortUnchanged() { throw Error('unexpected abort') } } }
    s.deps.refreshActivatedLibrary = async () => { expect(guarded).toBe(true); s.events.push('refresh') }
    s.deps.plugins.restart = async () => { expect(guarded).toBe(true); s.events.push('restart') }
    const resume = s.transport.resumeNewDeviceBinding!
    s.transport.resumeNewDeviceBinding = async (...args) => { expect(guarded).toBe(false); return resume(...args) }
    expect(await s.flow.bind(target, s.transport, { mode: 'new-device' })).toMatchObject({ kind: 'bound', action: 'new-device' })
    expect(guarded).toBe(false)
})

describe('transport-neutral first binding', () => {
    it.each(['server', 'external'] as const)('owns the resumed persisted %s signal and fences it before disconnect', async kind => {
        const s = setup()
        const persisted: SyncBindingState = { target: { kind, connectionId: 'persisted' }, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
        s.setState(persisted)
        const old = { ...s.transport, resumeBinding: vi.fn<SyncBindingTransport['resumeBinding']>(async () => {}), fenceOldJobs: vi.fn(async () => {}) }
        s.deps.resolveTransport = () => old
        await s.flow.resumeCurrent(persisted.target)
        await s.flow.resumeCurrent(persisted.target)
        expect(old.resumeBinding).toHaveBeenCalledOnce()
        const signal = old.resumeBinding.mock.calls[0][0].signal
        expect(signal.aborted).toBe(false)
        await s.flow.unbind()
        expect(signal.aborted).toBe(true)
        expect(old.fenceOldJobs).toHaveBeenCalledWith(expect.objectContaining({ state: persisted, signal }))
    })
    it('fences a partially resumed startup adapter before reporting failure and permits a fresh retry', async () => {
        const s = setup()
        const persisted: SyncBindingState = { target, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
        s.setState(persisted)
        const old = { ...s.transport, resumeBinding: vi.fn<SyncBindingTransport['resumeBinding']>().mockRejectedValueOnce(new Error('startup failure')).mockResolvedValue(undefined), fenceOldJobs: vi.fn(async () => {}) }
        s.deps.resolveTransport = () => old
        await expect(s.flow.resumeCurrent(target)).rejects.toThrow('startup failure')
        expect(old.resumeBinding.mock.calls[0][0].signal.aborted).toBe(true)
        expect(old.fenceOldJobs).toHaveBeenCalledOnce()
        await s.flow.resumeCurrent(target)
        expect(old.resumeBinding).toHaveBeenCalledTimes(2)
        expect(old.resumeBinding.mock.calls[1][0].signal.aborted).toBe(false)
    })
    it('resumes an adopted but never started persisted adapter after replacement acknowledgement is cancelled', async () => {
        const s = setup({ confirm: false })
        const persisted: SyncBindingState = { target, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
        s.setState(persisted)
        const old = { ...s.transport, resumeBinding: vi.fn(async () => {}) }
        s.deps.resolveTransport = () => old
        expect(await s.flow.bind({ kind: 'external', connectionId: 'incoming' }, s.transport)).toEqual({ kind: 'cancelled' })
        await s.flow.resumeCurrent(target)
        expect(old.resumeBinding).toHaveBeenCalledOnce()
    })
    it('refuses a persisted binding whose registered transport is unavailable before any selection change', async () => {
        const s = setup(); s.setState({ target, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null })
        s.deps.resolveTransport = () => undefined
        await expect(s.flow.resumeCurrent(target)).rejects.toThrow('transport is unavailable')
        await expect(s.flow.unbind()).rejects.toThrow('transport is unavailable')
        await expect(s.flow.bind({ kind: 'external', connectionId: 'incoming' }, s.transport)).rejects.toThrow('transport is unavailable')
        expect(s.events).toEqual([])
    })
    it('acknowledges, validates, fences and activates database before refreshing and restarting plugins', async () => {
        const s = setup()
        expect(await s.flow.bind(target, s.transport)).toMatchObject({ kind: 'bound', action: 'replaced' })
        expect(s.events).toEqual(['inspect', 'confirm', 'stage', 'jobs-fence', 'plugin-fence', 'pause-flush', 'gate', 'switch', 'activate-database', 'invalidate', 'refresh', 'restart', 'resume'])
    })
    it('cancellation leaves original authority, plugin execution and contents intact', async () => {
        const s = setup({ confirm: false })
        expect(await s.flow.bind(target, s.transport)).toEqual({ kind: 'cancelled' })
        expect(s.events).toEqual(['inspect', 'confirm'])
    })
    it('default-only first binding replaces without a dialog', async () => {
        const s = setup({ nonDefault: false }); await s.flow.bind(target, s.transport)
        expect(s.events).not.toContain('confirm'); expect(s.events).toContain('activate-database')
    })
    it('failed staging never fences or clears any values', async () => {
        const s = setup(); s.transport.pullAvailableState = async () => { throw Error('invalid stage') }
        await expect(s.flow.bind(target, s.transport)).rejects.toThrow('invalid stage')
        expect(s.events).toEqual(['inspect', 'confirm'])
    })
    it('rejects a changed staged target without switching authority', async () => {
        const s = setup(); s.transport.pullAvailableState = async () => ({ targetId: 'wrong', libraryId: 'library', stagingId: 'stage', receiveId: 'receive' })
        await expect(s.flow.bind(target, s.transport)).rejects.toThrow('Sync target changed')
        expect(s.events).not.toContain('switch')
    })
    it('empty targets initialize without resetting plugin values', async () => {
        const s = setup({ empty: true }); await s.flow.bind(target, s.transport)
        expect(s.events).toEqual(['inspect', 'jobs-fence', 'pause-flush', 'gate', 'switch', 'refresh', 'publish', 'resume'])
    })
    it('default shared state needs no transfer even with plugin-local-only data', async () => {
        const s = setup({ empty: true }); s.deps.hasNonDefaultSharedData = async () => false
        await s.flow.bind(target, s.transport)
        expect(s.events).not.toContain('publish'); expect(s.events).not.toContain('plugin-fence')
        expect(s.events.filter(event => event === 'resume')).toHaveLength(1)
    })
    it('same-library recovery resumes without staging or resetting namespaces', async () => {
        const s = setup({ previouslyBoundLibrary: true }); await s.flow.bind(target, s.transport)
        expect(s.events).toEqual(['inspect', 'jobs-fence', 'pause-flush', 'gate', 'switch', 'refresh', 'resume'])
    })
    it('same-target same-library recovery takes no activation guard or full refresh', async () => {
        const s = setup({ previouslyBoundLibrary: true })
        s.setState({ target, targetAuthority: '9', selectionEpoch: 'same', libraryId: 'library', progress: null })
        const guard = vi.spyOn(s.deps, 'beginActivatedLibraryGuard')
        await s.flow.bind(target, s.transport)
        expect(guard).not.toHaveBeenCalled()
        expect(s.events).toEqual(['inspect', 'jobs-fence', 'pause-flush', 'gate', 'resume'])
    })
    it('stale state stops before activation', async () => {
        const s = setup(); s.transport.pullAvailableState = async () => { s.changeAuthority(); return { targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'receive' } }
        await expect(s.flow.bind(target, s.transport)).rejects.toThrow('stale authority')
        expect(s.events).not.toContain('activate-database')
    })
    it('replacement failure leaves plugins fenced and does not restart against partial data', async () => {
        const s = setup(); s.transport.replaceFromTarget = async () => { throw Error('interrupted') }
        await expect(s.flow.bind(target, s.transport)).rejects.toThrow('interrupted')
        expect(s.events).toContain('plugin-fence'); expect(s.events).not.toContain('restart')
    })
    it('switching aborts old transport completions and fences the old transport', async () => {
        const s = setup({ empty: true }); let signal: AbortSignal | undefined
        s.transport.publishInitialSharedState = async context => { signal = context.signal }
        await s.flow.bind(target, s.transport)
        const oldSignal = signal
        const other = { ...s.transport, fenceOldJobs: vi.fn(s.transport.fenceOldJobs) }
        await s.flow.bind({ kind: 'external', connectionId: 'other' }, other)
        expect(oldSignal?.aborted).toBe(true)
        expect(other.fenceOldJobs).not.toHaveBeenCalled()
    })
    it.each(['server', 'external'] as const)('fences the persisted %s adapter before a cross-transport switch without a prior flow bind', async kind => {
        const s = setup({ empty: true, nonDefault: false })
        const persisted: SyncBindingState = { target: { kind, connectionId: 'persisted' }, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
        s.setState(persisted)
        const old = { ...s.transport, fenceOldJobs: vi.fn(async () => { s.events.push('persisted-fence') }) }
        Object.assign(s.deps, { resolveTransport: () => old })
        const incoming = { ...s.transport, fenceOldJobs: vi.fn(async () => { s.events.push('incoming-fence') }) }
        await s.flow.bind({ kind: kind === 'server' ? 'external' : 'server', connectionId: 'incoming' }, incoming)
        expect(old.fenceOldJobs).toHaveBeenCalledWith(expect.objectContaining({ state: persisted }))
        expect(incoming.fenceOldJobs).not.toHaveBeenCalled()
        expect(s.events.indexOf('persisted-fence')).toBeLessThan(s.events.indexOf('switch'))
    })
    it('fences the persisted adapter before disconnect without a prior flow bind', async () => {
        const s = setup()
        const persisted: SyncBindingState = { target, targetAuthority: '4', selectionEpoch: 'persisted', libraryId: 'library', progress: null }
        s.setState(persisted)
        const fence = vi.fn(async () => { s.events.push('persisted-fence') })
        Object.assign(s.deps, { resolveTransport: () => ({ ...s.transport, fenceOldJobs: fence }) })
        expect((await s.flow.unbind()).target).toEqual({ kind: 'none' })
        expect(fence).toHaveBeenCalledWith(expect.objectContaining({ state: persisted }))
        expect(s.events.indexOf('persisted-fence')).toBeLessThan(s.events.indexOf('switch'))
    })
})

import { createStorageMutationGate, createInRealmStorageLockManager } from '../storageMutationGate'
it('flushes actual gated commits before transition and refreshes after exclusive release', async () => {
    const s = setup()
    const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
    const store = createMutationGatedPersistentDataStore(
        new IndexedDbPersistentDataStore('binding-gated-flush', new IDBFactory(), IDBKeyRange), gate,
    )
    await store.open()
    let paused = false
    s.deps.gate = gate
    s.deps.withPausedWrites = async operation => {
        if (paused) throw Error('already paused')
        paused = true
        try {
            const root = await store.readRoot()
            await store.commit({ expectedRevision: root.revision, root: { ...root.value, username: 'Synthetic flush' } })
            s.events.push('gated-flush')
            return await operation()
        } finally { paused = false }
    }
    s.deps.refreshActivatedLibrary = async () => {
        expect(paused).toBe(true)
        await gate.runWrite(async () => { s.events.push('gated-refresh') })
    }
    await s.flow.bind(target, s.transport)
    expect(s.events).toContain('gated-flush')
    expect(s.events).toContain('gated-refresh')
    expect(paused).toBe(false)
    expect((await store.readRoot()).value.username).toBe('Synthetic flush')
    await gate.runWrite(async () => {})
})
it('cancellation and activation failure release the real pause and storage gate', async () => {
    for (const fail of [false, true]) {
        const s = setup({ nonDefault: false })
        const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
        s.deps.gate = gate
        let paused = false
        s.deps.withPausedWrites = async operation => {
            paused = true
            try { return await operation() } finally { paused = false }
        }
        if (fail) s.transport.replaceFromTarget = async () => { throw Error('activation failed') }
        else {
            let read = 0
            s.deps.hasNonDefaultData = async () => ++read > 1
            s.deps.confirmReplacement = async () => false
        }
        if (fail) await expect(s.flow.bind(target, s.transport)).rejects.toThrow('activation failed')
        else expect(await s.flow.bind(target, s.transport)).toEqual({ kind: 'cancelled' })
        expect(paused).toBe(false)
        await gate.runWrite(async () => {})
    }
})

it('acknowledges plugin-local writes that finish while plugin execution is draining', async () => {
    const s = setup({ nonDefault: false })
    let data = false
    s.deps.hasNonDefaultData = async () => data
    s.deps.plugins.fenceExecution = async () => { s.events.push('plugin-fence'); data = true }
    s.deps.confirmReplacement = async () => { s.events.push('confirm'); return false }
    expect(await s.flow.bind(target, s.transport)).toEqual({ kind: 'cancelled' })
    expect(s.events).toContain('restart')
    expect(s.events).not.toContain('switch')
    expect(s.events).not.toContain('activate-database')
    expect(s.events.indexOf('confirm')).toBeGreaterThan(s.events.indexOf('plugin-fence'))
})

it('drains a plugin mutation using the same gate before entering exclusive activation', async () => {
    const s = setup({ nonDefault: false })
    const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
    s.deps.gate = gate
    let release!: () => void
    const drain = new Promise<void>(resolve => { release = resolve })
    let mutation: Promise<void> | undefined
    s.transport.pullAvailableState = async () => {
        mutation = gate.runWrite(async () => { await drain; s.events.push('plugin-mutation') })
        return { targetId: 'target', libraryId: 'library', stagingId: 'validated', receiveId: 'receive' }
    }
    s.deps.plugins.fenceExecution = async () => { release(); await mutation; s.events.push('plugin-fence') }
    await s.flow.bind(target, s.transport)
    expect(s.events.indexOf('plugin-mutation')).toBeLessThan(s.events.indexOf('activate-database'))
})
it('late cancellation restarts plugins through the same gate after draining without clearing data', async () => {
    const s = setup({ nonDefault: false })
    const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
    s.deps.gate = gate
    let changed = false
    s.deps.hasNonDefaultData = async () => changed
    s.deps.plugins.fenceExecution = async () => { changed = true }
    s.deps.plugins.restart = () => gate.runWrite(async () => { s.events.push('gated-restart') })
    s.deps.confirmReplacement = async () => false
    expect(await s.flow.bind(target, s.transport)).toEqual({ kind: 'cancelled' })
    expect(s.events).toContain('gated-restart')
    expect(s.events).not.toContain('switch')
})
it('resume and initial publish callbacks may use the same renderer gate', async () => {
    for (const empty of [false,true]) {
        const s = setup({ empty, previouslyBoundLibrary: !empty })
        const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() }); s.deps.gate = gate
        s.transport.resumeBinding = () => gate.runWrite(async () => { s.events.push('gated-resume') })
        s.transport.publishInitialSharedState = () => gate.runWrite(async () => { s.events.push('gated-publish') })
        await s.flow.bind(target,s.transport)
        expect(s.events).toContain(empty ? 'gated-publish' : 'gated-resume')
    }
})

it.each([
    { empty: true, nonDefault: false, action: 'initialized', publishes: false },
    { empty: true, nonDefault: true, action: 'initialized', publishes: true },
    { empty: false, nonDefault: true, action: 'replaced', publishes: false },
    { previouslyBoundLibrary: true, action: 'resumed', publishes: false },
])('activates the ordinary adapter after pause release and delivers the first edit for %s', async options => {
    const s = setup(options)
    const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
    const store = createMutationGatedPersistentDataStore(
        new IndexedDbPersistentDataStore('binding-first-edit', new IDBFactory(), IDBKeyRange), gate,
    )
    await store.open()
    s.deps.gate = gate
    let paused = false
    let adapterActive = false
    const delivered: number[] = []
    s.deps.withPausedWrites = async operation => {
        paused = true
        try { return await operation() } finally { paused = false; s.events.push('pause-release') }
    }
    const publish = vi.fn(() => gate.runWrite(async () => {
        expect(paused).toBe(false)
        s.events.push('publish')
    }))
    const resume = vi.fn(() => gate.runWrite(async () => {
        expect(paused).toBe(false)
        expect(adapterActive).toBe(false)
        adapterActive = true
        s.events.push('resume')
    }))
    s.transport.publishInitialSharedState = publish
    s.transport.resumeBinding = resume
    expect(await s.flow.bind(target, s.transport)).toMatchObject({ action: options.action })
    expect(publish).toHaveBeenCalledTimes(options.publishes ? 1 : 0)
    expect(resume).toHaveBeenCalledOnce()
    expect(s.events.indexOf('resume')).toBeGreaterThan(s.events.indexOf('pause-release'))
    if (options.publishes) expect(s.events.indexOf('resume')).toBeGreaterThan(s.events.indexOf('publish'))
    const root = await store.readRoot()
    const commit = await store.commit({ expectedRevision: root.revision, root: { ...root.value, username: 'Synthetic first edit' } })
    if (adapterActive) delivered.push(commit.revision)
    expect(delivered).toEqual([commit.revision])
    expect((await store.readRoot()).value.username).toBe('Synthetic first edit')
})

it('does not publish or resume when authority changes as pause releases', async () => {
    const s = setup({ empty: true })
    s.deps.withPausedWrites = async operation => {
        const result = await operation()
        s.changeAuthority()
        return result
    }
    await expect(s.flow.bind(target, s.transport)).rejects.toThrow('stale authority')
    expect(s.events).not.toContain('publish')
    expect(s.events).not.toContain('resume')
})
it('does not resume if authority changes during initial publication', async () => {
    const s = setup({ empty: true })
    s.transport.publishInitialSharedState = async () => { s.events.push('publish'); s.changeAuthority() }
    await expect(s.flow.bind(target, s.transport)).rejects.toThrow('stale authority')
    expect(s.events).toContain('publish')
    expect(s.events).not.toContain('resume')
})
it('does not publish or resume after replacement refresh fails', async () => {
    const s = setup()
    s.deps.refreshActivatedLibrary = async () => { throw Error('refresh failed') }
    await expect(s.flow.bind(target, s.transport)).rejects.toThrow('refresh failed')
    expect(s.events).not.toContain('publish')
    expect(s.events).not.toContain('resume')
})

it('drains coordinator-queued plugin mutations before acquiring pause', async () => {
    const s = setup({ nonDefault: false })
    let paused = false
    const queuedSetter = Promise.resolve().then(() => { if (paused) throw Error('setter blocked behind pause') })
    s.deps.plugins.fenceExecution = async () => { await queuedSetter; expect(paused).toBe(false) }
    s.deps.withPausedWrites = async operation => { paused = true; try { return await operation() } finally { paused = false } }
    await s.flow.bind(target,s.transport)
})
