import {
    inferBlobMime,
    validateBlobReadRange,
    type BlobReadRange,
    type BlobMetadata,
    type BlobStore,
    type BlobWriteMetadata,
    type InlayEncodeOptions,
    type InlayBlobMetadata,
} from './blobStore'
import { validImageDimensions } from './imageGeometry'
import {
    hashPayloadBytes,
    PayloadSizeMismatchError,
    type ImmutablePayloadCas,
    type PreparedImmutablePayload,
} from './payloadCas'
import {
    readCurrentRevision,
    RevisionConflictError,
    validateAssetAlias,
    validateAssetAliasIdentity,
} from './persistentDataStore'
import type {
    AssetAlias,
    AssetAliasIdentity,
    AssetAliasKind,
    AssetAliasListQuery,
    AssetAliasPage,
    DataRevision,
    PersistentDataStore,
    Versioned,
} from './persistentDataStore'

export type AssetAliasPayloadSource = 'cas' | 'remote' | 'missing'

/** Remote availability is distinct from physical CAS existence. */
export interface RemoteAssetReader {
    statObject(hash: string): Promise<number | null>
    statObjects?(hashes: readonly string[]): Promise<(number | null)[]>
    readObject(hash: string, range?: BlobReadRange): Promise<Uint8Array | null>
}

export interface AssetAliasRead {
    alias: AssetAlias
    data: Uint8Array | null
    source: AssetAliasPayloadSource
}

export interface AssetAliasStat {
    alias: AssetAlias
    objectSize: number | null
    source: AssetAliasPayloadSource
}

export interface AssetAliasCatalog {
    readAssetAlias(identity: AssetAliasIdentity): Promise<Versioned<AssetAlias> | null>
    listAssetAliases(query: AssetAliasListQuery): Promise<AssetAliasPage>
    deleteAssetAlias(
        identity: AssetAliasIdentity,
        expectedRevision: DataRevision,
    ): Promise<{ revision: DataRevision }>
}

interface TypedAssetRepositoryReader {
    read(
        identity: AssetAliasIdentity,
        range?: BlobReadRange,
    ): Promise<Versioned<AssetAliasRead> | null>
    stat(identity: AssetAliasIdentity): Promise<Versioned<AssetAliasStat> | null>
}

interface TypedAssetRepositoryReaderOptions {
    remote?: RemoteAssetReader
    reader: Pick<AssetAliasCatalog, 'readAssetAlias'>
    cas: ImmutablePayloadCas
}

function aliasBlobMetadata(alias: AssetAlias): BlobMetadata {
    if (alias.kind === 'asset') {
        return {
            key: alias.key,
            kind: 'asset',
            size: alias.size,
            mime: alias.mime,
            name: alias.name,
            ext: alias.ext,
        }
    }
    if (alias.inlayType === undefined) {
        throw new Error(`Inlay asset alias is missing inlayType for ${alias.key}`)
    }
    return {
        key: alias.key,
        kind: 'inlay',
        size: alias.size,
        mime: alias.mime,
        name: alias.name,
        ext: alias.ext,
        inlayType: alias.inlayType,
        ...(alias.metadata === undefined ? {} : { metadata: alias.metadata }),
        ...(alias.width === undefined ? {} : { width: alias.width }),
        ...(alias.height === undefined ? {} : { height: alias.height }),
    }
}

function createTypedAssetRepositoryReader(
    options: TypedAssetRepositoryReaderOptions,
): TypedAssetRepositoryReader {
    const { reader, cas } = options
    return {
        async read(identity, range) {
            validateAssetAliasIdentity(identity)
            const { key } = identity
            if (range) validateBlobReadRange(range)
            const versioned = await reader.readAssetAlias(identity)
            if (!versioned) return null
            const alias = versioned.value
            validateAssetAlias(alias)
            if (alias.kind !== identity.kind || alias.key !== identity.key) {
                throw new TypeError('Asset alias does not match its requested identity')
            }
            if (alias.objectHash !== null) {
                const mismatch = (error: unknown): never => {
                    if (error instanceof PayloadSizeMismatchError) {
                        throw new Error(`Asset alias size mismatch for ${key}`)
                    }
                    throw error
                }
                if (range) {
                    const data = await cas.readObjectRange(alias.objectHash, range, alias.size)
                        .catch(mismatch)
                    if (data !== null) {
                        const expectedSize = Math.max(
                            0,
                            Math.min(range.endExclusive, alias.size)
                            - Math.min(range.start, alias.size),
                        )
                        if (data.byteLength !== expectedSize) {
                            throw new Error(`Asset alias range size mismatch for ${key}`)
                        }
                        return {
                            revision: versioned.revision,
                            value: { alias, data, source: 'cas' },
                        }
                    }
                } else {
                    const data = await cas.readObject(alias.objectHash, alias.size).catch(mismatch)
                    if (data !== null) {
                        if (data.byteLength !== alias.size) {
                            throw new Error(`Asset alias size mismatch for ${key}`)
                        }
                        return {
                            revision: versioned.revision,
                            value: { alias, data, source: 'cas' },
                        }
                    }
                }
            }
            if (alias.objectHash !== null && options.remote) {
                const data = await options.remote.readObject(alias.objectHash, range)
                    .catch((error: unknown) => {
                        if (error instanceof PayloadSizeMismatchError) {
                            throw new Error(`Remote asset identity mismatch for ${key}`)
                        }
                        throw error
                    })
                if (data !== null) {
                    const expectedSize = range
                        ? Math.max(
                              0,
                              Math.min(range.endExclusive, alias.size) -
                                  Math.min(range.start, alias.size),
                          )
                        : alias.size
                    if (
                        data.byteLength !== expectedSize ||
                        (!range && (await hashPayloadBytes(data)) !== alias.objectHash)
                    ) {
                        throw new Error(`Remote asset identity mismatch for ${key}`)
                    }
                    return {
                        revision: versioned.revision,
                        value: { alias, data, source: 'remote' },
                    }
                }
            }
            return {
                revision: versioned.revision,
                value: { alias, data: null, source: 'missing' },
            }
        },
        async stat(identity) {
            validateAssetAliasIdentity(identity)
            const { key } = identity
            const versioned = await reader.readAssetAlias(identity)
            if (!versioned) return null
            const alias = versioned.value
            validateAssetAlias(alias)
            if (alias.kind !== identity.kind || alias.key !== identity.key) {
                throw new TypeError('Asset alias does not match its requested identity')
            }
            if (alias.objectHash !== null) {
                const objectSize = await cas.statObject(alias.objectHash)
                if (objectSize !== null) {
                    if (objectSize !== alias.size) {
                        throw new Error(`Asset alias size mismatch for ${key}`)
                    }
                    return {
                        revision: versioned.revision,
                        value: { alias, objectSize, source: 'cas' },
                    }
                }
            }
            if (alias.objectHash !== null && options.remote) {
                const objectSize = await options.remote.statObject(alias.objectHash)
                if (objectSize !== null) {
                    if (objectSize !== alias.size)
                        throw new Error(`Remote asset size mismatch for ${key}`)
                    return {
                        revision: versioned.revision,
                        value: { alias, objectSize, source: 'remote' },
                    }
                }
            }
            return {
                revision: versioned.revision,
                value: { alias, objectSize: null, source: 'missing' },
            }
        },
    }
}

export interface CompleteAssetAliasStore extends AssetAliasCatalog,
    Pick<PersistentDataStore, 'readRoot' | 'readRevision' | 'commitAssetAlias'> {
    readAssetAliasesByKeys?: PersistentDataStore['readAssetAliasesByKeys']
    readLibraryIdentity?: PersistentDataStore['readLibraryIdentity']
}

export interface AssetObjectUrlResolver {
    resolveObjectUrl(input: {
        contentHash: string
        mime: string
        size: number
    }): Promise<string | null>
}

export interface NewInlayImageEncoding {
    data: Uint8Array
    metadata: Omit<InlayBlobMetadata, 'key' | 'size'>
}

export interface NewInlayImageEncoder {
    encodeNewInlayImage(
        key: string,
        data: Uint8Array,
        input: { name: string, options?: InlayEncodeOptions, metadata?: Record<string, unknown> },
    ): Promise<NewInlayImageEncoding>
}

export type DurableAssetWriteReleaseOutcome = 'committed' | 'aborted'
export type DurableAssetWriteObjectRole = 'direct-object' | 'owner-manifest'

export interface DurableAssetWriteSession {
    prepare(
        data: Uint8Array,
        role?: DurableAssetWriteObjectRole,
    ): Promise<PreparedImmutablePayload>
    seal(): Promise<void>
    release(outcome: DurableAssetWriteReleaseOutcome): Promise<void>
}

export interface DurableAssetWriteSessionFactory {
    begin(): Promise<DurableAssetWriteSession>
}

export interface CompleteAssetRepositoryBlobStoreOptions {
    imageGeometry?: import('./imageGeometry').ImageGeometryStore
    remote?: RemoteAssetReader
    store: CompleteAssetAliasStore
    cas: ImmutablePayloadCas
    objectUrls: AssetObjectUrlResolver
    newInlayImages: NewInlayImageEncoder
    writeSessions?: DurableAssetWriteSessionFactory
    listPageSize?: number
}

export class ConditionalAssetWriteConflictError extends Error {
    constructor() { super('Asset source changed before conditional publication'); this.name = 'ConditionalAssetWriteConflictError' }
}

export class AssetAliasSizeMismatchError extends Error {
    constructor(readonly key: string) { super(`Asset alias size mismatch for ${key}`) }
}

export interface PreparedCompleteAssetWrite {
    activate(expectedAlias?: AssetAlias): Promise<BlobMetadata>
    abort(): Promise<void>
}

export type CompleteAssetRepositoryBlobStore = BlobStore &
    Required<Pick<BlobStore, 'putNewInlayImage'>> & {
        prepareOwnedPut(
            key: string,
            ownedData: Uint8Array,
            metadata: BlobWriteMetadata,
            verifyContent?: boolean,
        ): Promise<PreparedCompleteAssetWrite>
        prepareOwnedNewInlayImage(
            key: string,
            ownedData: Uint8Array,
            input: { name: string, options?: InlayEncodeOptions, metadata?: Record<string, unknown> },
        ): Promise<PreparedCompleteAssetWrite>
    }

export interface CompleteTypedAssetRepository {
    resolveImageSource(identity: AssetAliasIdentity): Promise<import('./blobStore').BlobImageSource | null>
    prepareOwnedPut(
        identity: AssetAliasIdentity,
        ownedData: Uint8Array,
        metadata: BlobWriteMetadata,
        verifyContent?: boolean,
    ): Promise<PreparedCompleteAssetWrite>
    prepareOwnedNewInlayImage(
        identity: AssetAliasIdentity,
        ownedData: Uint8Array,
        input: { name: string, options?: InlayEncodeOptions, metadata?: Record<string, unknown> },
    ): Promise<PreparedCompleteAssetWrite>
    put(
        identity: AssetAliasIdentity,
        data: Uint8Array,
        metadata: Parameters<BlobStore['put']>[2],
    ): Promise<BlobMetadata>
    putNewInlayImage(
        identity: AssetAliasIdentity,
        data: Uint8Array,
        input: { name: string, options?: InlayEncodeOptions, metadata?: Record<string, unknown> },
    ): Promise<InlayBlobMetadata>
    read(identity: AssetAliasIdentity, range?: BlobReadRange): Promise<Uint8Array | null>
    stat(identity: AssetAliasIdentity): Promise<BlobMetadata | null>
    list(query?: Parameters<BlobStore['list']>[0]): Promise<BlobMetadata[]>
    remove(identity: AssetAliasIdentity): Promise<void>
    resolveUrl(identity: AssetAliasIdentity): Promise<string | null>
}

function blobIdentity(key: string): AssetAliasIdentity {
    return {
        kind: /^assets[/\\]/.test(key) ? 'asset' : 'inlay',
        key,
    }
}

async function readValidatedAlias(
    store: Pick<AssetAliasCatalog, 'readAssetAlias'>,
    identity: AssetAliasIdentity,
): Promise<Versioned<AssetAlias> | null> {
    const versioned = await store.readAssetAlias(identity)
    if (!versioned) return null
    validateAssetAlias(versioned.value)
    if (
        versioned.value.kind !== identity.kind
        || versioned.value.key !== identity.key
    ) {
        throw new TypeError('Asset alias does not match its requested identity')
    }
    return versioned
}

export function createCompleteTypedAssetRepository(
    options: CompleteAssetRepositoryBlobStoreOptions,
): CompleteTypedAssetRepository {
    const pageSize = options.listPageSize ?? 512
    if (!Number.isSafeInteger(pageSize) || pageSize <= 0) {
        throw new RangeError('Asset alias list page size must be a positive safe integer')
    }
    const repository = createTypedAssetRepositoryReader({
        remote: options.remote,
        reader: options.store,
        cas: options.cas,
    })

    const preparePublish = async (
        identity: AssetAliasIdentity,
        ownedData: Uint8Array,
        metadata: Parameters<BlobStore['put']>[2],
        verifyContent = false,
    ): Promise<PreparedCompleteAssetWrite> => {
        validateAssetAliasIdentity(identity)
        if (metadata.kind !== identity.kind) {
            throw new TypeError('Asset alias metadata kind does not match its identity')
        }
        const pendingAlias = {
            ...metadata,
            key: identity.key,
            objectHash: null,
            size: ownedData.byteLength,
        } as AssetAlias
        validateAssetAlias(pendingAlias)
        const expectedLibrary = verifyContent ? await options.store.readLibraryIdentity?.() : undefined
        const session = await options.writeSessions?.begin()
        try {
            const prepared = session
                ? await session.prepare(ownedData)
                : await options.cas.prepare(ownedData)
            if (prepared.byteSize !== ownedData.byteLength) {
                throw new Error(`Prepared payload size mismatch for ${identity.key}`)
            }
            if (verifyContent && prepared.contentHash !== await hashPayloadBytes(ownedData)) {
                throw new Error(`Prepared payload integrity mismatch for ${identity.key}`)
            }
            const alias = { ...pendingAlias, objectHash: prepared.contentHash } as AssetAlias
            validateAssetAlias(alias)
            const releaseSession = async (outcome: DurableAssetWriteReleaseOutcome) => {
                try {
                    await session?.release(outcome)
                } catch {
                    await session?.release(outcome)
                }
            }
            let state: 'prepared' | 'activating' | 'activated' | 'aborted' | 'failed' = 'prepared'
            return {
                async activate(expectedAlias?: AssetAlias) {
                    if (state !== 'prepared') {
                        throw new Error(`Prepared asset write is already ${state}`)
                    }
                    state = 'activating'
                    let committed = false
                    try {
                        await session?.seal()
                        for (let attempt = 0; attempt < 8; attempt++) {
                            const revision = await readCurrentRevision(options.store)
                            if (expectedAlias) {
                                const current = await options.store.readAssetAlias(identity)
                                if (!current || JSON.stringify(current.value) !== JSON.stringify(expectedAlias)) {
                                    throw new ConditionalAssetWriteConflictError()
                                }
                            }
                            try {
                                if (expectedLibrary === undefined) await options.store.commitAssetAlias(alias, revision)
                                else await options.store.commitAssetAlias(alias, revision, expectedLibrary)
                                committed = true
                                break
                            } catch (error) {
                                if (!(error instanceof RevisionConflictError) || attempt === 7) throw error
                            }
                        }
                    } catch (error) {
                        state = 'failed'
                        try {
                            await releaseSession('aborted')
                        } catch (releaseError) {
                            throw new AggregateError(
                                [error, releaseError],
                                `Asset activation and durable CAS session cleanup failed for ${identity.key}`,
                            )
                        }
                        throw error
                    }
                    if (committed) {
                        state = 'activated'
                        await releaseSession('committed')
                        if (alias.kind === 'inlay' && alias.inlayType === 'image' && alias.objectHash && validImageDimensions(alias.width, alias.height)) {
                            void options.imageGeometry?.write([{ contentHash: alias.objectHash, width: alias.width!, height: alias.height! }])
                                .catch(() => console.warn('Image dimensions could not be saved'))
                        }
                    }
                    return aliasBlobMetadata(alias)
                },
                async abort() {
                    if (state !== 'prepared') {
                        throw new Error(`Prepared asset write is already ${state}`)
                    }
                    if (session) await session.release('aborted')
                    state = 'aborted'
                },
            }
        } catch (error) {
            if (session) {
                try {
                    await session.release('aborted')
                } catch (releaseError) {
                    throw new AggregateError(
                        [error, releaseError],
                        `Asset write and durable CAS session cleanup failed for ${identity.key}`,
                    )
                }
            }
            throw error
        }
    }

    const publish = async (
        identity: AssetAliasIdentity,
        data: Uint8Array,
        metadata: Parameters<BlobStore['put']>[2],
    ): Promise<BlobMetadata> => {
        const prepared = await preparePublish(identity, data.slice(), metadata)
        return prepared.activate()
    }

    const prepareNewInlayImage = async (
        identity: AssetAliasIdentity,
        ownedData: Uint8Array,
        input: { name: string, metadata?: Record<string, unknown> },
    ): Promise<PreparedCompleteAssetWrite> => {
        validateAssetAliasIdentity(identity)
        if (identity.kind !== 'inlay') {
            throw new TypeError('New Inlay image requires an Inlay identity')
        }
        const encoded = await options.newInlayImages.encodeNewInlayImage(
            identity.key,
            ownedData,
            { ...input },
        )
        if (!(encoded.data instanceof Uint8Array)) {
            throw new TypeError('New Inlay image encoder must return Uint8Array bytes')
        }
        if (encoded.metadata.kind !== 'inlay') {
            throw new TypeError('New Inlay image encoder must return Inlay metadata')
        }
        const { preservationReason, ...metadata } = encoded.metadata
        const prepared = await preparePublish(identity, encoded.data, { ...metadata, ...(input.metadata === undefined ? {} : { metadata: input.metadata }) })
        return {
            ...prepared,
            async activate(expectedAlias?: AssetAlias) {
                const result = await prepared.activate(expectedAlias)
                return { ...result, ...(preservationReason ? { preservationReason } : {}) }
            },
        }
    }

    return {
        prepareOwnedPut: preparePublish,
        prepareOwnedNewInlayImage: prepareNewInlayImage,
        put: publish,
        async putNewInlayImage(identity, data, input) {
            const prepared = await prepareNewInlayImage(identity, data.slice(), input)
            return await prepared.activate() as InlayBlobMetadata
        },
        async read(identity, range) {
            return (await repository.read(identity, range))?.value.data ?? null
        },
        async stat(identity) {
            const result = await repository.stat(identity)
            if (!result || result.value.source === 'missing') return null
            return aliasBlobMetadata(result.value.alias)
        },
        async list(query = {}) {
            const output: BlobMetadata[] = []
            const seenCursors = new Set<string>()
            let cursor: string | undefined
            let revision: DataRevision | undefined
            do {
                const page = await options.store.listAssetAliases({
                    ...(query.kind === undefined ? {} : { kind: query.kind }),
                    limit: pageSize,
                    ...(cursor === undefined ? {} : { cursor }),
                })
                if (revision !== undefined && revision !== page.revision) {
                    throw new Error('Asset alias revision changed while listing')
                }
                revision = page.revision
                for (const alias of page.items) {
                    validateAssetAlias(alias)
                    if (query.kind !== undefined && alias.kind !== query.kind) {
                        throw new TypeError('Asset alias list returned the wrong kind')
                    }
                    output.push(aliasBlobMetadata(alias))
                }
                cursor = page.nextCursor
                if (cursor !== undefined && seenCursors.has(cursor)) {
                    throw new Error('Asset alias list cursor did not advance')
                }
                if (cursor !== undefined) seenCursors.add(cursor)
            } while (cursor !== undefined)
            return output
        },
        async remove(identity) {
            validateAssetAliasIdentity(identity)
            for (;;) {
                const versioned = await readValidatedAlias(options.store, identity)
                if (!versioned) return
                try {
                    await options.store.deleteAssetAlias(identity, versioned.revision)
                    return
                } catch (error) {
                    if (!(error instanceof RevisionConflictError)) throw error
                }
            }
        },
        async resolveImageSource(identity) {
            validateAssetAliasIdentity(identity)
            const versioned = await readValidatedAlias(options.store, identity)
            const alias = versioned?.value
            if (!alias?.objectHash) return null
            const isImage = alias.kind === 'inlay' ? alias.inlayType === 'image' : inferBlobMime(alias.mime, alias.ext).startsWith('image/')
            if (!isImage) return null
            const size = await options.cas.statObject(alias.objectHash) ?? await options.remote?.statObject(alias.objectHash) ?? null
            if (size === null) return null
            if (size !== alias.size) throw new Error('Image asset size mismatch')
            const url = await options.objectUrls.resolveObjectUrl({ contentHash: alias.objectHash, mime: inferBlobMime(alias.mime, alias.ext), size })
            if (!url) return null
            let geometry: import('./imageGeometry').ImageGeometry | undefined
            try { geometry = (await options.imageGeometry?.read([alias.objectHash]))?.[0] }
            catch { console.warn('Stored image dimensions are unavailable') }
            const source: import('./blobStore').BlobImageSource = {
                url,
                metadata: aliasBlobMetadata(alias),
                contentHash: alias.objectHash,
                width: geometry?.width ?? (alias.kind === 'inlay' ? alias.width : undefined),
                height: geometry?.height ?? (alias.kind === 'inlay' ? alias.height : undefined),
                async recordDimensions(width, height) {
                    if (!options.imageGeometry) return
                    await options.imageGeometry.write([{ contentHash: alias.objectHash!, width, height }])
                    source.width = width
                    source.height = height
                },
            }
            return source
        },
        async resolveUrl(identity) {
            validateAssetAliasIdentity(identity)
            const versioned = await readValidatedAlias(options.store, identity)
            if (!versioned) return null
            const alias = versioned.value
            if (alias.objectHash === null) return null
            const size = await options.cas.statObject(alias.objectHash)
                ?? await options.remote?.statObject(alias.objectHash) ?? null
            if (size === null) return null
            if (size !== alias.size) {
                throw new Error(`Asset alias size mismatch for ${identity.key}`)
            }
            return options.objectUrls.resolveObjectUrl({
                contentHash: alias.objectHash,
                mime: inferBlobMime(alias.mime, alias.ext),
                size: alias.size,
            })
        },
    }
}

function requireNamespacedBlobIdentity(
    key: string,
    kind?: AssetAliasKind,
): AssetAliasIdentity {
    const identity = blobIdentity(key)
    if (kind !== undefined && identity.kind !== kind) {
        throw new TypeError('BlobStore key namespace does not match its metadata kind')
    }
    return identity
}

export function createCompleteAssetRepositoryBlobStore(
    options: CompleteAssetRepositoryBlobStoreOptions,
): CompleteAssetRepositoryBlobStore {
    const repository = createCompleteTypedAssetRepository(options)
    return {
        imageGeometry: options.imageGeometry,
        resolveImageSource: (key) => repository.resolveImageSource(requireNamespacedBlobIdentity(key)),
        prepareOwnedPut(key, ownedData, metadata, verifyContent) {
            return repository.prepareOwnedPut(
                requireNamespacedBlobIdentity(key, metadata.kind),
                ownedData,
                metadata,
                verifyContent,
            )
        },
        prepareOwnedNewInlayImage(key, ownedData, input) {
            return repository.prepareOwnedNewInlayImage(
                requireNamespacedBlobIdentity(key, 'inlay'),
                ownedData,
                input,
            )
        },
        async put(key, data, metadata) {
            return await repository.put(
                requireNamespacedBlobIdentity(key, metadata.kind),
                data,
                metadata,
            )
        },
        async putVerified(key, data, metadata, beforeActivate) {
            const prepared = await repository.prepareOwnedPut(
                requireNamespacedBlobIdentity(key, metadata.kind), data.slice(), metadata, true,
            )
            try { await beforeActivate?.() }
            catch (error) { await prepared.abort(); throw error }
            return prepared.activate()
        },
        async putNewInlayImage(key, data, input) {
            return await repository.putNewInlayImage(
                requireNamespacedBlobIdentity(key, 'inlay'),
                data,
                input,
            )
        },
        read: (key, range) => repository.read(requireNamespacedBlobIdentity(key), range),
        stat: (key) => repository.stat(requireNamespacedBlobIdentity(key)),
        async statMany(keys) {
            if (!options.store.readAssetAliasesByKeys) {
                const results: (BlobMetadata | null)[] = []
                for (const key of keys) results.push(await repository.stat(requireNamespacedBlobIdentity(key)))
                return results
            }
            const identities = [...new Set(keys)].map(key => requireNamespacedBlobIdentity(key))
            const found = new Map<string, BlobMetadata>()
            for (const kind of ['asset', 'inlay'] as const) {
                const selected = identities.filter(identity => identity.kind === kind).map(identity => identity.key)
                for (let start = 0; start < selected.length; start += 512) {
                    const page = selected.slice(start, start + 512)
                    const aliases = (await options.store.readAssetAliasesByKeys(kind, page)).value
                    const requested = new Set(page)
                    for (const alias of aliases) {
                        validateAssetAlias(alias)
                        if (alias.kind !== kind || !requested.has(alias.key)) throw new Error('Asset batch identity mismatch')
                    }
                    const hashes = [...new Set(aliases.flatMap(alias => alias.objectHash ? [alias.objectHash] : []))]
                    const sizes = options.cas.statObjects
                        ? await options.cas.statObjects(hashes)
                        : await Promise.all(hashes.map(hash => options.cas.statObject(hash)))
                    if (sizes.length !== hashes.length) throw new Error('Asset batch size response mismatch')
                    const known = new Map(hashes.map((hash, index) => [hash, sizes[index]]))
                    if (options.remote) {
                        const remoteHashes = hashes.filter(hash => known.get(hash) === null)
                        const remoteSizes = options.remote.statObjects
                            ? await options.remote.statObjects(remoteHashes)
                            : await Promise.all(remoteHashes.map(hash => options.remote!.statObject(hash)))
                        if (remoteSizes.length !== remoteHashes.length) throw new Error('Remote asset batch size response mismatch')
                        remoteHashes.forEach((hash, index) => known.set(hash, remoteSizes[index]))
                    }
                    for (const alias of aliases) {
                        const size = alias.objectHash ? known.get(alias.objectHash) : null
                        if (size == null) continue
                        if (size !== alias.size) throw new AssetAliasSizeMismatchError(alias.key)
                        found.set(alias.key, aliasBlobMetadata(alias))
                    }
                }
            }
            return keys.map(key => found.get(key) ?? null)
        },
        async list(query) {
            const items = await repository.list(query)
            for (const item of items) requireNamespacedBlobIdentity(item.key, item.kind)
            return items
        },
        remove: (key) => repository.remove(requireNamespacedBlobIdentity(key)),
        resolveUrl: (key) => repository.resolveUrl(requireNamespacedBlobIdentity(key)),
    }
}
