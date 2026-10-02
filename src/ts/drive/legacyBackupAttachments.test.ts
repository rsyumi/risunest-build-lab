import { describe, expect, it, vi } from 'vitest'
import type { BlobStore, BlobWriteMetadata } from '../storage/blobStore'
import { createLegacyBackupAttachments } from './legacyBackupAttachments'

vi.mock('localforage', () => ({ default: {
    INDEXEDDB: 'indexeddb',
    createInstance: () => {
        const values = new Map<string, unknown>()
        return {
            setItem: async (key: string, value: unknown) => { values.set(key, value) },
            getItem: async (key: string) => values.get(key) ?? null,
            dropInstance: async () => { values.clear() },
        }
    },
} }))

const metadata: BlobWriteMetadata = { kind: 'asset', mime: 'application/octet-stream', name: 'synthetic', ext: 'bin' }

function fixture(bytes: number[]) {
    const data = new Uint8Array(bytes)
    const store = {
        stat: vi.fn(async () => ({ ...metadata, key: 'assets/synthetic.bin', size: data.byteLength })),
        read: vi.fn(async () => data),
        put: vi.fn(async (key: string, body: Uint8Array) => ({ ...metadata, key, size: body.byteLength })),
        remove: vi.fn(),
    } as unknown as BlobStore
    return { store, attachments: createLegacyBackupAttachments(store) }
}

describe('hashless upstream backup attachment adoption', () => {
    it('verifies equal content and reuses the destination without rewriting it', async () => {
        const { store, attachments } = fixture([1, 2, 3])
        await attachments.put('assets/synthetic.bin', new Uint8Array([1, 2, 3]), metadata)
        await attachments.activate(() => undefined)
        expect(store.read).toHaveBeenCalledOnce()
        expect(store.put).not.toHaveBeenCalled()
        expect(store.remove).not.toHaveBeenCalled()
        await attachments.dispose()
    })

    it('never reuses equal logical names and sizes with different content', async () => {
        const { store, attachments } = fixture([1, 2, 3])
        await attachments.put('assets/synthetic.bin', new Uint8Array([3, 2, 1]), metadata)
        await attachments.activate(() => undefined)
        expect(store.put).toHaveBeenCalledWith('assets/synthetic.bin', new Uint8Array([3, 2, 1]), metadata)
        expect(store.remove).not.toHaveBeenCalled()
        await attachments.dispose()
    })

    it('reports adoption failure without reverting already committed library attachments', async () => {
        const { store, attachments } = fixture([1, 2, 3])
        vi.mocked(store.put).mockRejectedValue(new Error('synthetic write failure'))
        await attachments.put('assets/synthetic.bin', new Uint8Array([3, 2, 1]), metadata)
        await expect(attachments.activate(() => undefined)).rejects.toThrow('synthetic write failure')
        expect(store.put).toHaveBeenCalledOnce()
        expect(store.remove).not.toHaveBeenCalled()
    })
})
