import { getPersistentDataRuntime } from '../../storage/persistentDataRuntime.svelte'
import { getPersistentDataStore } from '../../storage/persistentDataStoreFactory'
import { resolveBlobStore } from '../../storage/platformBlobStore'
import { clearNativeAssetSourceCache } from '../../globalApi.svelte'
import { calculateMissingImageGeometry, emptyImageGeometryProgress, type ImageGeometryProgress } from './imageGeometryJob'

export class ImageGeometryController {
    running = $state(false)
    cancelRequested = $state(false)
    progress = $state<ImageGeometryProgress>(emptyImageGeometryProgress())
    result = $state<ImageGeometryProgress | null>(null)
    failed = $state(false)

    cancel(): void { this.cancelRequested = true }

    async start(): Promise<void> {
        if (this.running) return
        this.running = true
        this.cancelRequested = false
        this.failed = false
        this.result = null
        this.progress = emptyImageGeometryProgress()
        try {
            const runtime = getPersistentDataRuntime()
            const epoch = runtime.getStorageAuthorityEpoch()
            const geometry = (await resolveBlobStore()).imageGeometry
            if (!geometry) throw new Error('Image geometry is unavailable')
            this.result = await calculateMissingImageGeometry({
                geometry,
                list: query => getPersistentDataStore().listAssetAliases(query),
                isCancelled: () => this.cancelRequested || runtime.getStorageAuthorityEpoch() !== epoch,
                onProgress: value => { this.progress = value },
            })
        } catch {
            this.failed = true
        } finally {
            if (this.progress.saved > 0) clearNativeAssetSourceCache()
            this.running = false
        }
    }
}

export const imageGeometryController = new ImageGeometryController()
