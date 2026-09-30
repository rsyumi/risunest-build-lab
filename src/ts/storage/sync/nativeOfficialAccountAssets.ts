import type { AccountStorage } from '../accountStorage'
import type { BlobStore } from '../blobStore'
import { storeActiveAsset } from '../accountAssetAccess'
import type { NativeDeviceSettings } from '../nativeDeviceSettings'
import type { PersistentDataStore } from '../persistentDataStore'
import { withPersistentRevisionLease } from '../persistentRecordIterator'
import type { OfficialAssetLedger } from './officialAssetLedger'
import { collectPinnedReferences } from './officialAccountSnapshot'
import { nativeOfficialAccountKeys } from './nativeOfficialAccountFlow'

export function createNativeOfficialAccountAssets(dependencies: {
    settings: NativeDeviceSettings
    store: PersistentDataStore
    resolveBlobs(): Promise<BlobStore>
    account: Pick<AccountStorage, 'readItem'>
    ledger: OfficialAssetLedger
    flushMetadata(): Promise<void>
}) {
    const { settings, store, account, ledger } = dependencies
    const key = nativeOfficialAccountKeys.pendingAssets
    const clear = () => settings.set(key, null)
    const prepare = (accountId: string) => settings.set(key, { accountId })
    const complete = async (accountId: string, onProgress?: (completed: number, total: number) => void): Promise<number> => {
        const pending = await settings.get(key) as { accountId?: unknown } | null
        if (pending?.accountId !== accountId) return 0
        const lease = await store.acquireRevision((await store.readRoot()).revision)
        return withPersistentRevisionLease(lease, async reader => {
            const references = await collectPinnedReferences(reader)
            const blobs = await dependencies.resolveBlobs()
            let missing = 0
            let completed = 0
            onProgress?.(completed, references.assets.length)
            for (const asset of references.assets) {
                if (await blobs.stat(asset)) {
                    onProgress?.(++completed, references.assets.length)
                    continue
                }
                const result = await account.readItem(asset)
                if (result.kind === 'missing') {
                    missing += 1
                    onProgress?.(++completed, references.assets.length)
                    continue
                }
                const name = asset.replace(/\\/g, '/').split('/').pop() ?? asset
                await storeActiveAsset(blobs, asset, result.bytes, {
                    kind: 'asset', mime: '', name, ext: name.split('.').pop() ?? '',
                })
                const verified = await blobs.read(asset)
                if (!verified || verified.length !== result.bytes.length
                    || verified.some((value, index) => value !== result.bytes[index])) {
                    throw new Error('Restored account asset verification failed')
                }
                ledger.record(asset, asset)
                onProgress?.(++completed, references.assets.length)
            }
            await dependencies.flushMetadata()
            if (missing === 0) await clear()
            return missing
        })
    }
    return { prepare, complete, clear }
}
