import type { GalleryMediaSource } from './galleryMedia'
import type { GalleryItem } from './types'

export function isGalleryThumbnailFormat(mime: string, ext: string): boolean {
    const normalized = mime.split(';')[0].trim().toLowerCase()
    return /^image\/(png|x-png|apng|jpeg|jpg|pjpeg|webp|gif)$/.test(normalized)
        || (!normalized || normalized === 'application/octet-stream') && /^(png|apng|jpe?g|webp|gif)$/i.test(ext)
}

export function galleryPreviewSource(item: GalleryItem, source: GalleryMediaSource | null,
    nativePreview: boolean): GalleryMediaSource | null {
    if (!nativePreview || !source || source.objectUrl || item.type !== 'image') return source
    if (!/^http:\/\/127\.0\.0\.1:[1-9][0-9]*\/[a-f0-9]{32}\/[a-f0-9]+\?/.test(source.url)) return source
    const url = new URL(source.url)
    if (!url.searchParams.has('mime') || !url.searchParams.has('size')) return source
    if (!isGalleryThumbnailFormat(url.searchParams.get('mime') || item.mime, item.ext)) return source
    url.searchParams.set('thumbnail', 'gallery-v1')
    return { ...source, url: url.toString(), thumbnail: true, recordDimensions: undefined }
}
