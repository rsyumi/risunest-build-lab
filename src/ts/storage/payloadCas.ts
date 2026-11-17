import {
    validateBlobReadRange,
    type BlobKeyValueBackend,
    type BlobReadOptions,
    type BlobReadRange,
    type BlobStore,
} from './blobStore'

export interface ImmutablePayloadBackend {
    putIfAbsent(key: string, data: Uint8Array): Promise<boolean>
    read(key: string): Promise<Uint8Array | null>
    readRange?(key: string, range: BlobReadRange): Promise<Uint8Array | null>
    stat(key: string): Promise<number | null>
}

export interface PayloadKeyLock {
    /** All adapters for one backend must share this lock. Process-local locks do not coordinate tabs or workers. */
    readonly scope: 'process-local' | 'cross-context'
    withExclusive<T>(key: string, operation: () => Promise<T>): Promise<T>
}

export interface PreparedImmutablePayload {
    contentHash: string
    byteSize: number
    physicalKey: string
    deduplicated: boolean
}

/** A stored object whose size differs from the size its reader expected. */
export class PayloadSizeMismatchError extends Error {
    constructor() {
        super('Stored payload size differs from its expected size')
        this.name = 'PayloadSizeMismatchError'
    }
}

export interface ImmutablePayloadCas {
    prepare(data: Uint8Array): Promise<PreparedImmutablePayload>
    /** Throws `PayloadSizeMismatchError` when a stored object is not `expectedSize` bytes. */
    readObject(contentHash: string, expectedSize?: number, options?: BlobReadOptions): Promise<Uint8Array | null>
    readObjectRange(
        contentHash: string,
        range: BlobReadRange,
        expectedSize?: number,
        options?: BlobReadOptions,
    ): Promise<Uint8Array | null>
    statObject(contentHash: string): Promise<number | null>
    statObjects?(contentHashes: readonly string[]): Promise<(number | null)[]>
}

export async function hashPayloadBytes(bytes: Uint8Array): Promise<string> {
    const digest = await globalThis.crypto.subtle.digest(
        'SHA-256',
        bytes.slice().buffer as ArrayBuffer,
    )
    return Buffer.from(digest).toString('hex')
}

export function objectPhysicalKey(contentHash: string): string {
    if (!/^[0-9a-f]{64}$/.test(contentHash)) {
        throw new TypeError('Content hash must be 64 lowercase hexadecimal characters')
    }
    return `assets/objects/${contentHash.slice(0, 2)}/${contentHash.slice(2)}`
}

async function verifyObject(
    backend: ImmutablePayloadBackend,
    physicalKey: string,
    contentHash: string,
    byteSize: number,
): Promise<void> {
    const storedSize = await backend.stat(physicalKey)
    const stored = storedSize === byteSize ? await backend.read(physicalKey) : null
    if (
        stored === null
        || stored.byteLength !== byteSize
        || await hashPayloadBytes(stored) !== contentHash
    ) {
        throw new Error(`Payload collision or corruption at ${physicalKey}`)
    }
}

export function createImmutablePayloadCas(backend: ImmutablePayloadBackend): ImmutablePayloadCas {
    return {
        async prepare(data) {
            const ownedData = data.slice()
            const contentHash = await hashPayloadBytes(ownedData)
            const physicalKey = objectPhysicalKey(contentHash)
            const created = await backend.putIfAbsent(physicalKey, ownedData)
            await verifyObject(backend, physicalKey, contentHash, ownedData.byteLength)
            return {
                contentHash,
                byteSize: ownedData.byteLength,
                physicalKey,
                deduplicated: !created,
            }
        },
        async readObject(contentHash, expectedSize) {
            const data = await backend.read(objectPhysicalKey(contentHash))
            if (data !== null && expectedSize !== undefined && data.byteLength !== expectedSize) {
                throw new PayloadSizeMismatchError()
            }
            return data
        },
        async readObjectRange(contentHash, range, expectedSize) {
            validateBlobReadRange(range)
            const physicalKey = objectPhysicalKey(contentHash)
            if (expectedSize !== undefined) {
                const size = await backend.stat(physicalKey)
                if (size === null) return null
                if (size !== expectedSize) throw new PayloadSizeMismatchError()
            }
            if (backend.readRange) return backend.readRange(physicalKey, range)
            const data = await backend.read(physicalKey)
            return data?.slice(range.start, range.endExclusive) ?? null
        },
        async statObject(contentHash) {
            return backend.stat(objectPhysicalKey(contentHash))
        },
    }
}

export function createBlobKeyValuePayloadBackend(
    backend: BlobKeyValueBackend,
    lock: PayloadKeyLock,
): ImmutablePayloadBackend {
    const exists = async (key: string): Promise<boolean> => {
        if (backend.size) return await backend.size(key) !== null
        return await backend.read(key) !== null
    }
    return {
        async putIfAbsent(key, data) {
            return lock.withExclusive(key, async () => {
                if (await exists(key)) return false
                await backend.write(key, data.slice())
                return true
            })
        },
        read: (key) => backend.read(key),
        async readRange(key, range) {
            if (backend.readRange) return backend.readRange(key, range)
            const data = await backend.read(key)
            return data?.slice(range.start, range.endExclusive) ?? null
        },
        async stat(key) {
            if (backend.size) return backend.size(key)
            return (await backend.read(key))?.byteLength ?? null
        },
    }
}

export function createProcessLocalPayloadKeyLock(): PayloadKeyLock {
    const tails = new Map<string, Promise<void>>()
    return {
        scope: 'process-local',
        async withExclusive(key, operation) {
            const previous = tails.get(key) ?? Promise.resolve()
            let release = (): void => {}
            const current = new Promise<void>((resolve) => {
                release = resolve
            })
            tails.set(key, current)
            await previous
            try {
                return await operation()
            } finally {
                release()
                if (tails.get(key) === current) tails.delete(key)
            }
        },
    }
}

export function createShadowCopyBlobStore(
    authoritative: BlobStore,
    cas: ImmutablePayloadCas,
    observe?: (prepared: PreparedImmutablePayload) => void | Promise<void>,
): BlobStore {
    return {
        async put(key, data, metadata) {
            const ownedData = data.slice()
            const prepared = await cas.prepare(ownedData)
            await observe?.(prepared)
            return authoritative.put(key, ownedData, metadata)
        },
        read: (key, range) => authoritative.read(key, range),
        stat: (key) => authoritative.stat(key),
        list: (query) => authoritative.list(query),
        remove: (key) => authoritative.remove(key),
        resolveUrl: (key) => authoritative.resolveUrl(key),
    }
}
