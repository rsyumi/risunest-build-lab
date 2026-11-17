import type { InlayBlobMetadata } from '../storage/blobStore'
import type { GalleryItem } from './types'
import type { InlayGalleryQuery } from './inlayGallery'

export function retainGallerySelection(selected: ReadonlySet<string>, targets: ReadonlySet<string>, retained: ReadonlySet<string>): Set<string> {
    return new Set([...selected].filter(key => !targets.has(key) || retained.has(key)))
}

export async function readGallerySelectionRange(
    start: number, end: number,
    read: (offset: number, limit: number) => Promise<{ revision: number; scanId?: string | null; items: GalleryItem[] }>,
): Promise<string[]> {
    const keys: string[] = []
    let snapshot: { revision: number; scanId?: string | null } | undefined
    for (let offset = Math.min(start, end), last = Math.max(start, end); offset <= last;) {
        const page = await read(offset, Math.min(512, last - offset + 1))
        if (snapshot && (snapshot.revision !== page.revision || snapshot.scanId !== page.scanId)) throw new Error('Gallery changed during range selection')
        snapshot = page
        if (!page.items.length) break
        keys.push(...page.items.map(item => item.key))
        offset += page.items.length
    }
    return keys
}
export function galleryInlayItem(value: InlayBlobMetadata & { sortTime?: number; objectHash?: string | null }, revision?: number): GalleryItem {
    const pocket = value.metadata?.pocketRisu as Record<string, unknown> | undefined
    const recorded = value.metadata?.createdAt
    const imported = pocket?.createdAt
    return { ...value, contentRevision: value.objectHash ? undefined : revision, id: value.key, kind: 'inlay', type: value.inlayType === 'signature' ? 'other' : value.inlayType,
        sortTime: value.sortTime ?? (typeof recorded === 'number' && recorded > 0 ? recorded : typeof imported === 'number' && imported > 0 ? imported : 0) }
}
export function sortGalleryInlays(items: GalleryItem[], query: InlayGalleryQuery): GalleryItem[] {
    return items.filter(item => item.type !== 'other' && (query.type === 'all' || item.type === query.type))
        .sort((a, b) => (query.sort === 'largest' ? b.size - a.size : query.sort === 'oldest' ? (a.sortTime ?? 0) - (b.sortTime ?? 0) : (b.sortTime ?? 0) - (a.sortTime ?? 0)) || a.key.localeCompare(b.key))
}
/** The page replaces its previous window instead of accumulating visited pages. */
export function galleryWindow(items: GalleryItem[], offset: number): Map<number, GalleryItem> {
    return new Map(items.map((item, index) => [index + offset, item]))
}

export interface UnusedDeletePorts {
    flush(): Promise<void>
    revision(): Promise<number>
    scanRevision(): number | null
    rescan(): Promise<boolean>
    unusedKeys(): Promise<string[]>
    confirm(keys: string[], dropped: number): Promise<boolean>
    remove(keys: string[], revision: number): Promise<void>
    selection(keys: Set<string>): void
    allDropped(count: number): void
    isConflict(error: unknown): boolean
}
export async function deleteUnusedGallerySelection(selected: ReadonlySet<string>, ports: UnusedDeletePorts): Promise<void> {
    let keys = new Set(selected)
    let dropped = 0
    // A refused delete means the scan no longer describes the library, even
    // when the revision is unchanged (device-local plugin values).
    let stale = false
    while (keys.size) {
        await ports.flush()
        if ((stale || ports.scanRevision() !== await ports.revision()) && !await ports.rescan()) return
        stale = false
        const unused = new Set(await ports.unusedKeys())
        const retained = new Set([...keys].filter(key => unused.has(key)))
        dropped += keys.size - retained.size
        keys = retained
        ports.selection(keys)
        if (!keys.size) { ports.allDropped(dropped); return }
        const revision = ports.scanRevision()
        if (revision === null) return
        if (!await ports.confirm([...keys], dropped)) return
        await ports.flush()
        try {
            await ports.remove([...keys], revision)
            ports.selection(new Set())
            return
        } catch (error) {
            if (!ports.isConflict(error)) throw error
            stale = true
        }
    }
}

export function isGalleryRevisionConflict(error: unknown): boolean {
    if (typeof error === 'string') { try { return isGalleryRevisionConflict(JSON.parse(error)) } catch { return false } }
    return !!error && typeof error === 'object' && ('code' in error && error.code === 'revision-conflict' || 'name' in error && error.name === 'RevisionConflictError')
}




export function galleryOriginIds(metadata: Record<string, unknown> | undefined): { characterId: string; conversationId: string } | null {
    const pocket = metadata?.pocketRisu as Record<string, unknown> | undefined
    const characterId = typeof metadata?.charId === 'string' ? metadata.charId : pocket?.charId
    const conversationId = typeof metadata?.chatId === 'string' ? metadata.chatId : pocket?.chatId
    if (typeof characterId !== 'string' || !characterId || typeof conversationId !== 'string' || !conversationId) return null
    return { characterId, conversationId }
}
