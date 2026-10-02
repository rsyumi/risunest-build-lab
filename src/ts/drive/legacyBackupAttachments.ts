import localforage from 'localforage'
import type { BlobMetadata, BlobStore, BlobWriteMetadata } from '../storage/blobStore'

type Attachment = { data: Uint8Array, metadata: BlobWriteMetadata }

async function contentHash(data: Uint8Array): Promise<string> {
    const digest = await crypto.subtle.digest('SHA-256', new Uint8Array(data).buffer);
    return Array.from(new Uint8Array(digest), value => value.toString(16).padStart(2, '0')).join('');
}

export function createLegacyBackupAttachments(store: BlobStore) {
    const staging = localforage.createInstance({
        name: `legacy-restore-${crypto.randomUUID()}`,
        driver: localforage.INDEXEDDB,
    })
    const keys = new Set<string>()
    return {
        async put(key: string, data: Uint8Array, metadata: BlobWriteMetadata): Promise<BlobMetadata> {
            await staging.setItem(`incoming:${key}`, { data, metadata })
            keys.add(key)
            return { ...metadata, key, size: data.byteLength } as BlobMetadata
        },
        async activate(checkCancelled: () => void) {
            for (const key of keys) {
                checkCancelled()
                const incoming = await staging.getItem<Attachment>(`incoming:${key}`)
                if (!incoming) throw new Error('Missing staged backup attachment')
                const incomingHash = await contentHash(incoming.data)
                const metadata = await store.stat(key)
                if (metadata?.size === incoming.data.byteLength) {
                    const data = await store.read(key)
                    if (data === null) throw new Error('Existing attachment body is unavailable')
                    if (await contentHash(data) === incomingHash) continue
                }
                checkCancelled()
                await store.put(key, incoming.data, incoming.metadata)
            }
            checkCancelled()
        },
        dispose: async () => { await staging.dropInstance() },
    }
}
