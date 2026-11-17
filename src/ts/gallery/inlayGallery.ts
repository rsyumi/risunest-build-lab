import { galleryInlayItem, sortGalleryInlays } from './inlayGalleryState'
export { galleryInlayItem, sortGalleryInlays, galleryWindow, deleteUnusedGallerySelection, isGalleryRevisionConflict } from './inlayGalleryState'
import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../platform'
import { listInlayAssetMetadata, getInlayAssetMetadata } from '../process/files/inlays'
import { resolveBlobStore } from '../storage/platformBlobStore'
import { getPersistentDataRuntime } from '../storage/persistentDataRuntime.svelte'
import { getPersistentDataStore, getPersistentStorageAuthority } from '../storage/persistentDataStoreFactory'
import { readCurrentRevision, RevisionConflictError, type AssetAlias } from '../storage/persistentDataStore'
import type { InlayBlobMetadata } from '../storage/blobStore'
import type { GalleryItem } from './types'

export type InlayGalleryType = 'all' | 'image' | 'video' | 'audio'
export interface InlayGalleryQuery {
    type: InlayGalleryType
    sort: 'newest' | 'oldest' | 'largest'
    usage: 'all' | 'unused' | 'unknown'
    characterId?: string
    offset: number
    limit: number
}
export interface GalleryLocation {
    kind: 'message' | 'character' | 'conversation' | 'settings' | 'plugin' | 'archive' | 'pending'
    characterId?: string
    characterName?: string
    conversationId?: string
    conversationName?: string
    messageId?: string
    messageIndex?: number
    archived: boolean
    trashed: boolean
}
export interface GalleryScanStatus {
    id: string
    revision: number
    state: 'running' | 'complete' | 'cancelled' | 'failed'
    completedItems: number
    totalItems: number
    completedConversations: number
    totalConversations: number
    uncheckedArchives: number
    uncheckedRecords: number
    counts: { used: number; unused: number; unknown: number }
    characters: { characterId: string; name?: string; count: number }[]
    error?: string
}
export interface GalleryLocations {
    revision: number | null
    state: 'used' | 'unused' | 'unknown' | null
    uncheckedArchives: number
    uncheckedRecords: number
    locations: GalleryLocation[]
}
export interface InlayGalleryPage {
    scanId?: string | null
    revision: number
    total: number
    totalBytes: number
    counts: Record<InlayGalleryType, number>
    items: GalleryItem[]
}
export async function readInlayGalleryPage(query: InlayGalleryQuery): Promise<InlayGalleryPage> {
    if (isTauri) {
        const page = await invoke<Omit<InlayGalleryPage, 'items'> & { items: (Extract<AssetAlias, { kind: 'inlay' }> & { sortTime: number })[] }>('pds_gallery_page', { query })
        return { ...page, items: page.items.map(item => galleryInlayItem(item, page.revision)) }
    }
    const revision = await galleryRevision()
    const all = (await listInlayAssetMetadata()).filter(item => item.inlayType !== 'signature').map(item => galleryInlayItem(item, revision))
    const filtered = sortGalleryInlays(all, query)
    return { revision, total: filtered.length, totalBytes: filtered.reduce((sum, item) => sum + item.size, 0),
        counts: { all: all.length, image: all.filter(item => item.type === 'image').length, video: all.filter(item => item.type === 'video').length, audio: all.filter(item => item.type === 'audio').length },
        items: filtered.slice(query.offset, query.offset + query.limit) }
}
export async function readInlayGalleryKeys(query: InlayGalleryQuery): Promise<string[]> {
    if (isTauri) return (await invoke<{ keys: string[] }>('pds_gallery_keys', { query })).keys
    return sortGalleryInlays((await listInlayAssetMetadata()).map(item => galleryInlayItem(item)), query).map(item => item.key)
}
export async function inlayGalleryIndex(query: InlayGalleryQuery, key: string): Promise<number | null> {
    if (isTauri) return invoke<number | null>('pds_gallery_index', { query, key })
    const index = (await readInlayGalleryKeys(query)).indexOf(key)
    return index < 0 ? null : index
}
export async function missingGalleryKeys(keys: ReadonlySet<string>): Promise<Set<string>> {
    if (isTauri) return new Set(await invoke<string[]>('pds_gallery_missing_keys', { keys: [...keys] }))
    const missing = new Set<string>()
    for (const key of keys) if (!await getInlayAssetMetadata(key)) missing.add(key)
    return missing
}
export const galleryRevision = () => readCurrentRevision(getPersistentDataStore())
export const readGalleryLocations = (key: string) => invoke<GalleryLocations>('pds_gallery_locations', { key })
export const startGalleryScan = (expectedRevision: number) => invoke<GalleryScanStatus>('pds_gallery_scan_start', { expectedRevision })
export const readGalleryScan = () => invoke<GalleryScanStatus | null>('pds_gallery_scan_status')
export const readGalleryScanResult = () => invoke<GalleryScanStatus | null>('pds_gallery_scan_result')
export const cancelGalleryScan = () => invoke<boolean>('pds_gallery_scan_cancel')
/** With `scanId`, the native store deletes only while that usage scan is still current. */
export async function deleteGalleryKeys(keys: string[], expectedRevision: number, scanId?: string): Promise<void> {
    if (isTauri) {
        const runtime = getPersistentDataRuntime()
        const epoch = runtime.getStorageAuthorityEpoch()
        await runtime.runStorageOnlyMutation(current => getPersistentStorageAuthority().gate.runWrite(async () => {
            if (current !== expectedRevision) throw new RevisionConflictError(expectedRevision, current)
            if (epoch !== runtime.getStorageAuthorityEpoch()) throw new Error('Storage authority changed')
            return (await invoke<{ revision: number }>('pds_gallery_delete', { keys, expectedRevision, scanId })).revision
        }))
    } else {
        const store = await resolveBlobStore()
        for (const key of keys) await store.remove(key)
    }
}

export async function readGalleryMetadata(keys: readonly string[]): Promise<InlayBlobMetadata[]> {
    if (isTauri) return (await getPersistentDataStore().readAssetAliasesByKeys('inlay', [...keys])).value.filter((item): item is typeof item & { kind: 'inlay' } => item.kind === 'inlay')
    const values: InlayBlobMetadata[] = []
    for (const key of keys) { const item = await getInlayAssetMetadata(key); if (item) values.push(item) }
    return values
}


export const galleryUsageCount = (keys: string[]) => invoke<number>('pds_gallery_usage_count', { keys })
