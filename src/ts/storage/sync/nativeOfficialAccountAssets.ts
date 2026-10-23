import type { AccountStorage } from '../accountStorage'
import type { BlobStore } from '../blobStore'
import { AssetAliasSizeMismatchError } from '../assetRepository'
import type { NativeDeviceSettings } from '../nativeDeviceSettings'
import { NativeFileJobError, runNativeOfficialAssetRestore, type NativeFileJobDependencies } from '../nativeFileJobs'
import type { PersistentDataStore } from '../persistentDataStore'
import type { OfficialAssetLedger } from './officialAssetLedger'
import { createOfficialAccountAssetContinuation, OfficialAssetOwnershipError } from './officialAccountAssetContinuation'
import { emptyOfficialAccountAssetCounts, type OfficialAccountAssetOptions, type OfficialAccountAssetOutcome } from './officialAccountAssetOutcome'
import { createRateMeter } from './transferRate'

export function createNativeOfficialAccountAssets(dependencies: {
    settings: NativeDeviceSettings
    store: PersistentDataStore
    resolveBlobs(): Promise<BlobStore>
    account: Pick<AccountStorage, 'readAssetsFromNative'>
    ledger: OfficialAssetLedger
    flushMetadata(): Promise<void>
    baseUrl: string
    concurrency?: 1 | 2 | 4
    getAccountId(): string | null | undefined
    jobs?: NativeFileJobDependencies
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
        let receipt: NonNullable<Awaited<ReturnType<typeof continuation.load>>>
        try {
            const loaded = await continuation.load(accountId, options.operationId)
            if (!loaded) throw new OfficialAssetOwnershipError('receipt')
            receipt = loaded
        } catch (error) {
            outcome.state = 'failed'
            outcome.reason = error instanceof OfficialAssetOwnershipError ? error.reason : 'checking'
            report()
            throw error
        }
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
            if (!blobs.statMany) throw new Error('Native account asset storage is unavailable')
            const present: Awaited<ReturnType<NonNullable<BlobStore['statMany']>>> = []
            for (let offset = 0; offset < assets.length; offset += 512) {
                if (options.signal?.aborted) { outcome.state = 'cancelled'; return outcome }
                await continuation.guard(receipt)
                const keys = assets.slice(offset, offset + 512)
                const damaged = new Set<string>()
                let batch: Awaited<ReturnType<NonNullable<BlobStore['statMany']>>>
                while (true) {
                    if (options.signal?.aborted) { outcome.state = 'cancelled'; return outcome }
                    const checking = keys.filter(key => !damaged.has(key))
                    try {
                        const available = await blobs.statMany(checking)
                        let index = 0
                        batch = keys.map(key => damaged.has(key) ? null : available[index++])
                        break
                    } catch (error) {
                        if (!(error instanceof AssetAliasSizeMismatchError) || !checking.includes(error.key)) throw error
                        damaged.add(error.key)
                    }
                }
                present.push(...batch)
                batch.forEach((value, index) => { if (value) { counts.alreadyPresent++; counts.notAttempted--; stillFailed.delete(assets[offset + index]) } })
                report()
            }
            const remaining = assets.filter((_asset, index) => !present[index])
            let pending = [...remaining.filter(asset => !failedBefore.has(asset)), ...remaining.filter(asset => failedBefore.has(asset))]
            outcome.phase = 'downloading'
            report()
            const rate = createRateMeter()
            rate.add(Date.now(), 0)
            let bytesBefore = 0
            const authorized = pending.length === 0 ? 'done' : await account.readAssetsFromNative(async ({ credential, reauthenticated }) => {
                await continuation.guard(receipt)
                const result = await runNativeOfficialAssetRestore({
                    baseUrl: dependencies.baseUrl, credential, accountId, expectedLibrary: receipt.library, keys: pending,
                    concurrency: dependencies.concurrency, reauthenticated,
                }, {
                    signal: options.signal,
                    guard: () => continuation.guard(receipt),
                    settled(asset, result) {
                        // The ledger follows the signed-in account, so a result that settles after an account change is not recorded.
                        if (result === 'installed' && dependencies.getAccountId() === accountId) ledger.record(asset, asset)
                        counts[result]++
                        counts.notAttempted--
                        if (result === 'failed') stillFailed.add(asset)
                        else stillFailed.delete(asset)
                        report()
                    },
                    progress(job) {
                        const bytes = bytesBefore + job.bytes
                        rate.add(Date.now(), bytes)
                        outcome.transfer = { bytes, bytesPerSecond: rate.rate(Date.now()), active: job.active, limit: job.limit, waitingUntil: job.waitingUntil }
                        report()
                    },
                }, dependencies.jobs)
                bytesBefore = outcome.transfer?.bytes ?? bytesBefore
                pending = result.unsettled
                if (result.cancelled) outcome.state = 'cancelled'
                else if (result.reason === 'reauthentication-needed') return 'reauthentication-needed'
                else outcome.reason = result.reason
                return 'done'
            }, { signal: options.signal })
            if (authorized === 'unauthenticated') {
                await continuation.guard(receipt)
                outcome.reason = 'authentication'
            }
        } catch (error) {
            if (error instanceof OfficialAssetOwnershipError) outcome.reason = error.reason
            else if (error instanceof NativeFileJobError && error.code === 'library-changed') outcome.reason = 'library-changed'
            else if (!options.signal?.aborted) outcome.reason = outcome.phase === 'checking' ? 'checking' : 'local'
        }
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
            try { await continuation.settle(receipt, outcome, options.signal) }
            catch (error) { outcome.state = 'failed'; outcome.reason = error instanceof OfficialAssetOwnershipError ? error.reason : 'local' }
            report()
        }
        return outcome
    }
    return { ...continuation, complete }
}
