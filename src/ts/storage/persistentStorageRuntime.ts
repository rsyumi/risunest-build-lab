import { isTauri } from '../platform'
import { getPersistentDataRuntime } from './persistentDataRuntime.svelte'
import { getPersistentStorageAuthority } from './persistentDataStoreFactory'
import {
    configureActiveBlobStore,
} from './platformBlobStore'
import {
    createNativeAssetBlobStore,
    createCoordinatorOwnedAssetBlobStore,
    createRuntimeAssetRepositoryDispatcher,
    type RuntimeAssetRepositoryDispatcher,
} from './assetRepositoryRuntime'
import {
    createNativeAssetObjectUrlResolver,
    createNativeRemoteAssetReader,
    createNativeDurableAssetWriteSessionFactory,
    createNativeImmutablePayloadCas,
    createNativeNewInlayImageEncoder,
} from './nativeAssetRepository'
import type { BlobStore } from './blobStore'
import type { PersistentStorageAuthority } from './persistentStorageAuthority'

function createConfiguredAssetBlobStore(
    store: RuntimeAssetRepositoryDispatcher,
    authority: PersistentStorageAuthority,
): BlobStore {
    return createCoordinatorOwnedAssetBlobStore(
        store,
        authority,
        getPersistentDataRuntime(),
    )
}

async function installPersistentStorage(): Promise<void> {
    const authority = getPersistentStorageAuthority()
    await authority.rawStore.open()
    if (!isTauri) {
        configureActiveBlobStore(authority.gate)
        return
    }
    const repository = createNativeAssetBlobStore({
        store: authority.rawStore,
        cas: createNativeImmutablePayloadCas(),
        objectUrls: createNativeAssetObjectUrlResolver(),
        remote: createNativeRemoteAssetReader(),
        newInlayImages: createNativeNewInlayImageEncoder(),
        writeSessions: createNativeDurableAssetWriteSessionFactory(),
    })
    configureActiveBlobStore(
        authority.gate,
        createConfiguredAssetBlobStore(
            createRuntimeAssetRepositoryDispatcher(repository),
            authority,
        ),
        { alreadyGuarded: true },
    )
}

let installation: Promise<void> | null = null

export function initializePersistentStorage(): Promise<void> {
    return installation ??= installPersistentStorage().catch((error) => {
        installation = null
        throw error
    })
}
