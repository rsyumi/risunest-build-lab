export type GallerySize = 'normal' | 'large'
export interface GalleryItem {
    id: string
    key: string
    kind?: 'asset' | 'inlay'
    name: string
    ext: string
    mime: string
    size: number
    objectHash?: string | null
    contentRevision?: number
    type: 'image' | 'video' | 'audio' | 'other'
    width?: number
    height?: number
    sortTime?: number
    metadata?: Record<string, unknown>
    section?: string
    portrait?: boolean
}
export function galleryBytes(bytes: number): string {
    if (!Number.isFinite(bytes) || bytes < 0) return ''
    const units = ['B', 'KB', 'MB', 'GB', 'TB']
    let index = 0
    while (bytes >= 1024 && index < units.length - 1) { bytes /= 1024; index++ }
    return `${index === 0 ? bytes : bytes.toFixed(bytes >= 10 ? 1 : 2)} ${units[index]}`
}
