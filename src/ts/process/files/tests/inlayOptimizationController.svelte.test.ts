import { describe, expect, test, vi } from 'vitest'
vi.mock('../../../mobileBackgroundTask', async () => (await import('../../../mobileBackgroundTask.testSupport')).mobileBackgroundTaskModule())
import { InlayOptimizationController } from '../inlayOptimizationController.svelte'
import { measuredTaskPercent, runWithMobileBackgroundTask } from '../../../mobileBackgroundTask'
import { defaultInlayEncodeOptions, type InlayBlobMetadata } from '../../../storage/blobStore'

describe('shared optimization ownership', () => {
    test('refuses a second owner and cancels an in-flight encoder before publication', async () => {
        const controller = new InlayOptimizationController()
        let resume!: () => void
        const encoding = new Promise<void>(resolve => { resume = resolve })
        const target: InlayBlobMetadata = { key: 'synthetic', kind: 'inlay', inlayType: 'image', mime: 'image/png', ext: 'png', name: 'synthetic', size: 10 }
        const write = vi.fn()
        const deps = { read: async () => new Uint8Array(10), write, encoder: { encodeNewInlayImage: async () => {
            await encoding
            return { data: new Uint8Array(1), metadata: { ...target, mime: 'image/webp', ext: 'webp' } }
        } } }
        const pending = controller.start([target], defaultInlayEncodeOptions, deps)
        expect(controller.running).toBe(true)
        expect(await controller.start([target], defaultInlayEncodeOptions, deps)).toBeNull()
        controller.cancel()
        resume()
        await pending
        expect(write).not.toHaveBeenCalled()
        expect(controller.running).toBe(false)
        expect(controller.result).toMatchObject({ scanned: 1, skipped: 1, converted: 0 })
    })
    test('does not re-encode an unchanged preserved source with the same policy', async () => {
        const controller = new InlayOptimizationController()
        const target: InlayBlobMetadata = { key: 'kept', kind: 'inlay', inlayType: 'image', mime: 'image/png', ext: 'png', name: 'kept', size: 10 }
        let source = new Uint8Array(10)
        const encodeNewInlayImage = vi.fn(async (_key, data) => ({ data, metadata: target }))
        const deps = { read: async () => source, write: vi.fn(), encoder: { encodeNewInlayImage } }
        await controller.start([target], defaultInlayEncodeOptions, deps)
        await controller.start([target], defaultInlayEncodeOptions, deps)
        expect(encodeNewInlayImage).toHaveBeenCalledOnce()
        source = new Uint8Array(10).fill(1)
        await controller.start([target], defaultInlayEncodeOptions, deps)
        expect(encodeNewInlayImage).toHaveBeenCalledTimes(2)
        expect(deps.write).not.toHaveBeenCalled()
    })
    test('runs under a maintenance hold that reports progress and stops when its time expires', async () => {
        const controller = new InlayOptimizationController()
        const expiry = new AbortController()
        const progress = vi.fn()
        vi.mocked(runWithMobileBackgroundTask).mockImplementationOnce(async (_kind, operation) => operation({ signal: expiry.signal, progress, async dispose() {} }))
        const targets: InlayBlobMetadata[] = ['first', 'second'].map(key => ({ key, kind: 'inlay', inlayType: 'image', mime: 'image/png', ext: 'png', name: key, size: 10 }))
        const encodeNewInlayImage = vi.fn(async (_key: string, _data: Uint8Array) => {
            expiry.abort()
            return { data: new Uint8Array(1), metadata: { ...targets[0], mime: 'image/webp', ext: 'webp' } }
        })
        const deps = { read: async () => new Uint8Array(10), write: vi.fn(), encoder: { encodeNewInlayImage } }
        await controller.start(targets, defaultInlayEncodeOptions, deps)
        expect(runWithMobileBackgroundTask).toHaveBeenLastCalledWith('maintenance', expect.any(Function), undefined, true)
        expect(encodeNewInlayImage).toHaveBeenCalledOnce()
        expect(deps.write).not.toHaveBeenCalled()
        // The hold shows the share of the two images scanned so far.
        expect(measuredTaskPercent).toHaveBeenLastCalledWith(expect.any(Number), 2)
        expect(progress).toHaveBeenCalled()
    })

})
