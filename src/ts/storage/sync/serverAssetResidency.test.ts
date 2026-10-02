import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
const mock = vi.hoisted(() => ({ invoke: vi.fn(), begin: vi.fn(), selectedIndex: 0 }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mock.invoke }))
vi.mock('../../mobileBackgroundTask', () => ({ beginMobileBackgroundTask: mock.begin }))
vi.mock('src/ts/stores.svelte', () => ({ selectedCharID: { subscribe: (run: (value: number) => void) => { run(mock.selectedIndex); return () => {} } } }))
vi.mock('../database.svelte', () => ({ getDatabase: () => ({ characters: [{ chaId: 'selected-policy-character' }] }) }))
import { evictLocalAssets, setAssetResidencyPolicy } from './serverAssetResidency'

const status = { policy: 'full', localBytes: 2, remoteBytes: 1, remoteObjects: 1, unavailableObjects: 0, evictedBytes: 0 }
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
        expect(mock.invoke).toHaveBeenCalledWith('server_sync_cancel')
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
})
