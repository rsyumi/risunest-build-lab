import type { InlayBlobMetadata, InlayEncodeOptions } from '../../storage/blobStore'
import { emptyInlayOptimizationProgress, inlayOptimizationGainRatio, runInlayOptimization, type InlayOptimizationDeps, type InlayOptimizationProgress } from './inlayOptimizationJob'

export class InlayOptimizationController {
    private readonly preserved = new Set<string>()
    running = $state(false)
    cancelRequested = $state(false)
    progress = $state<InlayOptimizationProgress>(emptyInlayOptimizationProgress())
    result = $state<InlayOptimizationProgress | null>(null)

    cancel(): void { this.cancelRequested = true }

    async start(targets: readonly InlayBlobMetadata[], options: InlayEncodeOptions, deps: InlayOptimizationDeps): Promise<InlayOptimizationProgress | null> {
        if (this.running) return null
        this.running = true
        this.cancelRequested = false
        this.result = null
        this.progress = emptyInlayOptimizationProgress(targets.length)
        try {
            const encoder: InlayOptimizationDeps['encoder'] = {
                encodeNewInlayImage: async (key, data, input) => {
                    const digest = await crypto.subtle.digest('SHA-256', data as Uint8Array<ArrayBuffer>)
                    const hash = Array.from(new Uint8Array(digest), value => value.toString(16).padStart(2, '0')).join('')
                    const fingerprint = JSON.stringify([key, hash, input.options])
                    if (this.preserved.has(fingerprint)) {
                        return { data, metadata: { kind: 'inlay', inlayType: 'image', mime: 'application/octet-stream', ext: '', name: input.name } }
                    }
                    const result = await deps.encoder.encodeNewInlayImage(key, data, input)
                    if (result.data.length > data.length * inlayOptimizationGainRatio) {
                        this.preserved.add(fingerprint)
                        if (this.preserved.size > 256) this.preserved.delete(this.preserved.values().next().value!)
                    }
                    return result
                },
            }
            this.result = await runInlayOptimization(targets, { ...deps, encoder }, {
                options,
                isCancelled: () => this.cancelRequested,
                onProgress: value => { this.progress = value },
            })
            return this.result
        } finally {
            this.running = false
            this.cancelRequested = false
        }
    }
}

export const inlayOptimizationController = new InlayOptimizationController()
