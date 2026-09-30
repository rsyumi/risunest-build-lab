import { describe, expect, it, vi } from 'vitest'
import { createNativeOfficialAccountAssets } from './nativeOfficialAccountAssets'
import type { BlobStore } from '../blobStore'
import type { PersistentDataStore } from '../persistentDataStore'
import type { NativeDeviceSettings } from '../nativeDeviceSettings'
import type { AccountStorage } from '../accountStorage'

vi.mock('./officialAccountSnapshot', () => ({
    collectPinnedReferences: async () => ({ assets: ['assets/avatar', 'assets/emotion', 'assets/background', 'assets/missing'] }),
}))
vi.mock('./nativeOfficialAccountFlow', () => ({ nativeOfficialAccountKeys: { pendingAssets: 'pending' } }))
vi.mock('../persistentRecordIterator', () => ({
    withPersistentRevisionLease: async (lease: { release(): void }, work: (reader: unknown) => Promise<unknown>) => {
        try { return await work(lease) } finally { lease.release() }
    },
}))

function harness() {
    const values = new Map<string, Uint8Array>()
    let marker: unknown = null
    const settings = { get: vi.fn(async () => marker), set: vi.fn(async (_key, value) => { marker = value }) } as unknown as NativeDeviceSettings
    const lease = { release: vi.fn() }
    const store = { readRoot: async () => ({ revision: 7 }), acquireRevision: async () => lease } as unknown as PersistentDataStore
    const blobs = {
        stat: vi.fn(async key => values.has(key) ? {} : null),
        read: vi.fn(async key => values.get(key) ?? null),
        put: vi.fn(async (key, bytes) => { values.set(key, bytes.slice()) }),
        resolveUrl: vi.fn(async key => values.has(key) ? `asset://${key}` : null),
    } as unknown as BlobStore
    const readItem = vi.fn(async key => key === 'assets/missing'
        ? { kind: 'missing' as const } : { kind: 'value' as const, bytes: Uint8Array.of(1, 2, 3) })
    const ledger = { record: vi.fn(), publishedAs: vi.fn(), reset: vi.fn(), clear: vi.fn() }
    const dependencies = { settings, store, account: { readItem } as Pick<AccountStorage, 'readItem'>, ledger, resolveBlobs: async () => blobs, flushMetadata: vi.fn(async () => {}) }
    return { values, settings, lease, blobs, readItem, ledger, dependencies, assets: createNativeOfficialAccountAssets(dependencies) }
}

describe('native restored account assets', () => {
    it('materializes avatar, emotion and background bytes and reports only a missing count', async () => {
        const h = harness()
        await h.assets.prepare('account')
        expect(await h.assets.complete('account')).toBe(1)
        expect(h.values.size).toBe(3)
        for (const key of ['assets/avatar', 'assets/emotion', 'assets/background']) {
            expect(await h.blobs.read(key)).toEqual(Uint8Array.of(1, 2, 3))
            expect(await h.blobs.resolveUrl(key)).toBe(`asset://${key}`)
            expect(h.ledger.record).toHaveBeenCalledWith(key, key)
        }
        expect(h.lease.release).toHaveBeenCalledOnce()
        expect(await h.settings.get('pending')).toEqual({ accountId: 'account' })
    })

    it('resumes after interruption and isolates a different signed-in account', async () => {
        const h = harness()
        await h.assets.prepare('account')
        h.readItem.mockRejectedValueOnce(new Error('offline'))
        await expect(h.assets.complete('account')).rejects.toThrow('offline')
        const resumed = createNativeOfficialAccountAssets(h.dependencies)
        await expect(resumed.complete('other')).resolves.toBe(0)
        expect(h.readItem).toHaveBeenCalledOnce()
        h.readItem.mockImplementation(async () => ({ kind: 'value', bytes: Uint8Array.of(4) }))
        await expect(resumed.complete('account')).resolves.toBe(0)
        expect(await h.settings.get('pending')).toBeNull()
        expect(h.values.size).toBe(4)
    })

    it('retains the resume marker when stored bytes fail verification', async () => {
        const h = harness()
        await h.assets.prepare('account')
        vi.mocked(h.blobs.read).mockResolvedValue(Uint8Array.of(9))
        await expect(h.assets.complete('account')).rejects.toThrow('verification failed')
        expect(h.ledger.record).not.toHaveBeenCalled()
        expect(await h.settings.get('pending')).toEqual({ accountId: 'account' })
    })
})
