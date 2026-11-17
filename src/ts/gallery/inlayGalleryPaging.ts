import type { InlayGalleryPage, InlayGalleryQuery } from './inlayGallery'
import type { GalleryItem } from './types'

export function inlayGalleryPageRange(offset: number, limit: number): { offset: number; limit: number } {
    const count = Math.max(1, Math.min(512, limit))
    const aligned = Math.floor(offset / 128) * 128
    const needed = offset - aligned + count
    if (needed > 512) return { offset, limit: count }
    return { offset: aligned, limit: Math.min(512, (Math.ceil(needed / 128) + 1) * 128) }
}

/** One metadata request and one latest desired window, independent of distance scrolled. */
export class InlayGalleryPaging {
    private desired: InlayGalleryQuery | null = null
    private epoch = 0
    private appliedEpoch = -1
    private scanId: string | null = null
    private revision = -1
    private minimumRevision = -1
    private total = 0
    private items = new Map<number, GalleryItem>()
    private running: Promise<void> | null = null
    private disposed = false

    constructor(
        private readonly read: (query: InlayGalleryQuery) => Promise<InlayGalleryPage>,
        private readonly apply: (page: InlayGalleryPage, items: Map<number, GalleryItem>) => void,
    ) {}

    observeRevision(revision: number): void {
        this.minimumRevision = Math.max(this.minimumRevision, revision)
    }

    request(query: InlayGalleryQuery, refresh = false): Promise<void> {
        if (this.disposed) return Promise.resolve()
        const previous = this.desired
        if (refresh || !previous || previous.type !== query.type || previous.sort !== query.sort
            || previous.usage !== query.usage || previous.characterId !== query.characterId) this.epoch++
        this.desired = { ...query, limit: Math.max(1, query.limit) }
        if (!this.running) {
            this.running = Promise.resolve().then(() => this.drain()).then(() => {
                this.running = null
                if (!this.disposed && this.desired && !this.covered(this.desired)) return this.request(this.desired)
            }, error => { this.running = null; throw error })
        }
        return this.running
    }

    dispose(): void { this.disposed = true; this.desired = null; this.items.clear() }

    private covered(query: InlayGalleryQuery): boolean {
        if (this.appliedEpoch !== this.epoch || this.revision < this.minimumRevision) return false
        const end = Math.min(this.total, query.offset + query.limit)
        for (let index = query.offset; index < end; index++) if (!this.items.has(index)) return false
        return true
    }

    private async drain(): Promise<void> {
        while (!this.disposed && this.desired && !this.covered(this.desired)) {
            const epoch = this.epoch
            const minimumRevision = this.minimumRevision
            let offset = this.desired.offset
            if (this.appliedEpoch === epoch && this.revision >= minimumRevision) {
                while (offset < Math.min(this.total, this.desired.offset + this.desired.limit) && this.items.has(offset)) offset++
            }
            const request = { ...this.desired, ...inlayGalleryPageRange(offset, this.desired.offset + this.desired.limit - offset) }
            let page: InlayGalleryPage
            try { page = await this.read(request) }
            catch (error) {
                if (epoch !== this.epoch) continue
                throw error
            }
            if (this.disposed || !this.desired) return
            if (epoch !== this.epoch) continue
            if (page.revision < this.minimumRevision) {
                if (minimumRevision < this.minimumRevision) continue
                throw new Error('Attachment metadata revision is out of date')
            }
            const desired = this.desired
            const range = inlayGalleryPageRange(desired.offset, desired.limit)
            const end = Math.max(range.offset + range.limit, desired.offset + desired.limit)
            const overlaps = request.offset < desired.offset + desired.limit && request.offset + page.items.length > desired.offset
            if (page.items.length && !overlaps && desired.offset < page.total) continue
            const sameSnapshot = this.appliedEpoch === epoch && this.revision === page.revision && this.scanId === (page.scanId ?? null)
            const retained = new Map<number, GalleryItem>()
            if (sameSnapshot) {
                for (const [index, item] of this.items) if (index >= range.offset && index < end) retained.set(index, item)
            }
            page.items.forEach((item, position) => {
                const index = request.offset + position
                if (index >= range.offset && index < end) retained.set(index, retained.get(index)?.key === item.key ? retained.get(index)! : item)
            })
            this.appliedEpoch = epoch
            this.scanId = page.scanId ?? null
            this.revision = page.revision
            this.observeRevision(page.revision)
            this.total = page.total
            this.items = retained
            this.apply(page, retained)
        }
    }
}
