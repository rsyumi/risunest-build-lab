import type { AccountStorage } from '../accountStorage'
import type { BlobStore } from '../blobStore'
import type { NativeDeviceSettings } from '../nativeDeviceSettings'
import type { PersistentDataStore } from '../persistentDataStore'
import type { OfficialAssetLedger } from './officialAssetLedger'
import { createOfficialAccountAssetContinuation, OfficialAssetOwnershipError } from './officialAccountAssetContinuation'
import { emptyOfficialAccountAssetCounts, type OfficialAccountAssetOptions, type OfficialAccountAssetOutcome } from './officialAccountAssetOutcome'
import { transferOfficialAccountAssets } from './officialAccountTransfer'

export function createNativeOfficialAccountAssets(dependencies: {
    settings: NativeDeviceSettings
    store: PersistentDataStore
    resolveBlobs(): Promise<BlobStore>
    account: Pick<AccountStorage, 'readItem'>
    ledger: OfficialAssetLedger
    flushMetadata(): Promise<void>
    origin: string
    concurrency?: 1 | 2 | 4
    getAccountId(): string | null | undefined
}) {
    const { account, ledger } = dependencies
    const continuation = createOfficialAccountAssetContinuation(dependencies)
    const complete = async (accountId: string, options: OfficialAccountAssetOptions = {}): Promise<OfficialAccountAssetOutcome> => {
        const outcome: OfficialAccountAssetOutcome = { phase: 'checking', state: 'running', counts: emptyOfficialAccountAssetCounts(), failedKeys: [] }
        const counts = outcome.counts
        let lastReportAt = -Infinity, lastPhase = ''
        const report = () => {
            const phase = `${outcome.phase}:${outcome.state}`
            if (phase === lastPhase && Date.now() - lastReportAt < 150) return
            lastPhase = phase; lastReportAt = Date.now()
            options.onProgress?.({ ...outcome, counts: { ...counts } })
        }
        const receipt = await continuation.load(accountId, options.operationId)
        if (!receipt) throw new OfficialAssetOwnershipError('receipt')
        outcome.operationId = receipt.operationId
        options.onRestoreStatus?.(receipt.status)
        const failedBefore = new Set([...receipt.failedKeys, ...options.previousFailedKeys ?? []])
        const stillFailed = new Set(failedBefore)
        report()
        try {
            await continuation.start(receipt)
            const assets = receipt.manifest
            counts.required = assets.length
            counts.notAttempted = counts.required
            report()
            if (options.signal?.aborted) { outcome.state = 'cancelled'; return outcome }
            const blobs = await dependencies.resolveBlobs()
            if (!blobs.statMany || !blobs.putVerified) throw new Error('Native account asset storage is unavailable')
            const present: Awaited<ReturnType<NonNullable<BlobStore['statMany']>>> = []
            for (let offset = 0; offset < assets.length; offset += 512) {
                if (options.signal?.aborted) { outcome.state = 'cancelled'; return outcome }
                await continuation.guard(receipt)
                const batch = await blobs.statMany(assets.slice(offset, offset + 512))
                present.push(...batch)
                batch.forEach((value, index) => { if (value) { counts.alreadyPresent++; counts.notAttempted--; stillFailed.delete(assets[offset + index]) } })
                report()
            }
            const remaining = assets.filter((_asset, index) => !present[index])
            const queue = [...remaining.filter(asset => !failedBefore.has(asset)), ...remaining.filter(asset => failedBefore.has(asset))]
            outcome.phase = 'downloading'
            report()
            const result = await transferOfficialAccountAssets({
                accountId, origin: dependencies.origin, keys: queue, concurrency: dependencies.concurrency, signal: options.signal,
                read: async (key, options) => { await continuation.guard(receipt); return account.readItem(key, options) },
                install: async (asset, bytes, signal) => {
                    await blobs.putVerified!(asset, bytes, {
                        kind: 'asset', mime: '', name: asset.replace(/\\/g, '/').split('/').pop() ?? asset,
                        ext: asset.split('.').pop() ?? '',
                    }, async () => { if (signal.aborted) throw signal.reason; await continuation.guard(receipt) })
                    ledger.record(asset, asset)
                },
                settled(asset, result) {
                    counts[result]++
                    counts.notAttempted--
                    if (result === 'failed') stillFailed.add(asset)
                    else stillFailed.delete(asset)
                    report()
                },
                progress(transfer) { outcome.transfer = transfer; report() },
            })
            outcome.reason = result.reason
            if (result.cancelled) outcome.state = 'cancelled'
        } catch (error) { outcome.reason = error instanceof OfficialAssetOwnershipError && error.reason !== 'receipt' ? error.reason : 'local' }
        finally {
            outcome.phase = 'finishing'
            report()
            try { await dependencies.flushMetadata() }
            catch { outcome.reason = 'local'; outcome.state = 'failed' }
            if (outcome.state === 'running' && options.signal?.aborted) outcome.state = 'cancelled'
            if (outcome.state === 'running') {
                outcome.state = outcome.reason || counts.failed ? 'failed' : 'succeeded'
                if (counts.failed && !outcome.reason) outcome.reason = 'files'
            }
            outcome.failedKeys = [...stillFailed]
            try { await continuation.settle(receipt, outcome) }
            catch (error) { outcome.state = 'failed'; outcome.reason = error instanceof OfficialAssetOwnershipError && error.reason !== 'receipt' ? error.reason : 'local' }
            report()
        }
        return outcome
    }
    return { ...continuation, complete }
}
