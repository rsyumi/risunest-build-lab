import { describe, expect, it } from 'vitest'
import { deleteUnusedGallerySelection, galleryInlayItem, galleryOriginIds, galleryWindow, sortGalleryInlays, retainGallerySelection, readGallerySelectionRange, type UnusedDeletePorts } from './inlayGalleryState'
import type { GalleryItem } from './types'

function item(key: string, size = 1): GalleryItem {
    return { id: key, key, name: key, ext: 'png', mime: 'image/png', size, type: 'image' }
}

describe('attachment gallery', () => {
    it('keeps unrelated selections when a viewer delete is rechecked or completed', () => {
        const selected = new Set(['selected-a', 'selected-b'])
        expect([...retainGallerySelection(selected, new Set(['viewer']), new Set(['viewer']))]).toEqual([...selected])
        expect([...retainGallerySelection(selected, new Set(['viewer']), new Set())]).toEqual([...selected])
        expect([...retainGallerySelection(selected, new Set(['selected-a']), new Set())]).toEqual(['selected-b'])
    })
    it('selects only the requested range in bounded pages and rejects mixed scan results', async () => {
        const records = Array.from({ length: 1200 }, (_, index) => item(String(index)))
        const read = async (offset: number, limit: number) => {
            expect(limit).toBeLessThanOrEqual(512)
            expect(offset).toBeGreaterThanOrEqual(100)
            expect(offset + limit).toBeLessThanOrEqual(1101)
            return { revision: 1, scanId: 'scan-a', items: records.slice(offset, offset + limit) }
        }
        expect(await readGallerySelectionRange(1100, 100, read)).toEqual(records.slice(100, 1101).map(value => value.key))
        await expect(readGallerySelectionRange(100, 1100, async (offset, limit) => ({
            ...await read(offset, limit), scanId: offset === 100 ? 'scan-a' : 'scan-b',
        }))).rejects.toThrow('Gallery changed')
    })
    it('uses creation metadata precedence, filters signatures and breaks date ties by key', () => {
        const created = galleryInlayItem({ key: 'b', kind: 'inlay', inlayType: 'image', name: 'b', ext: 'png', mime: 'image/png', size: 1, metadata: { createdAt: 100, pocketRisu: { createdAt: 200 } } })
        const imported = galleryInlayItem({ key: 'a', kind: 'inlay', inlayType: 'image', name: 'a', ext: 'png', mime: 'image/png', size: 1, metadata: { createdAt: -1, pocketRisu: { createdAt: 100 } } })
        const hidden = { ...item('signature'), type: 'other' as const }
        expect(created.sortTime).toBe(100)
        expect(imported.sortTime).toBe(100)
        expect(sortGalleryInlays([created, hidden, imported], { type: 'all', sort: 'newest', usage: 'all', offset: 0, limit: 20 }).map(value => value.key)).toEqual(['a', 'b'])
    })
    it('carries content identity through native pages and invalidates hashless metadata by revision', () => {
        const metadata = { key:'stable', kind:'inlay' as const, inlayType:'image' as const, name:'photo.png', ext:'png', mime:'image/png', size:5 }
        const hash = 'a'.repeat(64)
        expect(galleryInlayItem({...metadata,objectHash:hash}, 10)).toMatchObject({objectHash:hash,contentRevision:undefined})
        expect(galleryInlayItem({...metadata,objectHash:hash,name:'renamed.png'}, 11).contentRevision).toBeUndefined()
        expect(galleryInlayItem(metadata, 10).contentRevision).toBe(10)
        expect(galleryInlayItem(metadata, 11).contentRevision).toBe(11)
    })
    it('replaces a distant metadata window without retaining prior items', () => {
        let window = galleryWindow([item('a'), item('b')], 0)
        window = galleryWindow([item('last')], 100_000)
        expect([...window.entries()]).toEqual([[100_000, item('last')]])
    })
    it('flushes confirmation edits, rejects the old revision, rescans and keeps only still unused attachments', async () => {
        let revision = 1
        let scanned = 1
        const stored = new Set(['safe', 'new-reference', 'unknown'])
        const selected = new Set(stored)
        const dialogs: { keys: string[]; dropped: number }[] = []
        let pendingEdit = false
        let flushes = 0
        const ports: UnusedDeletePorts = {
            async flush() { flushes++; if (pendingEdit) { revision++; pendingEdit = false } },
            async revision() { return revision },
            scanRevision() { return scanned },
            async rescan() { scanned = revision; return true },
            async unusedKeys() { return scanned === 1 ? [...stored] : ['safe'] },
            async confirm(keys, dropped) { dialogs.push({ keys, dropped }); if (dialogs.length === 1) pendingEdit = true; return true },
            async remove(keys, expected) { if (revision !== expected) throw new Error('conflict'); for (const key of keys) stored.delete(key) },
            allDropped() {},
            selection(keys) { selected.clear(); for (const key of keys) selected.add(key) },
            isConflict(error) { return error instanceof Error && error.message === 'conflict' },
        }
        await deleteUnusedGallerySelection(selected, ports)
        expect([...stored]).toEqual(['new-reference', 'unknown'])
        expect(dialogs).toEqual([{ keys: ['safe', 'new-reference', 'unknown'], dropped: 0 }, { keys: ['safe'], dropped: 2 }])
        expect(flushes).toBe(4)
        expect(selected.size).toBe(0)
    })
    it('does not confirm or delete when the recheck leaves only unknown attachments', async () => {
        let selected = new Set(['unknown'])
        let removed = false
        let confirmed = false
        let feedback = 0
        await deleteUnusedGallerySelection(selected, {
            async flush() {}, async revision() { return 2 }, scanRevision() { return 1 }, async rescan() { return true },
            async unusedKeys() { return [] }, async confirm() { confirmed = true; return true }, async remove() { removed = true },
            allDropped(count) { feedback = count },
            selection(keys) { selected = keys }, isConflict() { return false },
        })
        expect(selected.size).toBe(0)
        expect(feedback).toBe(1)
        expect(confirmed).toBe(false)
        expect(removed).toBe(false)
    })
})

describe('unused attachment deletion across store changes', () => {
    it('flushes an already queued reference before deciding whether to rescan', async () => {
        let revision = 1
        let scanned = 1
        let pending = true
        let selected = new Set(['referenced'])
        const stored = new Set(selected)
        await deleteUnusedGallerySelection(selected, {
            async flush() { if (pending) { revision++; pending = false } },
            async revision() { return revision }, scanRevision() { return scanned },
            async rescan() { scanned = revision; return true },
            async unusedKeys() { return scanned === 1 ? ['referenced'] : [] },
            async confirm() { return true },
            async remove(keys, expected) { if (expected !== revision) throw new Error('conflict'); for (const key of keys) stored.delete(key) },
            allDropped() {},
            selection(keys) { selected = keys },
            isConflict(error) { return error instanceof Error && error.message === 'conflict' },
        })
        expect([...stored]).toEqual(['referenced'])
        expect(selected.size).toBe(0)
    })
    it('rechecks when sync adds a reference immediately before the atomic delete', async () => {
        let revision = 1
        let scanned = 1
        let syncPending = true
        let selected = new Set(['sync-reference', 'safe'])
        const stored = new Set(selected)
        const confirmations: string[][] = []
        await deleteUnusedGallerySelection(selected, {
            async flush() {}, async revision() { return revision }, scanRevision() { return scanned },
            async rescan() { scanned = revision; return true },
            async unusedKeys() { return scanned === 1 ? ['sync-reference', 'safe'] : ['safe'] },
            async confirm(keys) { confirmations.push(keys); return true },
            async remove(keys, expected) {
                if (syncPending) { revision++; syncPending = false }
                if (expected !== revision) throw new Error('conflict')
                for (const key of keys) stored.delete(key)
            },
            allDropped() {},
            selection(keys) { selected = keys }, isConflict(error) { return error instanceof Error && error.message === 'conflict' },
        })
        expect([...stored]).toEqual(['sync-reference'])
        expect(confirmations).toEqual([['sync-reference', 'safe'], ['safe']])
        expect(selected.size).toBe(0)
    })
    it('rescans after a refused delete even when the revision did not move', async () => {
        let scans = 1
        let selected = new Set(['device-reference', 'safe'])
        const stored = new Set(selected)
        const confirmations: string[][] = []
        await deleteUnusedGallerySelection(selected, {
            async flush() {}, async revision() { return 1 }, scanRevision() { return 1 },
            async rescan() { scans++; return true },
            async unusedKeys() { return scans === 1 ? ['device-reference', 'safe'] : ['safe'] },
            async confirm(keys) { confirmations.push(keys); return confirmations.length < 4 },
            async remove(keys) {
                if (scans === 1) throw new Error('conflict')
                for (const key of keys) stored.delete(key)
            },
            allDropped() {},
            selection(keys) { selected = keys }, isConflict(error) { return error instanceof Error && error.message === 'conflict' },
        })
        expect([...stored]).toEqual(['device-reference'])
        expect(confirmations).toEqual([['device-reference', 'safe'], ['safe']])
    })
})

it('resolves origin IDs with the same per-field string precedence as compatible export', () => {
    const pocketRisu = { charId: 'pocket-character', chatId: 'pocket-chat' }
    expect(galleryOriginIds({ charId: 'nest-character', pocketRisu })).toEqual({ characterId: 'nest-character', conversationId: 'pocket-chat' })
    expect(galleryOriginIds({ charId: 42, chatId: {}, pocketRisu })).toEqual({ characterId: 'pocket-character', conversationId: 'pocket-chat' })
    expect(galleryOriginIds({ charId: '', pocketRisu })).toBeNull()
    expect(galleryOriginIds({ chatId: '', pocketRisu })).toBeNull()
    expect(galleryOriginIds(undefined)).toBeNull()
})
