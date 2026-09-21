import type {
    BlobMetadata,
    BlobStore,
    BlobWriteMetadata,
} from './blobStore'
import {
    createCompleteAssetRepositoryBlobStore,
    type AssetObjectUrlResolver,
    type CompleteAssetRepositoryBlobStore,
    type DurableAssetWriteSessionFactory,
    type NewInlayImageEncoder,
    type PreparedCompleteAssetWrite,
    type RemoteAssetReader,
} from './assetRepository'
import type { ImmutablePayloadCas } from './payloadCas'
import { RevisionConflictError, type PersistentDataStore } from './persistentDataStore'
import type { PersistentStorageAuthority } from './persistentStorageAuthority'

export function createNativeAssetBlobStore(input: {
    remote?: RemoteAssetReader
    store: PersistentDataStore
    cas: ImmutablePayloadCas
    objectUrls: AssetObjectUrlResolver
    newInlayImages: NewInlayImageEncoder
    writeSessions: DurableAssetWriteSessionFactory
}): CompleteAssetRepositoryBlobStore {
    return createCompleteAssetRepositoryBlobStore({
        remote: input.remote,
        store: input.store,
        cas: input.cas,
        objectUrls: input.objectUrls,
        newInlayImages: input.newInlayImages,
        writeSessions: input.writeSessions,
    })
}

export interface StagedRuntimeAssetWrite {
    readonly prepared: PreparedCompleteAssetWrite
}

export type RuntimeAssetRepositoryDispatcher = BlobStore &
    Required<Pick<BlobStore, 'putNewInlayImage'>> & {
        stagePut(
            key: string,
            ownedData: Uint8Array,
            metadata: BlobWriteMetadata,
        ): Promise<StagedRuntimeAssetWrite>
        stageNewInlayImage(
            key: string,
            ownedData: Uint8Array,
            input: { name: string },
        ): Promise<StagedRuntimeAssetWrite>
        activateStagedWrite(staged: StagedRuntimeAssetWrite): Promise<BlobMetadata>
        abortStagedWrite(staged: StagedRuntimeAssetWrite): Promise<void>
    }

export interface StorageOnlyMutationCoordinator {
    getStorageAuthorityEpoch(): number
    runStorageOnlyMutation(
        operation: (expectedRevision: number) => Promise<number>,
    ): Promise<void>
}

export function createCoordinatorOwnedAssetBlobStore(
    store: RuntimeAssetRepositoryDispatcher,
    authority: PersistentStorageAuthority,
    runtime: StorageOnlyMutationCoordinator,
): BlobStore {
    const invocationTails = new Map<string, Promise<void>>()
    const reserveInvocation = (key: string) => {
        const previous = invocationTails.get(key) ?? Promise.resolve()
        let releaseOwn!: () => void
        const own = new Promise<void>((resolve) => { releaseOwn = resolve })
        const tail = previous.then(() => own)
        invocationTails.set(key, tail)
        return {
            previous,
            release() {
                releaseOwn()
                void tail.then(() => {
                    if (invocationTails.get(key) === tail) invocationTails.delete(key)
                })
            },
        }
    }
    const mutate = async <T>(
        key: string,
        operation: () => Promise<T>,
        expectedAuthorityEpoch?: number,
    ): Promise<T> => {
        let result!: T
        let operationFailure: { error: unknown } | null = null
        await runtime.runStorageOnlyMutation((expectedRevision) =>
            authority.gate.runKeyedWrite(key, async () => {
                const before = await authority.rawStore.readRoot()
                if (before.revision !== expectedRevision) {
                    throw new RevisionConflictError(expectedRevision, before.revision)
                }
                if (
                    expectedAuthorityEpoch !== undefined
                    && runtime.getStorageAuthorityEpoch() !== expectedAuthorityEpoch
                ) {
                    throw new Error(
                        'Asset storage authority changed before staged write activation',
                    )
                }
                try {
                    result = await operation()
                } catch (error) {
                    operationFailure = { error }
                }
                try {
                    return (await authority.rawStore.readRoot()).revision
                } catch (error) {
                    if (!operationFailure) throw error
                    throw new AggregateError(
                        [operationFailure.error, error],
                        `Asset mutation and revision read failed for ${key}`,
                    )
                }
            }))
        if (operationFailure) throw operationFailure.error
        return result
    }
    const stageInInvocationOrder = async <T>(
        key: string,
        prepare: () => ReturnType<RuntimeAssetRepositoryDispatcher['stagePut']>,
    ): Promise<T> => {
        const invocation = reserveInvocation(key)
        const expectedAuthorityEpoch = runtime.getStorageAuthorityEpoch()
        let staged: StagedRuntimeAssetWrite | undefined
        try {
            staged = await prepare()
            await invocation.previous
            try {
                return await mutate(
                    key,
                    () => store.activateStagedWrite(staged!) as Promise<T>,
                    expectedAuthorityEpoch,
                )
            } catch (error) {
                try {
                    await store.abortStagedWrite(staged)
                } catch (abortError) {
                    throw new AggregateError(
                        [error, abortError],
                        `Staged asset write and cleanup failed for ${key}`,
                    )
                }
                throw error
            }
        } finally {
            invocation.release()
        }
    }
    const mutateInInvocationOrder = async <T>(
        key: string,
        operation: () => Promise<T>,
    ): Promise<T> => {
        const invocation = reserveInvocation(key)
        const expectedAuthorityEpoch = runtime.getStorageAuthorityEpoch()
        try {
            await invocation.previous
            return await mutate(key, operation, expectedAuthorityEpoch)
        } finally {
            invocation.release()
        }
    }
    const coordinated: BlobStore = {
        async put(key, data, metadata) {
            const ownedData = data.slice()
            const ownedMetadata = { ...metadata }
            return stageInInvocationOrder(
                key,
                () => store.stagePut(key, ownedData, ownedMetadata),
            )
        },
        read: (key, range) => store.read(key, range),
        stat: (key) => store.stat(key),
        list: (query) => store.list(query),
        remove: (key) => mutateInInvocationOrder(key, () => store.remove(key)),
        resolveUrl: (key) => store.resolveUrl(key),
    }
    coordinated.putNewInlayImage = (key, data, input) => {
        const ownedData = data.slice()
        const ownedInput = { ...input }
        return stageInInvocationOrder(
            key,
            () => store.stageNewInlayImage(key, ownedData, ownedInput),
        )
    }
    return coordinated
}

export function createRuntimeAssetRepositoryDispatcher(
    store: CompleteAssetRepositoryBlobStore,
): RuntimeAssetRepositoryDispatcher {
    const activationStarted = new WeakSet<StagedRuntimeAssetWrite>()
    const abortStarted = new WeakSet<StagedRuntimeAssetWrite>()
    return {
        async stagePut(key, ownedData, metadata) {
            return { prepared: await store.prepareOwnedPut(key, ownedData, metadata) }
        },
        async stageNewInlayImage(key, ownedData, request) {
            return {
                prepared: await store.prepareOwnedNewInlayImage(key, ownedData, request),
            }
        },
        async activateStagedWrite(staged) {
            activationStarted.add(staged)
            return staged.prepared.activate()
        },
        async abortStagedWrite(staged) {
            if (activationStarted.has(staged) || abortStarted.has(staged)) return
            abortStarted.add(staged)
            await staged.prepared.abort()
        },
        put: (key, data, metadata) => store.put(key, data, metadata),
        putNewInlayImage: (key, data, request) => store.putNewInlayImage(key, data, request),
        read: (key, range) => store.read(key, range),
        stat: (key) => store.stat(key),
        list: (query) => store.list(query),
        remove: (key) => store.remove(key),
        resolveUrl: (key) => store.resolveUrl(key),
    }
}
