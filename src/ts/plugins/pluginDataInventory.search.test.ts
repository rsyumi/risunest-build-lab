import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { PluginStorageValueQuery } from '../storage/persistentDataStore'

const mock = vi.hoisted(() => ({
    pages: vi.fn(), release: vi.fn(async () => {}),
}))
vi.mock('../platform', () => ({ isTauri: false }))
vi.mock('./plugins.svelte', () => ({ pluginStorageStore: { invalidate: vi.fn() } }))
vi.mock('../storage/persistentDataStoreFactory', () => ({
    getPersistentDataStore: () => ({
        readRoot: async () => ({ revision: 7 }),
        acquireRevision: async () => ({ revision: 7, readPluginStorageValues: mock.pages, release: mock.release }),
    }),
}))
import { searchPluginDataValues, pluginDataItemId, type PluginDataItem } from './pluginDataInventory'

const items: PluginDataItem[] = Array.from({ length: 2000 }, (_, index) => ({
    owner: 'owner-a', key: `key-${index}`, byteSize: 8192, valueType: 'string', automatic: false,
}))

beforeEach(() => { mock.pages.mockReset(); mock.release.mockClear() })

describe('bounded plugin value search', () => {
    it('retains matching identities instead of 2000 large values', async () => {
        mock.pages.mockImplementation(async ({ owner, afterKey }: PluginStorageValueQuery) => {
            const start = (afterKey?.ordinal ?? -1) + 1
            const selected = items.slice(start, start + 128)
            const end = start + selected.length
            return { revision: 7,
                items: selected.map((item) => ({ owner, key: item.key, value: `MATCH ${'x'.repeat(8000)}` })),
                nextCursor: end < items.length ? { owner, key: items[end - 1].key, ordinal: end - 1 } : null }
        })
        const result = await searchPluginDataValues(items, 'match', () => false)
        expect(result).toEqual(new Set(items.map(pluginDataItemId)))
        expect(mock.pages).toHaveBeenCalledTimes(16)
        expect(mock.pages.mock.calls.every(([query]) => query.owner === 'owner-a')).toBe(true)
        expect(new TextEncoder().encode(JSON.stringify([...result])).byteLength).toBeLessThan(100_000)
        expect(mock.release).toHaveBeenCalledOnce()
    })

    it('stops before the next page and releases the lease after cancellation', async () => {
        let cancelled = false
        mock.pages.mockImplementation(async () => {
            cancelled = true
            return { revision: 7, items: [{ owner: 'owner-a', key: 'key-0', value: 'match' }],
                nextCursor: { owner: 'owner-a', key: 'key-0', ordinal: 0 } }
        })
        expect(await searchPluginDataValues(items, 'match', () => cancelled)).toEqual(new Set())
        expect(mock.pages).toHaveBeenCalledOnce()
        expect(mock.release).toHaveBeenCalledOnce()
    })

    it('rejects mixed revisions and releases the lease', async () => {
        mock.pages.mockResolvedValue({ revision: 8, items: [], nextCursor: null })
        await expect(searchPluginDataValues(items, 'match', () => false)).rejects.toThrow('expected 7')
        expect(mock.release).toHaveBeenCalledOnce()
    })
})
