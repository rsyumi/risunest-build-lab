import { beforeEach, expect, it, vi } from 'vitest'
const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
import { createNativeSyncBindingBridge, replaceNativeSyncBinding, replaceNativeSyncBindingAsNewDevice } from './bindingNative'
const state = { target: { kind: 'none' } as const, targetAuthority: '3', selectionEpoch: 'epoch', libraryId: null, progress: [] }
beforeEach(() => invoke.mockReset())
it('switches only using the native inspection token and exact previous authority', async () => {
    invoke.mockResolvedValue(state)
    await createNativeSyncBindingBridge().switchTarget(state, { kind: 'server', connectionId: 'connection' }, { inspectionId: 'native-receipt', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: false }, 'frozen-switch', true)
    expect(invoke).toHaveBeenCalledWith('pds_lww_switch_target', { request: expect.objectContaining({ bindingAuthority: '3', expectedSelectionEpoch: 'epoch', inspectionId: 'native-receipt', target: { kind: 'server', connectionId: 'connection' }, initialPublication: true }) })
    expect(invoke.mock.calls[0][1].request).not.toHaveProperty('libraryId')
})
it('rejects stale native authority and selection epochs', async () => {
    invoke.mockResolvedValue({ ...state, selectionEpoch: 'changed' })
    await expect(createNativeSyncBindingBridge().assertAuthority(state)).rejects.toThrow('Sync binding changed')
})
it('uses the staged receive identity for replacement retries', async () => {
    invoke.mockResolvedValue(undefined)
    await replaceNativeSyncBinding({ targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'immutable-operation' }, { state, signal: new AbortController().signal })
    expect(invoke).toHaveBeenCalledWith('pds_lww_replace_from_target', { request: { bindingAuthority: '3', requestId: 'immutable-operation', expectedSelectionEpoch: 'epoch', targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'immutable-operation' } })
})
it('an aborted old transport cannot invoke replacement', async () => {
    const controller = new AbortController(); controller.abort()
    await expect(replaceNativeSyncBinding({ targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'receive' }, { state, signal: controller.signal })).rejects.toThrow()
    expect(invoke).not.toHaveBeenCalled()
})
it('replays a lost switch response with the identical caller-frozen request body', async () => {
    invoke.mockRejectedValueOnce(Error('response lost')).mockResolvedValue(state)
    const bridge = createNativeSyncBindingBridge()
    const target = { kind: 'server', connectionId: 'connection' } as const
    const inspection = { inspectionId: 'native-receipt', targetId: 'target', libraryId: 'library', empty: false, previouslyBoundLibrary: false }
    await expect(bridge.switchTarget(state, target, inspection, 'frozen-switch', false)).rejects.toThrow('response lost')
    await bridge.switchTarget(state, target, inspection, 'frozen-switch', false)
    expect(JSON.stringify(invoke.mock.calls[1])).toBe(JSON.stringify(invoke.mock.calls[0]))
    expect(invoke.mock.calls[1][1].request.requestId).toBe('frozen-switch')
})
it('replays lost ordinary and new-device replacement responses with identical native request bytes', async () => {
    const staged = { targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'immutable-operation' }
    const context = { state, signal: new AbortController().signal }
    for (const replace of [
        () => replaceNativeSyncBinding(staged, context),
        () => replaceNativeSyncBindingAsNewDevice(staged, { authorizationId: 'authorization', writerId: 'reserved' }, context),
    ]) {
        invoke.mockReset().mockRejectedValueOnce(Error('response lost')).mockResolvedValue({ revision: 2, writerId: 'reserved', bindingAuthority: '4' })
        await expect(replace()).rejects.toThrow('response lost')
        await replace()
        expect(JSON.stringify(invoke.mock.calls[1])).toBe(JSON.stringify(invoke.mock.calls[0]))
        expect(invoke.mock.calls[1][1].request.requestId).toBe('immutable-operation')
    }
})
it('new device completion uses original authority and only a native authorization token', async () => {
    const result = { revision: 42, writerId: 'reserved-writer', bindingAuthority: '4' }
    invoke.mockResolvedValue(result)
    expect(await replaceNativeSyncBindingAsNewDevice(
        { targetId: 'target', libraryId: 'library', stagingId: 'stage', receiveId: 'receive' },
        { authorizationId: 'native-authorization', writerId: 'reserved-writer' },
        { state, signal: new AbortController().signal },
    )).toEqual(result)
    expect(invoke).toHaveBeenCalledWith('pds_lww_replace_target_as_new_device', { request: {
        bindingAuthority: '3', requestId: 'receive', stagingId: 'stage', authorizationId: 'native-authorization',
    } })
    expect(invoke.mock.calls[0][1].request).not.toHaveProperty('writerId')
})
