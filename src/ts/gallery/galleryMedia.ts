import type { GalleryItem } from './types'
import { getFileImageSource, getFileSrc } from '../globalApi.svelte'
import { getInlayRenderSource } from '../process/files/inlayRenderSource'
import { isTauri } from '../platform'
import { galleryPreviewSource, isGalleryThumbnailFormat } from './galleryPreview'
import { createTauriCasObjectUrl, getNativeMediaEndpoint } from '../storage/platformBlobStore'
import { inferBlobMime } from '../storage/blobStore'

export interface GalleryMediaSource {
    url: string
    width?: number
    height?: number
    size?: number
    objectUrl?: boolean
    thumbnail?: boolean
    recordDimensions?: (width: number, height: number) => Promise<void>
}
export async function resolveGalleryMedia(item: GalleryItem, preview = false): Promise<GalleryMediaSource | null> {
    if (isTauri && preview && item.kind === 'inlay' && item.type === 'image'
        && typeof item.objectHash === 'string' && /^[a-f0-9]{64}$/.test(item.objectHash)
        && Number.isSafeInteger(item.size) && item.size >= 0) {
        const mime = inferBlobMime(item.mime, item.ext)
        if (/^[\x20-\x7e]+$/.test(mime) && isGalleryThumbnailFormat(mime, item.ext)) {
            const url = createTauriCasObjectUrl({ contentHash:item.objectHash, mime, size:item.size }, await getNativeMediaEndpoint())
            return galleryPreviewSource(item, { url, width:item.width, height:item.height, size:item.size }, true)
        }
    }
    const source = await resolveOriginalGalleryMedia(item)
    return galleryPreviewSource(item, source, isTauri && preview)
}
async function resolveOriginalGalleryMedia(item: GalleryItem): Promise<GalleryMediaSource | null> {
    if (item.kind === 'inlay' || (!item.kind && !item.key.startsWith('assets/'))) {
        if (item.type === 'other') return null
        return getInlayRenderSource(item.key, isTauri, {
            key: item.key, kind: 'inlay', inlayType: item.type, name: item.name,
            ext: item.ext, mime: item.mime, size: item.size, width: item.width, height: item.height,
        })
    }
    if (item.type === 'image') {
        const image = await getFileImageSource(item.key)
        if (image) return { ...image, size: image.metadata.size }
    }
    const url = await getFileSrc(item.key)
    return url ? { url } : null
}
export function releaseGalleryMedia(source: GalleryMediaSource | null): void {
    if (source?.objectUrl) URL.revokeObjectURL(source.url)
}
