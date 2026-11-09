import { measuredTaskPercent } from '../../mobileBackgroundTask'
import type { AccountStorage } from '../accountStorage'
import {
    cancelNativeOfficialPublication,
    continueNativeOfficialPublication,
    runNativeOfficialAssetPublication,
    runNativeOfficialPublicationAttempt,
    type NativeFileJobOptions,
    type NativeOfficialAssetPublicationOptions,
    type NativeOfficialAssetPublicationRequest,
    type NativeOfficialAssetPublicationResult,
    type NativeOfficialPublicationReceipt,
    type NativeOfficialPublicationRequest,
    type NativeOfficialPublicationRetryRequest,
    type NativeOfficialPublicationRunResult,
} from '../nativeFileJobs'
import {
    hasNativePersistentRevisionLease,
    nativePersistentRevisionLease,
} from '../nativePersistentExport'
import type {
    OfficialNativeAssetPublisher,
    OfficialNativeDatabasePublisher,
    OfficialRecoveredPublication,
} from './officialAccountSnapshot'

export interface NativeOfficialPublicationJobDependencies {
    account: Pick<AccountStorage, 'writeOfficialDatabaseFromNative'>
    baseUrl: string
    runAttempt?(
        request: NativeOfficialPublicationRequest,
        options?: NativeFileJobOptions,
    ): Promise<NativeOfficialPublicationRunResult | null>
    continueAttempt?(
        jobId: string,
        request: NativeOfficialPublicationRetryRequest,
        expected: {
            revision: number
            accountId: string
        },
        options?: NativeFileJobOptions,
    ): Promise<NativeOfficialPublicationRunResult>
    cancelAttempt?(jobId: string): Promise<NativeOfficialPublicationReceipt | null>
    reconcilePendingPublications?(input: {
        accountId: string
        revision: number
    }): Promise<OfficialRecoveredPublication | null>
}

function hasDrainedNativeOfficialPublicationCancellation(error: unknown): boolean {
    return typeof error === 'object'
        && error !== null
        && 'nativeOfficialPublicationCancellationDrained' in error
        && error.nativeOfficialPublicationCancellationDrained === true
}

export function createNativeOfficialPublicationJobPublisher(
    dependencies: NativeOfficialPublicationJobDependencies,
): OfficialNativeDatabasePublisher {
    const runAttempt = dependencies.runAttempt ?? runNativeOfficialPublicationAttempt
    const continueAttempt = dependencies.continueAttempt ?? continueNativeOfficialPublication
    const cancelAttempt = dependencies.cancelAttempt ?? cancelNativeOfficialPublication
    return async (input) => {
        const { background } = input
        const recovered = await dependencies.reconcilePendingPublications?.({
            accountId: input.accountId,
            revision: input.revision,
        })
        if (recovered) {
            return {
                databaseFingerprint: recovered.databaseFingerprint,
                acknowledge: async () => undefined,
                completeReload: async () => undefined,
            }
        }
        if (!hasNativePersistentRevisionLease(input.lease)) return null
        const onStatus: NonNullable<NativeFileJobOptions['onStatus']> = status => {
            input.onStatus?.(status)
            background.progress(measuredTaskPercent(status.progress.completedBytes, status.progress.totalBytes))
        }
        let pendingJobId: string | null = null
        let result
        try {
            result = await dependencies.account.writeOfficialDatabaseFromNative(
                async (context) => {
                    const outcome = pendingJobId
                        ? await continueAttempt(
                            pendingJobId,
                            {
                                accountId: input.accountId,
                                session: context.session,
                                saveDate: context.saveDate,
                                credential: context.credential,
                            },
                            {
                                revision: input.revision,
                                accountId: input.accountId,
                            },
                            { signal: context.signal, onStatus: status => { input.onStatus?.(status); background.progress(null) } },
                        )
                        : await runAttempt({
                            expectedRevision: input.revision,
                            lease: input.lease[nativePersistentRevisionLease],
                            accountId: input.accountId,
                            baseUrl: dependencies.baseUrl,
                            replacements: input.resourceReplacements,
                            session: context.session,
                            saveDate: context.saveDate,
                            credential: context.credential,
                        }, { signal: context.signal, onStatus })
                    if (outcome === null) return null
                    if (outcome.kind === 'waiting-for-reauthentication') {
                        pendingJobId = outcome.jobId
                        return {
                            kind: 'reauthentication-needed',
                            session: outcome.session,
                            warning: outcome.warning,
                        }
                    }
                    pendingJobId = null
                    const receipt = outcome.receipt
                    const publication = receipt.result.publication
                    if (publication.kind === 'auth-warning') {
                        await receipt.acknowledge()
                        return {
                            kind: 'auth-warning',
                            session: publication.session,
                            warning: publication.warning,
                        }
                    }
                    if (publication.kind === 'reauthentication-needed') {
                        await receipt.acknowledge()
                        throw new Error(
                            'Native official publication ended before reauthentication retry',
                        )
                    }
                    return {
                        kind: publication.kind,
                        session: publication.session,
                        replacementKey: publication.replacementKey,
                        warning: publication.kind === 'written' ? publication.warning : null,
                        reloadSession: publication.kind === 'written'
                            ? publication.reloadSession
                            : false,
                        receipt,
                    }
                },
                { signal: background.signal },
            )
        }
        catch (error) {
            if (
                pendingJobId !== null
                && !hasDrainedNativeOfficialPublicationCancellation(error)
            ) {
                try {
                    await cancelAttempt(pendingJobId)
                }
                catch {}
            }
            throw error
        }
        if (result === null) {
            if (pendingJobId === null) return null
            try {
                await cancelAttempt(pendingJobId)
            }
            catch {}
            throw new Error('Native official publication ended before reauthentication retry')
        }
        if (result.kind === 'auth-warning') {
            throw new Error('Official account authorization warning while writing database/database.bin')
        }
        return {
            databaseFingerprint: result.receipt.result.sourceSha256,
            acknowledge: result.receipt.acknowledge,
            completeReload: result.completeReload,
        }
    }
}

export interface NativeOfficialAssetPublisherDependencies {
    account: Pick<AccountStorage, 'writeOfficialDatabaseFromNative'>
    baseUrl: string
    run?(
        request: NativeOfficialAssetPublicationRequest,
        options?: NativeOfficialAssetPublicationOptions,
    ): Promise<NativeOfficialAssetPublicationResult>
}

/** Account sessions, warnings, reloads and reauthentication follow the native database write. */
export function createNativeOfficialAssetPublisher(
    dependencies: NativeOfficialAssetPublisherDependencies,
): OfficialNativeAssetPublisher {
    const run = dependencies.run ?? runNativeOfficialAssetPublication
    return async (input) => {
        if (!hasNativePersistentRevisionLease(input.lease)) return false
        let remaining = input.assets
        const result = await dependencies.account.writeOfficialDatabaseFromNative(async (context) => {
            const outcome = await run({
                baseUrl: dependencies.baseUrl,
                credential: context.credential,
                accountId: input.accountId,
                lease: input.lease[nativePersistentRevisionLease],
                session: context.session,
                assets: remaining,
            }, { signal: context.signal, guard: input.guard, written: input.written })
            const unwritten = new Set(outcome.unwritten)
            remaining = remaining.filter(asset => unwritten.has(asset.key))
            if (outcome.cancelled) {
                throw new DOMException('Official account asset publication was cancelled', 'AbortError')
            }
            const reported = { session: outcome.session, warning: outcome.warning }
            if (outcome.reason === 'reauthentication-needed') return { kind: 'reauthentication-needed', ...reported }
            if (outcome.reason === 'auth-warning') return { kind: 'auth-warning', ...reported }
            return {
                kind: 'written',
                ...reported,
                replacementKey: '',
                reloadSession: outcome.reloadSession,
                receipt: outcome,
            }
        }, { signal: input.background.signal })
        if (result === null) return false
        if (result.kind === 'auth-warning') {
            throw new Error('Official account authorization warning while writing assets')
        }
        await result.completeReload()
        if (result.receipt.reason || remaining.length > 0) {
            throw new Error('Official account asset publication did not complete')
        }
        return true
    }
}
