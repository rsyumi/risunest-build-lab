import { beforeEach, expect, it, vi } from 'vitest'
const state = vi.hoisted(() => ({
    epoch: 0, list: vi.fn(), read: vi.fn(), write: vi.fn(), compute: vi.fn(), invalidate: vi.fn(),
}))
vi.mock('../../../storage/persistentDataRuntime.svelte', () => ({ getPersistentDataRuntime: () => ({ getStorageAuthorityEpoch: () => state.epoch }) }))
vi.mock('../../../storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: () => ({ listAssetAliases: state.list }) }))
vi.mock('../../../storage/platformBlobStore', () => ({ resolveBlobStore: async () => ({ imageGeometry: { read: state.read, write: state.write, compute: state.compute } }) }))
vi.mock('../../../globalApi.svelte', () => ({ clearNativeAssetSourceCache: state.invalidate }))
import { ImageGeometryController } from '../imageGeometryController.svelte'

const contentHash = 'a'.repeat(64)
beforeEach(() => {
    vi.resetAllMocks()
    state.epoch = 1
    state.list.mockResolvedValue({ revision: 1, items: [{ kind: 'asset', key: 'assets/synthetic', mime: 'image/png', ext: 'png', objectHash: contentHash }] })
    state.read.mockResolvedValue([])
    state.write.mockResolvedValue(undefined)
    state.compute.mockResolvedValue({ contentHash, width: 10, height: 20 })
})

it('exposes committed results and refreshes cached size hints', async () => {
    const job = new ImageGeometryController()
    await job.start()
    expect(job.result).toMatchObject({ saved: 1, failed: 0, cancelled: false })
    expect(job.running).toBe(false)
    expect(state.invalidate).toHaveBeenCalledOnce()
})

it('prevents duplicate starts and discards computation after profile activation', async () => {
    let complete!: (value: unknown) => void
    state.compute.mockImplementation(() => new Promise(resolve => { complete = resolve }))
    const job = new ImageGeometryController()
    const running = job.start()
    await vi.waitFor(() => expect(state.compute).toHaveBeenCalledOnce())
    await job.start()
    state.epoch++
    complete({ contentHash, width: 10, height: 20 })
    await running
    expect(job.result).toMatchObject({ saved: 0, cancelled: true })
    expect(state.write).not.toHaveBeenCalled()
    expect(state.invalidate).not.toHaveBeenCalled()
})

it('reports index failure and allows an explicit retry', async () => {
    state.read.mockRejectedValueOnce(new Error('synthetic disk error'))
    const job = new ImageGeometryController()
    await job.start()
    expect(job.failed).toBe(true)
    expect(job.running).toBe(false)
    await job.start()
    expect(job.failed).toBe(false)
    expect(job.result?.saved).toBe(1)
})
