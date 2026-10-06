import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
const mock = vi.hoisted(() => ({ invoke: vi.fn(), begin: vi.fn(), selectedIndex: 0 }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mock.invoke }))
vi.mock('../../mobileBackgroundTask', () => ({ beginMobileBackgroundTask: mock.begin }))
vi.mock('src/ts/stores.svelte', () => ({ selectedCharID: { subscribe: (run: (value: number) => void) => { run(mock.selectedIndex); return () => {} } } }))
vi.mock('../database.svelte', () => ({ getDatabase: () => ({ characters: [{ chaId: 'selected-policy-character' }] }) }))
import { downloadRemoteAssets, evictLocalAssets, getAssetResidencyStatus, setAssetResidencyPolicy } from './serverAssetResidency'

const status = { policy: 'full', localBytes: 2, remoteBytes: 1, remoteObjects: 1, serverBytes: 1, serverObjects: 1, externalObjects: [], unavailableObjects: 0, evictedBytes: 0 }
beforeEach(() => { vi.resetAllMocks(); mock.selectedIndex = 0 })
afterEach(() => vi.restoreAllMocks())
describe('asset residency mobile lifetime', () => {
    it('prioritizes the actual selected character when switching to full policy', async () => {
        mock.begin.mockResolvedValue({ dispose: vi.fn(async () => {}), progress: vi.fn() })
        mock.invoke.mockResolvedValue(status)
        await setAssetResidencyPolicy('full')
        expect(mock.invoke).toHaveBeenCalledWith('server_sync_asset_policy', { policy: 'full', selectedCharacterId: 'selected-policy-character' })
        mock.selectedIndex = -1
        await setAssetResidencyPolicy('full')
        expect(mock.invoke).toHaveBeenCalledWith('server_sync_asset_policy', { policy: 'full', selectedCharacterId: null })
    })
    it.each(['full', 'cleanup'])('retains and releases the %s task', async kind => {
        const dispose = vi.fn(async () => {})
        const progress = vi.fn()
        mock.begin.mockResolvedValue({ dispose, progress })
        mock.invoke.mockResolvedValue(status)
        await (kind === 'full' ? setAssetResidencyPolicy('full') : evictLocalAssets())
        expect(mock.begin).toHaveBeenCalledExactlyOnceWith('sync')
        expect(progress).toHaveBeenCalledExactlyOnceWith(null)
        expect(dispose).toHaveBeenCalledOnce()
    })
    it('downloads one connection or every holder without a policy, keeping the selected character first', async () => {
        const dispose = vi.fn(async () => {})
        mock.begin.mockResolvedValue({ dispose, progress: vi.fn() })
        mock.invoke.mockResolvedValue(status)
        await downloadRemoteAssets('receiver')
        expect(mock.invoke).toHaveBeenCalledWith('asset_residency_download_remote', { connectionId: 'receiver', selectedCharacterId: 'selected-policy-character', operationId: expect.any(String) })
        await downloadRemoteAssets()
        expect(mock.invoke).toHaveBeenCalledWith('asset_residency_download_remote', { connectionId: null, selectedCharacterId: 'selected-policy-character', operationId: expect.any(String) })
        expect(mock.begin).toHaveBeenCalledTimes(2)
        expect(dispose).toHaveBeenCalledTimes(2)
    })
    it('reads bodies per holder and rejects a malformed split', async () => {
        const split = { ...status, remoteObjects: 3, externalObjects: [{ connectionId: 'receiver', objects: 2 }] }
        mock.invoke.mockResolvedValueOnce(split)
        await expect(getAssetResidencyStatus()).resolves.toEqual(split)
        for (const invalid of [
            { ...status, serverObjects: undefined },
            { ...status, serverBytes: -1 },
            { ...status, externalObjects: undefined },
            { ...status, externalObjects: [{ connectionId: '', objects: 1 }] },
            { ...status, externalObjects: [{ connectionId: 'receiver', objects: 1.5 }] },
        ]) {
            mock.invoke.mockResolvedValueOnce(invalid)
            await expect(getAssetResidencyStatus()).rejects.toThrow('invalid-asset-residency-status')
        }
    })
    it('writes remote policy without background admission', async () => {
        mock.invoke.mockResolvedValue({ ...status, policy: 'remote' })
        await setAssetResidencyPolicy('remote')
        expect(mock.begin).not.toHaveBeenCalled()
    })
    it('cancels the active operation on expiry and releases after failure', async () => {
        const controller = new AbortController()
        const dispose = vi.fn(async () => {})
        mock.begin.mockResolvedValue({ signal: controller.signal, dispose, progress: vi.fn() })
        let reject!: (error: Error) => void
        mock.invoke.mockImplementation(name => name === 'server_sync_cancel' ? Promise.resolve() : new Promise((_, fail) => { reject = fail }))
        const run = evictLocalAssets()
        const rejected = expect(run).rejects.toThrow('cancelled')
        await vi.waitFor(() => expect(mock.invoke).toHaveBeenCalledWith('server_sync_asset_evict', undefined))
        controller.abort()
        expect(mock.invoke).toHaveBeenCalledWith('server_sync_cancel', undefined)
        reject(new Error('cancelled'))
        await rejected
        expect(dispose).toHaveBeenCalledOnce()
    })
    it('does not enter native admission after an already expired task', async () => {
        const controller = new AbortController()
        controller.abort(new Error('expired'))
        const dispose = vi.fn(async () => {})
        mock.begin.mockResolvedValue({ signal: controller.signal, dispose, progress: vi.fn() })
        await expect(evictLocalAssets()).rejects.toThrow('expired')
        expect(mock.invoke).not.toHaveBeenCalled()
        expect(dispose).toHaveBeenCalledOnce()
    })
    it('cancels only this download and retains its lifetime until the native worker settles', async () => {
        const controller = new AbortController()
        const dispose = vi.fn(async () => {})
        mock.begin.mockResolvedValue({ dispose, progress: vi.fn() })
        let finish!: (value: unknown) => void
        mock.invoke.mockImplementation(name => name === 'asset_residency_download_remote' ? new Promise(resolve => { finish = resolve }) : Promise.resolve())
        const run = downloadRemoteAssets('receiver', { signal: controller.signal })
        const rejected = expect(run).rejects.toMatchObject({ name: 'AbortError' })
        await vi.waitFor(() => expect(mock.invoke).toHaveBeenCalledWith('asset_residency_download_remote', expect.any(Object)))
        const operationId = mock.invoke.mock.calls[0][1].operationId
        controller.abort()
        expect(mock.invoke).toHaveBeenCalledWith('server_sync_cancel', { operationId })
        expect(dispose).not.toHaveBeenCalled()
        finish(status)
        await rejected
        expect(dispose).toHaveBeenCalledOnce()
        mock.invoke.mockResolvedValue(status)
        await downloadRemoteAssets('receiver')
        expect(mock.invoke.mock.calls.at(-1)?.[1].operationId).not.toBe(operationId)
    })
    it('requires an exact target membership result and forwards the same target to download', async () => {
        const target = { kind: 'external', connectionId: 'shared-target' } as const
        mock.invoke.mockResolvedValueOnce(status)
        await expect(getAssetResidencyStatus(target)).rejects.toThrow('invalid-asset-residency-status')
        mock.invoke.mockResolvedValue({ ...status, previousStorageObjects: 0 })
        await expect(getAssetResidencyStatus(target)).resolves.toMatchObject({ previousStorageObjects: 0 })
        expect(mock.invoke).toHaveBeenLastCalledWith('server_sync_asset_status', { target })
        mock.begin.mockResolvedValue({ dispose: vi.fn(async () => {}), progress: vi.fn() })
        mock.invoke.mockResolvedValue(status)
        await downloadRemoteAssets(undefined, { target })
        expect(mock.invoke).toHaveBeenCalledWith('asset_residency_download_remote', expect.objectContaining({ target, connectionId: null }))
    })
    it.each(['user', 'mobile'])('reconciles %s cancellation while native preparation is still pending', async source => {
        const controller = new AbortController()
        const dispose = vi.fn(async () => {})
        mock.begin.mockResolvedValue({ signal: source === 'mobile' ? controller.signal : undefined, dispose, progress: vi.fn() })
        let prepare!: () => void
        mock.invoke.mockImplementation(name => name === 'asset_residency_prepare_download' ? new Promise<void>(resolve => { prepare = resolve }) : Promise.resolve())
        const run = downloadRemoteAssets('receiver', { signal: source === 'user' ? controller.signal : undefined })
        const rejected = expect(run).rejects.toMatchObject({ name: 'AbortError' })
        await vi.waitFor(() => expect(mock.invoke).toHaveBeenCalledOnce())
        const operationId = mock.invoke.mock.calls[0][1].operationId
        expect(mock.invoke).toHaveBeenCalledWith('asset_residency_prepare_download', { operationId })
        controller.abort()
        expect(mock.invoke).not.toHaveBeenCalledWith('server_sync_cancel', expect.anything())
        expect(dispose).not.toHaveBeenCalled()
        prepare()
        await rejected
        expect(mock.invoke).toHaveBeenCalledWith('server_sync_cancel', { operationId })
        expect(mock.invoke).not.toHaveBeenCalledWith('asset_residency_download_remote', expect.anything())
        expect(dispose).toHaveBeenCalledOnce()
    })
    it('releases preparation if the download invocation fails before returning a status', async () => {
        const dispose = vi.fn(async () => {})
        mock.begin.mockResolvedValue({ dispose, progress: vi.fn() })
        mock.invoke.mockImplementation(name => name === 'asset_residency_download_remote' ? Promise.reject(new Error('invoke-failed')) : Promise.resolve())
        await expect(downloadRemoteAssets('receiver')).rejects.toThrow('invoke-failed')
        const operationId = mock.invoke.mock.calls[0][1].operationId
        expect(mock.invoke).toHaveBeenLastCalledWith('server_sync_cancel', { operationId })
        expect(dispose).toHaveBeenCalledOnce()
    })
})
