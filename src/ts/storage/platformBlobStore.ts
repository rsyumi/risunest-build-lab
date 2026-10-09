import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../platform'
import {
    createKeyValueBlobStore,
    type BlobKeyValueBackend,
    type BlobPhysicalKeyMapper,
    type BlobStore,
} from './blobStore'
import type { StorageMutationGate } from './storageMutationGate'
import { objectPhysicalKey } from './payloadCas'

function logicalKeyHex(key: string): string {
    return Buffer.from(key, 'utf-8').toString('hex')
}

export function physicalBlobKeys(logicalKey: string) {
    return {
        payload: logicalKey.startsWith('assets/')
            ? logicalKey
            : `blobstore/inlays/${logicalKeyHex(logicalKey)}.bin`,
        metadata: `blobstore/metadata/${logicalKeyHex(logicalKey)}.json`,
    }
}

export function createTauriNativeMediaUrl(
    physicalKey: string,
    baseUrl: string,
): string {
    if (!/^http:\/\/127\.0\.0\.1:[1-9][0-9]*\/[a-f0-9]{32}\/$/.test(baseUrl)) {
        throw new TypeError('Invalid native media endpoint')
    }
    return `${baseUrl}${logicalKeyHex(physicalKey)}`
}

export function createNativeMediaEndpointProvider(
    invokeCommand: (command: string) => Promise<unknown> = invoke,
    onChanged: () => void = () => {},
): (() => Promise<string>) & { ensure(): Promise<boolean> } {
    let pending: Promise<string> | undefined
    let current: string | undefined
    let recovery: Promise<boolean> | undefined
    const read = async (command: string): Promise<string> => {
        const value = await invokeCommand(command)
        if (typeof value !== 'string') throw new TypeError('Invalid native media endpoint')
        createTauriNativeMediaUrl('assets/validation', value)
        return value
    }
    const get = () => pending ??= read('native_media_base_url').then(value => {
        current = value
        return value
    }).catch(error => { pending = undefined; throw error })
    return Object.assign(get, {
        ensure: () => recovery ??= (async () => {
            await pending?.catch(() => {})
            const value = await read('native_media_ensure')
            const changed = current !== undefined && current !== value
            current = value
            pending = Promise.resolve(value)
            if (changed) onChanged()
            return changed
        })().finally(() => { recovery = undefined }),
    })
}

const nativeMediaEndpointListeners = new Set<() => void>()
export function subscribeNativeMediaEndpointChanges(listener: () => void): () => void {
    nativeMediaEndpointListeners.add(listener)
    return () => { nativeMediaEndpointListeners.delete(listener) }
}
export const getNativeMediaEndpoint = createNativeMediaEndpointProvider(invoke, () => {
    for (const listener of nativeMediaEndpointListeners) {
        try { listener() } catch (error) { console.error('Native media refresh failed', error) }
    }
})

export function createTauriCasObjectUrl(
    input: { contentHash: string; mime: string; size: number },
    baseUrl: string,
): string {
    if (!Number.isSafeInteger(input.size) || input.size < 0) {
        throw new RangeError(
            'CAS object size must be a nonnegative safe integer',
        )
    }
    if (input.mime.length === 0 || !/^[\x20-\x7e]+$/.test(input.mime)) {
        throw new TypeError('CAS object MIME must be a nonempty header value')
    }
    const query = new URLSearchParams({
        mime: input.mime,
        size: input.size.toString(),
    })
    return `${createTauriNativeMediaUrl(objectPhysicalKey(input.contentHash), baseUrl)}?${query}`
}

const blobKeyMapper: BlobPhysicalKeyMapper = {
    payload: (key) => physicalBlobKeys(key).payload,
    metadata: (key) => physicalBlobKeys(key).metadata,
    metadataPrefix: 'blobstore/metadata/',
}

export function createBackedBlobStore(backend: BlobKeyValueBackend): BlobStore {
    return createKeyValueBlobStore(backend, blobKeyMapper)
}

export function createGatedBlobStore(store: BlobStore, gate: StorageMutationGate): BlobStore {
    const gated: BlobStore = {
        async captureConditionalWrite(key) {
            const original = await gate.runKeyedWrite(key, async () => ({ metadata: await store.stat(key), bytes: await store.read(key) }))
            if (!original.metadata || !original.bytes) return null
            let used = false
            return async (data, metadata, sourceBytes) => {
                if (used) throw new Error('Conditional blob writer already used')
                used = true
                if (sourceBytes.length !== original.bytes!.length || sourceBytes.some((byte, index) => byte !== original.bytes![index])) return null
                const ownedData = data.slice()
                return gate.runKeyedWrite(key, async () => {
                    const current = await store.stat(key)
                    const bytes = await store.read(key)
                    if (!current || JSON.stringify(current) !== JSON.stringify(original.metadata)
                        || !bytes || bytes.length !== original.bytes!.length
                        || bytes.some((byte, index) => byte !== original.bytes![index])) return null
                    return store.put(key, ownedData, metadata)
                })
            }
        },
        async put(key, data, metadata) {
            const ownedData = data.slice()
            const ownedMetadata = { ...metadata }
            return gate.runKeyedWrite(key, () => store.put(key, ownedData, ownedMetadata))
        },
        read: (key, range) => store.read(key, range),
        stat: (key) => store.stat(key),
        list: (query) => store.list(query),
        remove: (key) => gate.runKeyedWrite(key, () => store.remove(key)),
        resolveUrl: (key) => store.resolveUrl(key),
        imageGeometry: store.imageGeometry,
        resolveImageSource: store.resolveImageSource?.bind(store),
    }
    if (store.putNewInlayImage) {
        gated.putNewInlayImage = (key, data, input) => {
            const ownedData = data.slice()
            const ownedInput = { ...input }
            return gate.runKeyedWrite(key, () => store.putNewInlayImage!(key, ownedData, ownedInput))
        }
    }
    return gated
}

export type KeyValueStorage = {
    readonly blobStorageKind?: 'opfs'
    setItem(key: string, value: Uint8Array): Promise<unknown>
    getItem(key: string): Promise<Uint8Array | null>
    keys(): Promise<string[]>
    removeItem(key: string): Promise<unknown>
}

export function createStorageBlobKeyValueBackend(storage: KeyValueStorage): BlobKeyValueBackend {
    return {
        async write(key, value) { await storage.setItem(key, value) },
        async read(key) {
            const value = await storage.getItem(key)
            return value ? new Uint8Array(value) : null
        },
        async keys() { return storage.keys() },
        async remove(key) { await storage.removeItem(key) },
    }
}

const opfsHexNamePattern = /^(?:[0-9a-f]{2})+$/

export function createOpfsBlobBackend(directory: FileSystemDirectoryHandle): BlobKeyValueBackend {
    const fileName = (key: string) => Buffer.from(key, 'utf-8').toString('hex')
    const readFileObject = async (key: string): Promise<File | null> => {
        try {
            return await (await directory.getFileHandle(fileName(key))).getFile()
        } catch (error) {
            if (error instanceof DOMException && error.name === 'NotFoundError') return null
            throw error
        }
    }
    return {
        async write(key, value) {
            const stream = await (await directory.getFileHandle(fileName(key), { create: true })).createWritable()
            try {
                await stream.write(value.slice().buffer as ArrayBuffer)
            } catch (error) {
                try {
                    await stream.abort(error)
                } catch (abortError) {
                    console.error('OPFS blob write abort failed', abortError)
                }
                throw error
            }
            await stream.close()
        },
        async read(key) {
            const file = await readFileObject(key)
            return file ? new Uint8Array(await file.arrayBuffer()) : null
        },
        async readRange(key, range) {
            const file = await readFileObject(key)
            if (!file) return null
            return new Uint8Array(await file.slice(range.start, range.endExclusive).arrayBuffer())
        },
        async size(key) {
            return (await readFileObject(key))?.size ?? null
        },
        async keys() {
            const keys: string[] = []
            for await (const entry of directory.values()) {
                if (entry.kind === 'directory' || !opfsHexNamePattern.test(entry.name)) continue
                keys.push(Buffer.from(entry.name, 'hex').toString('utf-8'))
            }
            return keys
        },
        async remove(key) {
            try {
                await directory.removeEntry(fileName(key))
            } catch (error) {
                if (!(error instanceof DOMException) || error.name !== 'NotFoundError') throw error
            }
        },
    }
}

let productionStore: Promise<BlobStore> | undefined
let productionBackend: Promise<BlobKeyValueBackend> | undefined
let storageProvider: () => Promise<KeyValueStorage | null> = async () => null

export function configureBlobStoreStorageProvider(provider: () => Promise<KeyValueStorage | null>): void {
    storageProvider = provider
    productionStore = undefined
    productionBackend = undefined
}

export function createStorageBlobStore(
    selected: { storage: KeyValueStorage; isAccount: boolean },
): BlobStore {
    if (selected.isAccount) throw new TypeError('AccountStorage cannot be used as a BlobStore backend')
    return createBackedBlobStore(createStorageBlobKeyValueBackend(selected.storage))
}

export async function createBrowserBlobBackend(
    selected: KeyValueStorage | null,
    getOpfsDirectory: () => Promise<FileSystemDirectoryHandle> = () => navigator.storage.getDirectory(),
): Promise<BlobKeyValueBackend> {
    if (!selected) throw new Error('Blob storage provider is not configured')
    if (selected?.blobStorageKind === 'opfs') return createOpfsBlobBackend(await getOpfsDirectory())
    return createStorageBlobKeyValueBackend(selected)
}

export async function readBlobForFacade(
    store: BlobStore,
    key: string,
    rejectMissing: boolean,
): Promise<Uint8Array | null> {
    const value = await store.read(key)
    if (value === null && rejectMissing) throw new Error(`Missing asset: ${key}`)
    return value
}

async function createProductionBackend(): Promise<BlobKeyValueBackend> {
    if (isTauri) throw new Error('Native asset repository is not configured')
    return createBrowserBlobBackend(await storageProvider())
}

export function getPlatformBlobKeyValueBackend(): Promise<BlobKeyValueBackend> {
    return productionBackend ??= createProductionBackend()
}

async function createProductionStore(): Promise<BlobStore> {
    const backend = await getPlatformBlobKeyValueBackend()
    return createBackedBlobStore(backend)
}

/** Defers backend selection so callers can hold a store before storage is initialized. */
const deferredStore: BlobStore = {
    async put(key, data, metadata) { return (await openProductionStore()).put(key, data, metadata) },
    async putNewInlayImage(key, data, input) {
        const store = await openProductionStore()
        if (!store.putNewInlayImage) throw new Error('Native Inlay image writer is unavailable')
        return store.putNewInlayImage(key, data, input)
    },
    async read(key, range) { return (await openProductionStore()).read(key, range) },
    async stat(key) { return (await openProductionStore()).stat(key) },
    async list(query) { return (await openProductionStore()).list(query) },
    async remove(key) { return (await openProductionStore()).remove(key) },
    async resolveUrl(key) { return (await openProductionStore()).resolveUrl(key) },
}

function openProductionStore(): Promise<BlobStore> {
    return productionStore ??= createProductionStore()
}

let gatedProductionStore: BlobStore | null = null

export function configureActiveBlobStore(
    gate: StorageMutationGate,
    authoritative: BlobStore = deferredStore,
    options: { alreadyGuarded?: boolean } = {},
): void {
    gatedProductionStore = options.alreadyGuarded
        ? authoritative
        : createGatedBlobStore(authoritative, gate)
}

export function getBlobStore(): BlobStore {
    return gatedProductionStore ?? deferredStore
}

export async function resolveBlobStore(): Promise<BlobStore> {
    return getBlobStore()
}
