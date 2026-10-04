// @vitest-environment happy-dom

import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
const acknowledgement = vi.hoisted(() => vi.fn())
vi.mock('src/ts/alert', () => ({ alertCheckboxConfirm: acknowledgement }))
vi.mock('./serverAssetResidency', () => ({ getAssetResidencyStatus: vi.fn(), downloadRemoteAssets: vi.fn() }))
import BindingTargetSwitch from './BindingTargetSwitch.svelte'
import { createSyncBindingFlow, type SyncBindingState, type SyncBindingTransport } from './bindingFlow'
import { confirmSyncBindingReplacement } from './bindingDialog'
import * as registry from './bindingRegistry'
import { languageEnglish } from 'src/lang/en'

const target = { kind: 'server', connectionId: 'synthetic-connection' } as const
let component: ReturnType<typeof mount> | undefined
let host: HTMLDivElement
const cleanup: (() => void)[] = []

beforeEach(() => {
    acknowledgement.mockReset().mockResolvedValue({ confirmed: true, checked: true })
    host = document.createElement('div')
    document.body.appendChild(host)
})
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    while (cleanup.length) cleanup.pop()!()
    host.remove()
    vi.restoreAllMocks()
})

function installFlow() {
    let state: SyncBindingState = { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: 'initial', libraryId: null, progress: [] }
    const inspected = { inspectionId: 'inspection', targetId: 'target', libraryId: 'library', empty: true, previouslyBoundLibrary: false }
    const activated = () => { state = { target, targetAuthority: '1', selectionEpoch: 'activated', libraryId: 'library', progress: [] }; return state }
    const transport: SyncBindingTransport = {
        inspectTarget: vi.fn(async () => inspected),
        pullAvailableState: vi.fn(async () => ({ targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'receive' })),
        fenceOldJobs: vi.fn(async () => {}),
        replaceFromTarget: vi.fn(async () => {}),
        publishInitialSharedState: vi.fn(async () => {}),
        resumeBinding: vi.fn(async () => {}),
        prepareNewDeviceBinding: vi.fn(async () => ({ authorizationId: 'native-authorization', writerId: 'reserved-writer' })),
        replaceAsNewDevice: vi.fn(async () => { activated(); return { revision: 1, writerId: 'reserved-writer', bindingAuthority: '1' } }),
        resumeNewDeviceBinding: vi.fn(async () => {}),
    }
    const flow = createSyncBindingFlow({
        native: {
            state: async () => structuredClone(state),
            assertAuthority: async expected => { if (expected.targetAuthority !== state.targetAuthority || expected.selectionEpoch !== state.selectionEpoch) throw Error('changed binding') },
            switchTarget: async () => activated(),
        },
        gate: { runTransition: operation => operation(), runWrite: operation => operation(), runKeyedWrite: (_key, operation) => operation() },
        plugins: { fenceExecution: async () => {}, invalidateCaches: async () => {}, restart: async () => {} },
        withPausedWrites: operation => operation(),
        hasNonDefaultData: async () => false,
        hasNonDefaultSharedData: async () => true,
        confirmReplacement: confirmSyncBindingReplacement,
        refreshActivatedLibrary: async () => {},
        beginActivatedLibraryGuard: () => ({ complete() {}, async abortUnchanged() {} }),
    })
    cleanup.push(registry.registerSyncBindingFlow(flow), registry.registerSyncBindingTransport(target, transport))
    return { flow, transport }
}

it('forwards explicit options unchanged through the registry and requires the one G acknowledgement', async () => {
    const { flow, transport } = installFlow()
    const dispatch = vi.spyOn(registry, 'bindSyncTarget')
    const bind = vi.spyOn(flow, 'bind')
    const options = { mode: 'new-device' } as const
    const onBound = vi.fn(); const onError = vi.fn()
    component = mount(BindingTargetSwitch, { target: host, props: { target, label: languageEnglish.lwwSync.newDeviceAction, options, onBound, onError } })
    await tick(); host.querySelector('button')!.click()
    await vi.waitFor(() => expect(onBound).toHaveBeenCalledOnce())
    expect(dispatch.mock.calls[0][1]).toBe(options)
    expect(bind.mock.calls[0][2]).toBe(options)
    expect(acknowledgement).toHaveBeenCalledOnce()
    expect(acknowledgement).toHaveBeenCalledWith(expect.objectContaining({ requireChecked: true }))
    expect(transport.pullAvailableState).toHaveBeenCalledOnce()
    expect(transport.replaceAsNewDevice).toHaveBeenCalledOnce()
    expect(onBound).toHaveBeenCalledWith(expect.objectContaining({ kind: 'bound', action: 'new-device' }))
    expect(onError).not.toHaveBeenCalled()
})

it('omits options for ordinary use and never enters new-device recovery', async () => {
    const { flow, transport } = installFlow()
    const dispatch = vi.spyOn(registry, 'bindSyncTarget')
    const bind = vi.spyOn(flow, 'bind')
    const onBound = vi.fn(); const onError = vi.fn()
    component = mount(BindingTargetSwitch, { target: host, props: { target, label: 'Connect', onBound, onError } })
    await tick(); host.querySelector('button')!.click()
    await vi.waitFor(() => expect(onBound).toHaveBeenCalledOnce())
    expect(dispatch).toHaveBeenCalledWith(target)
    expect(bind.mock.calls[0][2]).toEqual({})
    expect(bind.mock.calls[0][2]).not.toHaveProperty('mode')
    expect(transport.prepareNewDeviceBinding).not.toHaveBeenCalled()
    expect(transport.replaceAsNewDevice).not.toHaveBeenCalled()
    expect(transport.publishInitialSharedState).toHaveBeenCalledOnce()
    expect(acknowledgement).not.toHaveBeenCalled()
    expect(onError).not.toHaveBeenCalled()
})

it('cannot activate explicit recovery when its acknowledgement is unchecked', async () => {
    const { transport } = installFlow()
    acknowledgement.mockResolvedValue({ confirmed: true, checked: false })
    const onBound = vi.fn(); const onError = vi.fn()
    component = mount(BindingTargetSwitch, { target: host, props: { target, label: languageEnglish.lwwSync.newDeviceAction, options: { mode: 'new-device' }, onBound, onError } })
    await tick(); host.querySelector('button')!.click()
    await vi.waitFor(() => expect(onBound).toHaveBeenCalledWith({ kind: 'cancelled' }))
    expect(acknowledgement).toHaveBeenCalledOnce()
    expect(transport.pullAvailableState).not.toHaveBeenCalled()
    expect(transport.prepareNewDeviceBinding).not.toHaveBeenCalled()
    expect(transport.replaceAsNewDevice).not.toHaveBeenCalled()
    expect(onError).not.toHaveBeenCalled()
})
