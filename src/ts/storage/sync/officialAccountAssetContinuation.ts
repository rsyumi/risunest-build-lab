import { v4 } from 'uuid'
import type { NativeDeviceSettings } from '../nativeDeviceSettings'
import type { NativeFileJobStatus } from '../nativeFileJobs'
import { readCurrentRevision, type PersistentDataStore } from '../persistentDataStore'
import { withPersistentRevisionLease } from '../persistentRecordIterator'
import { hashPayloadBytes } from '../payloadCas'
import { collectPinnedReferences } from './officialAccountSnapshot'
import { nativeOfficialAccountKeys } from './nativeOfficialAccountFlow'
import type { OfficialAccountAssetOutcome } from './officialAccountAssetOutcome'
import { OfficialAssetOwnershipError } from '../accountReadError'
import { isBackgroundExpiryReason } from '../../iosNative'
export { OfficialAssetOwnershipError } from '../accountReadError'

export interface OfficialAssetContinuation {
    version: 1
    operationId: string
    accountId: string
    state: 'awaiting-activation' | 'interrupted' | 'stopped' | 'failed'
    library?: string
    preActivationLibrary?: string
    manifest?: string[]
    manifestId?: string
    failedKeys: string[]
    status?: NativeFileJobStatus
    previous?: Omit<OfficialAssetContinuation, 'previous'>
}
export type OfficialAssetRecovery = { kind: 'none' | 'current-library' | 'retry' | 'interrupted'; operationId?: string }
const digest = (keys: string[]) => hashPayloadBytes(new TextEncoder().encode(JSON.stringify(keys)))

export function createOfficialAccountAssetContinuation(dependencies: {
    settings: NativeDeviceSettings; store: PersistentDataStore; getAccountId(): string | null | undefined
}) {
    const { settings, store } = dependencies
    const key = nativeOfficialAccountKeys.assetContinuation
    const read = async (): Promise<OfficialAssetContinuation | null> => {
        const value = await settings.get(key) as Partial<OfficialAssetContinuation> | null
        if (!value || value.version !== 1 || typeof value.operationId !== 'string' || typeof value.accountId !== 'string'
            || !['awaiting-activation', 'interrupted', 'stopped', 'failed'].includes(value.state ?? '')
            || !Array.isArray(value.failedKeys) || value.failedKeys.some(item => typeof item !== 'string')) return null
        return value as OfficialAssetContinuation
    }
    const guard = async (receipt: OfficialAssetContinuation) => {
        if (dependencies.getAccountId() !== receipt.accountId) throw new OfficialAssetOwnershipError('account-changed')
        if (!receipt.library || !store.readLibraryIdentity || await store.readLibraryIdentity() !== receipt.library) throw new OfficialAssetOwnershipError('library-changed')
    }
    const load = async (accountId: string, operationId?: string) => {
        const receipt = await read()
        if (!receipt) return null
        if (receipt.accountId !== accountId) throw new OfficialAssetOwnershipError('account-changed')
        if (operationId && operationId !== receipt.operationId) throw new OfficialAssetOwnershipError('receipt')
        if (receipt.state === 'awaiting-activation') return null
        await guard(receipt)
        if (!Array.isArray(receipt.manifest) || receipt.manifest.some(key => typeof key !== 'string' || !/^assets[/\\]/.test(key))
            || new Set(receipt.manifest).size !== receipt.manifest.length || receipt.manifestId !== await digest(receipt.manifest)) {
            throw new OfficialAssetOwnershipError('receipt')
        }
        return receipt as OfficialAssetContinuation & { manifest: string[]; library: string }
    }
    const clear = async () => {
        const results = await Promise.allSettled([settings.set(key, null), settings.set(nativeOfficialAccountKeys.pendingAssets, null)])
        const failure = results.find(result => result.status === 'rejected')
        if (failure?.status === 'rejected') throw failure.reason
    }
    const prepare = async (accountId: string) => {
        const current = await read()
        const previous = current?.state === 'awaiting-activation' ? current.previous : current ?? undefined
        const preActivationLibrary = await store.readLibraryIdentity?.()
        await settings.set(key, { version: 1, operationId: v4(), accountId, state: 'awaiting-activation', failedKeys: [], previous, preActivationLibrary } satisfies OfficialAssetContinuation)
    }
    const cancelPreparation = async () => {
        const receipt = await read()
        if (receipt?.state === 'awaiting-activation') await settings.set(key, receipt.previous ?? null)
    }
    const bind = async (accountId: string, status?: NativeFileJobStatus) => {
        const prepared = await read()
        if (!prepared || prepared.state !== 'awaiting-activation' || prepared.accountId !== accountId) throw new OfficialAssetOwnershipError('receipt')
        const { previous: _previous, preActivationLibrary: _preActivationLibrary, ...receipt } = prepared
        receipt.library = await store.readLibraryIdentity?.()
        await guard(receipt)
        const lease = await store.acquireRevision(await readCurrentRevision(store))
        receipt.manifest = await withPersistentRevisionLease(lease, async reader => (await collectPinnedReferences(reader)).assets)
        receipt.manifestId = await digest(receipt.manifest)
        receipt.status = status
        await guard(receipt)
        receipt.state = 'interrupted'
        await settings.set(key, receipt)
        await settings.set(nativeOfficialAccountKeys.pendingAssets, null)
    }
    const inspect = async (accountId: string): Promise<OfficialAssetRecovery> => {
        const raw = await read()
        if (!raw) {
            const legacy = await settings.get(nativeOfficialAccountKeys.pendingAssets) as { accountId?: unknown } | null
            return { kind: legacy?.accountId === accountId ? 'current-library' : 'none' }
        }
        if (raw.accountId !== accountId) { await clear(); return { kind: 'none' } }
        if (raw.state === 'awaiting-activation') {
            const library = await store.readLibraryIdentity?.()
            if ((raw.preActivationLibrary && raw.preActivationLibrary === library)
                || (raw.previous?.accountId === accountId && raw.previous.library && raw.previous.library === library)) {
                await cancelPreparation()
                return inspect(accountId)
            }
            return { kind: 'current-library' }
        }
        try {
            const receipt = await load(accountId)
            return { kind: receipt?.state === 'interrupted' ? 'interrupted' : 'retry', operationId: receipt!.operationId }
        } catch (error) {
            if (!(error instanceof OfficialAssetOwnershipError)) throw error
            await clear()
            return { kind: 'none' }
        }
    }
    return { prepare, bind, cancelPreparation, clear, load, guard, inspect,
        async stop(accountId: string, operationId: string) {
            let receipt: Awaited<ReturnType<typeof load>>
            try { receipt = await load(accountId, operationId) }
            catch (error) {
                if (error instanceof OfficialAssetOwnershipError) return
                throw error
            }
            if (receipt && receipt.state !== 'stopped') await settings.set(key, { ...receipt, state: 'stopped' })
        },
        async start(receipt: OfficialAssetContinuation) {
            await guard(receipt)
            await settings.set(key, { ...receipt, state: 'interrupted' })
        },
        async settle(receipt: OfficialAssetContinuation, outcome: OfficialAccountAssetOutcome, signal?: AbortSignal) {
            const current = await read()
            if (current?.operationId !== receipt.operationId) throw new OfficialAssetOwnershipError('receipt')
            await guard(receipt)
            if (outcome.state === 'succeeded') await clear()
            else await settings.set(key, { ...receipt, state: outcome.state === 'cancelled'
                ? isBackgroundExpiryReason(signal?.reason) ? 'interrupted' : 'stopped'
                : 'failed', failedKeys: outcome.failedKeys })
        },
    }
}
