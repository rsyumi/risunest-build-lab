import type { AssetAliasListQuery, AssetAliasPage } from '../../storage/persistentDataStore'
import { validImageDimensions, type ImageGeometry, type ImageGeometryStore } from '../../storage/imageGeometry'

export interface ImageGeometryProgress {
    saved: number
    skipped: number
    failed: number
    cancelled: boolean
    catalogChanged: boolean
}

export function emptyImageGeometryProgress(): ImageGeometryProgress {
    return { saved: 0, skipped: 0, failed: 0, cancelled: false, catalogChanged: false }
}

export async function calculateMissingImageGeometry(deps: {
    list(query: AssetAliasListQuery): Promise<AssetAliasPage>
    geometry: ImageGeometryStore
    isCancelled(): boolean
    onProgress(value: ImageGeometryProgress): void
}): Promise<ImageGeometryProgress> {
    const result = emptyImageGeometryProgress()
    const seen = new Set<string>()
    let cursor: string | undefined
    let revision: AssetAliasPage['revision'] | undefined
    const report = () => deps.onProgress({ ...result })
    const cancelled = () => result.cancelled = deps.isCancelled()
    const pending: ImageGeometry[] = []
    async function flush() {
        if (!pending.length || cancelled()) return
        const batch = pending.splice(0)
        try {
            await deps.geometry.write(batch)
            result.saved += batch.length
        } catch {
            result.failed += batch.length
        }
        report()
    }
    while (!cancelled()) {
        const page = await deps.list({ limit: 64, cursor })
        if (cancelled()) break
        if (revision !== undefined && page.revision !== revision) {
            result.catalogChanged = true
            break
        }
        revision = page.revision
        const images = page.items.filter(alias => {
            const image = alias.kind === 'inlay' ? alias.inlayType === 'image'
                : alias.mime.startsWith('image/') || /^(png|apng|jpe?g|webp|gif|avif|bmp|tiff?|svg)$/i.test(alias.ext)
            if (!image) return false
            if (!alias.objectHash) { result.skipped++; return false }
            if (seen.has(alias.objectHash)) return false
            seen.add(alias.objectHash)
            return true
        })
        const known = new Set((await deps.geometry.read(images.map(alias => alias.objectHash!))).map(value => value.contentHash))
        for (const alias of images) {
            if (cancelled()) break
            if (known.has(alias.objectHash!)) {
                result.skipped++
                continue
            }
            try {
                const value = alias.kind === 'inlay' && validImageDimensions(alias.width, alias.height)
                    ? { contentHash: alias.objectHash!, width: alias.width!, height: alias.height! }
                    : await deps.geometry.compute(alias.objectHash!)
                if (cancelled()) break
                if (value) pending.push(value)
                else result.skipped++
            } catch {
                if (cancelled()) break
                result.failed++
            }
            if (pending.length === 16) await flush()
            report()
        }
        await flush()
        if (cancelled()) break
        report()
        if (!page.nextCursor) {
            const final = await deps.list({ limit: 1 })
            if (!cancelled()) result.catalogChanged = final.revision !== revision
            break
        }
        if (page.nextCursor === cursor) throw new Error('Image catalog cursor did not advance')
        cursor = page.nextCursor
        await new Promise<void>(resolve => setTimeout(resolve, 0))
    }
    report()
    return result
}
