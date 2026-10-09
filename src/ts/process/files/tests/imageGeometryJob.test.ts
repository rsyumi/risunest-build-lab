import { expect, it, vi } from 'vitest'
import { calculateMissingImageGeometry } from '../imageGeometryJob'
import type { AssetAlias } from '../../../storage/persistentDataStore'
import type { ImageGeometry } from '../../../storage/imageGeometry'

const hash = (index: number) => index.toString(16).padStart(64, '0')
const alias = (index: number): AssetAlias => ({ kind: 'asset', key: `assets/${index}.png`, objectHash: hash(index), mime: 'image/png', ext: 'png', size: 10, name: 'synthetic' })
function fixture(items: AssetAlias[]) {
    const records = new Map<string, ImageGeometry>()
    let cancel = false
    let revision = 1
    const deps = {
        list: vi.fn(async ({ cursor, limit }: { cursor?: string; limit: number }) => {
            const start = Number(cursor ?? 0)
            return { revision, items: items.slice(start, start + limit), nextCursor: start + limit < items.length ? String(start + limit) : undefined }
        }),
        geometry: {
            read: vi.fn(async (hashes: readonly string[]) => hashes.flatMap(key => records.has(key) ? [records.get(key)!] : [])),
            write: vi.fn(async (values: readonly ImageGeometry[]) => { for (const value of values) records.set(value.contentHash, value) }),
            compute: vi.fn(async (contentHash: string): Promise<ImageGeometry | null> => ({ contentHash, width: 7, height: 3 })),
        },
        isCancelled: () => cancel,
        onProgress: vi.fn(),
    }
    return { deps, records, cancel: () => { cancel = true }, change: () => { revision++ } }
}

it('pages, deduplicates, persists only missing hashes and skips them on rerun', async () => {
    const f = fixture([...Array.from({ length: 150 }, (_, i) => alias(i)), alias(2)])
    const first = await calculateMissingImageGeometry(f.deps)
    expect(first).toEqual({ saved: 150, skipped: 0, failed: 0, cancelled: false, catalogChanged: false })
    expect(f.deps.list.mock.calls.every(([query]) => query.limit <= 64)).toBe(true)
    expect(f.deps.geometry.write.mock.calls.every(([values]) => values.length <= 16)).toBe(true)
    const second = await calculateMissingImageGeometry(f.deps)
    expect(second).toMatchObject({ saved: 0, skipped: 150 })
    expect(f.deps.geometry.compute).toHaveBeenCalledTimes(150)
})

it('keeps committed batches on cancellation without counting unfinished computation', async () => {
    const f = fixture(Array.from({ length: 70 }, (_, i) => alias(i)))
    f.deps.geometry.compute.mockImplementation(async contentHash => {
        if (contentHash === hash(18)) f.cancel()
        return { contentHash, width: 7, height: 3 }
    })
    expect(await calculateMissingImageGeometry(f.deps)).toMatchObject({ saved: 16, skipped: 0, failed: 0, cancelled: true })
    expect(f.records.size).toBe(16)
})

it('counts unsupported/nonlocal images as skipped and failed storage as failed', async () => {
    const f = fixture([alias(1), alias(2), alias(3)])
    f.deps.geometry.compute.mockResolvedValueOnce(null).mockRejectedValueOnce(new Error('bad header'))
    f.deps.geometry.write.mockRejectedValueOnce(new Error('disk full'))
    expect(await calculateMissingImageGeometry(f.deps)).toMatchObject({ saved: 0, skipped: 1, failed: 2 })
    expect(f.records.size).toBe(0)
})

it('stops a changed catalog with partial committed results', async () => {
    const f = fixture(Array.from({ length: 65 }, (_, i) => alias(i)))
    f.deps.onProgress.mockImplementation(value => { if (value.saved === 64) f.change() })
    expect(await calculateMissingImageGeometry(f.deps)).toMatchObject({ saved: 64, catalogChanged: true })
    expect(f.deps.geometry.compute).toHaveBeenCalledTimes(64)
})

it('reuses valid final inlay dimensions without decoding bytes', async () => {
    const f = fixture([{ ...alias(1), kind: 'inlay', key: 'inlay', inlayType: 'image', width: 30, height: 40 }])
    expect(await calculateMissingImageGeometry(f.deps)).toMatchObject({ saved: 1 })
    expect(f.deps.geometry.compute).not.toHaveBeenCalled()
    expect(f.records.get(hash(1))).toMatchObject({ width: 30, height: 40 })
})
