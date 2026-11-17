import { describe, expect, it } from 'vitest'
import { InlayGalleryPaging, inlayGalleryPageRange } from './inlayGalleryPaging'
import type { InlayGalleryPage, InlayGalleryQuery } from './inlayGallery'
import type { GalleryItem } from './types'

const query = (offset: number, limit = 60): InlayGalleryQuery => ({ type: 'all', sort: 'newest', usage: 'all', offset, limit })
function page(request: InlayGalleryQuery, revision = 1): InlayGalleryPage {
    return {
        revision, total: 200_000, totalBytes: 200_000, counts: { all: 200_000, image: 200_000, video: 0, audio: 0 },
        items: Array.from({ length: request.limit }, (_, position) => {
            const key = `${request.type}-${request.offset + position}`
            return { id: key, key, name: key, ext: 'png', mime: 'image/png', size: 1, type: request.type === 'audio' ? 'audio' : 'image' }
        }),
    }
}
function fixture() {
    const requests: { query: InlayGalleryQuery; resolve(page: InlayGalleryPage): void }[] = []
    const displayed: { revision: number; items: Map<number, GalleryItem> }[] = []
    const paging = new InlayGalleryPaging(request => new Promise(resolve => { requests.push({ query: request, resolve }) }),
        (response, items) => { displayed.push({ revision: response.revision, items }) })
    return { paging, requests, displayed }
}
async function turn() { await Promise.resolve(); await Promise.resolve(); await Promise.resolve() }
function expectCovered(items: Map<number, GalleryItem>, offset: number, limit: number) {
    for (let index = offset; index < offset + limit; index++) expect(items.has(index)).toBe(true)
    expect(items.size).toBeLessThanOrEqual(512)
}

describe('attachment metadata paging', () => {
    it('covers even an unaligned 512-item window without exceeding the native page maximum', () => {
        for (let offset = 0; offset < 256; offset++) {
            for (const count of [1, 128, 256, 300, 384, 400, 511, 512]) {
                const range = inlayGalleryPageRange(offset, count)
                expect(range.offset).toBeLessThanOrEqual(offset)
                expect(range.offset + range.limit).toBeGreaterThanOrEqual(offset + count)
                expect(range.limit).toBeLessThanOrEqual(512)
            }
        }
    })
    it('installs the overlapping pending page after rapid row moves and reuses its margin', async () => {
        const { paging, requests, displayed } = fixture()
        let done = paging.request(query(0))
        await turn()
        const firstRun = done
        for (let offset = 3; offset <= 117; offset += 3) {
            done = paging.request(query(offset))
            expect(done).toBe(firstRun)
        }
        expect(requests).toHaveLength(1)
        expect(requests[0].query).toMatchObject({ offset: 0, limit: 256 })
        requests[0].resolve(page(requests[0].query))
        await done
        expectCovered(displayed.at(-1)!.items, 117, 60)
        await paging.request(query(180))
        expect(requests).toHaveLength(1)
    })
    it('keeps one read in flight and coalesces distant requests to the latest desired range', async () => {
        const { paging, requests, displayed } = fixture()
        void paging.request(query(0))
        await turn()
        void paging.request(query(300))
        void paging.request(query(600))
        const done = paging.request(query(900))
        // Later desired ranges cannot race an older read by completing first.
        expect(requests).toHaveLength(1)
        requests[0].resolve(page(requests[0].query))
        await turn()
        expect(requests).toHaveLength(2)
        expect(displayed).toHaveLength(0)
        expect(requests[1].query.offset).toBe(896)
        requests[1].resolve(page(requests[1].query))
        await done
        expectCovered(displayed.at(-1)!.items, 900, 60)
    })
    it('keeps displayed overlap while fetching, preserves item identity and evicts old metadata', async () => {
        const { paging, requests, displayed } = fixture()
        let done = paging.request(query(0))
        await turn()
        requests[0].resolve(page(requests[0].query))
        await done
        const before = displayed[0].items
        done = paging.request(query(200, 100))
        await turn()
        expect(displayed.at(-1)!.items).toBe(before)
        done = paging.request(query(230, 100))
        requests[1].resolve(page(requests[1].query))
        await done
        const after = displayed.at(-1)!.items
        expectCovered(after, 230, 100)
        expect(after.get(240)).toBe(before.get(240))
        expect(after.has(0)).toBe(false)
        done = paging.request(query(100_000, 100))
        await turn()
        requests[2].resolve(page(requests[2].query))
        await done
        expectCovered(displayed.at(-1)!.items, 100_000, 100)
        expect(displayed.at(-1)!.items.has(240)).toBe(false)
    })
    it('discards earlier filter and revision responses without rolling the displayed revision back', async () => {
        const { paging, requests, displayed } = fixture()
        void paging.request(query(0))
        await turn()
        let done = paging.request({ ...query(0), type: 'audio' }, true)
        requests[0].resolve(page(requests[0].query, 7))
        await turn()
        expect(displayed).toHaveLength(0)
        expect(requests[1].query.type).toBe('audio')
        paging.observeRevision(9)
        requests[1].resolve(page(requests[1].query, 8))
        await turn()
        expect(displayed).toHaveLength(0)
        requests[2].resolve(page(requests[2].query, 9))
        await done
        expect(displayed.at(-1)!.revision).toBe(9)
        expect(displayed.at(-1)!.items.get(0)?.type).toBe('audio')
        const previous = displayed.at(-1)!.items
        done = paging.request({ ...query(0), type: 'audio' }, true)
        await turn()
        const changed = page(requests[3].query, 9)
        changed.items[0].name = 'refreshed'
        requests[3].resolve(changed)
        await done
        expect(displayed.at(-1)!.items.get(0)?.name).toBe('refreshed')
        expect(displayed.at(-1)!.items.get(0)).not.toBe(previous.get(0))
        paging.observeRevision(8)
        done = paging.request({ ...query(0), type: 'audio' }, true)
        await turn()
        requests[4].resolve(page(requests[4].query, 8))
        await expect(done).rejects.toThrow('revision is out of date')
        expect(displayed.at(-1)!.revision).toBe(9)
    })
    it('does not lose an uncovered request made immediately after a covered one', async () => {
        const { paging, requests, displayed } = fixture()
        let done = paging.request(query(0))
        await turn()
        requests[0].resolve(page(requests[0].query))
        await done
        void paging.request(query(10))
        done = paging.request(query(500))
        await turn()
        expect(requests).toHaveLength(2)
        requests[1].resolve(page(requests[1].query))
        await done
        expectCovered(displayed.at(-1)!.items, 500, 60)
    })
    it('does not publish a pending page after the gallery closes', async () => {
        const { paging, requests, displayed } = fixture()
        const done = paging.request(query(0))
        await turn()
        paging.dispose()
        requests[0].resolve(page(requests[0].query))
        await done
        expect(displayed).toHaveLength(0)
    })
})

it('accepts a shortened last page and an empty result without retrying nonexistent rows', async () => {
    const { paging, requests, displayed } = fixture()
    let done = paging.request(query(900))
    await turn()
    const shortened = page(requests[0].query)
    shortened.total = 910
    shortened.items = shortened.items.slice(0, 910 - requests[0].query.offset)
    requests[0].resolve(shortened)
    await done
    expectCovered(displayed.at(-1)!.items, 900, 10)
    expect(requests).toHaveLength(1)
    done = paging.request(query(900), true)
    await turn()
    requests[1].resolve({ ...page(requests[1].query, 2), total: 0, totalBytes: 0, items: [] })
    await done
    expect(displayed.at(-1)!.items.size).toBe(0)
    await paging.request(query(0))
    expect(requests).toHaveLength(2)
})


it('publishes a shortened total so the visible window can clamp after deleting the last rows', async () => {
    const requests: { query: InlayGalleryQuery; resolve(page: InlayGalleryPage): void }[] = []
    let visible = query(900)
    let shown = new Map<number, GalleryItem>()
    const paging = new InlayGalleryPaging(request => new Promise(resolve => { requests.push({ query: request, resolve }) }),
        (response, items) => {
            shown = items
            if (response.total && visible.offset >= response.total) {
                visible = { ...visible, offset: Math.max(0, response.total - visible.limit) }
                void paging.request(visible)
            }
        })
    const done = paging.request(visible)
    await turn()
    const shortened = page(requests[0].query)
    shortened.total = 898
    shortened.items = shortened.items.slice(0, 898 - requests[0].query.offset)
    requests[0].resolve(shortened)
    await turn()
    expect(visible.offset).toBe(838)
    expect(requests).toHaveLength(2)
    const clamped = page(requests[1].query)
    clamped.total = 898
    clamped.items = clamped.items.slice(0, 898 - requests[1].query.offset)
    requests[1].resolve(clamped)
    await done
    expectCovered(shown, 838, 60)
    expect(shown.has(898)).toBe(false)
})

it('fills a wide viewport using bounded pages and retains all visible rows', async () => {
    const requests: InlayGalleryQuery[] = []
    let shown = new Map<number, GalleryItem>()
    const paging = new InlayGalleryPaging(async request => { requests.push(request); return page(request) },
        (_, items) => { shown = items })
    await paging.request(query(31, 900))
    for (let index = 31; index < 931; index++) expect(shown.get(index)?.key).toBe(`all-${index}`)
    expect(requests.every(request => request.limit <= 512)).toBe(true)
    expect(requests.length).toBeGreaterThan(1)
    await paging.request(query(20_000, 900))
    expect(shown.has(31)).toBe(false)
    for (let index = 20_000; index < 20_900; index++) expect(shown.has(index)).toBe(true)
})

it('never combines two scan orders at the same metadata revision', async () => {
    let scanId = 'before'
    let shown = new Map<number, GalleryItem>()
    const paging = new InlayGalleryPaging(async request => {
        const result = page(request)
        result.scanId = scanId
        if (scanId === 'after') result.items = result.items.map((item, index) => ({ ...item, key: `all-${request.offset + index - 200}` }))
        return result
    }, (_, items) => {
        shown = items
        expect(new Set([...items.values()].map(item => item.key)).size).toBe(items.size)
    })
    await paging.request(query(0))
    scanId = 'after'
    await paging.request(query(200, 100))
    for (let index = 200; index < 300; index++) expect(shown.get(index)?.key).toBe(`all-${index - 200}`)
})
