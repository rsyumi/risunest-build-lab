import { runSharedNativeFileOperation } from '../nativeFileJobManager'
import { invoke } from '@tauri-apps/api/core'
import { runWithMobileBackgroundTask, measuredTaskPercent } from '../../mobileBackgroundTask'
import type { AccountStorage } from '../accountStorage'
import { listNativeOfficialPublicationJobs } from '../nativeFileJobRecovery'
import {
    resumeNativeOfficialPublication,
    isTerminalJob,
    type NativeFileJobStatus,
    type NativeFileJobOptions,
    type NativeOfficialPublicationReceipt,
} from '../nativeFileJobs'
import type {
    OfficialAccountSnapshotAdapter,
    OfficialRecoveredPublication,
} from './officialAccountSnapshot'

export interface NativeOfficialPublicationRecoveryDependencies {
    activeAccountId(): string | null
    account: Pick<AccountStorage, 'adoptRecoveredOfficialWrite'>
    adapter: Pick<OfficialAccountSnapshotAdapter, 'adoptPublishedRevision'>
    flushMetadata(): Promise<void>
    listJobIds?(): Promise<string[]>
    statusJob?(jobId: string): Promise<NativeFileJobStatus>
    resumeJob?(jobId: string, options?: NativeFileJobOptions): Promise<NativeOfficialPublicationReceipt | null>
}

export interface NativeOfficialPublicationRecovery {
    reconcile(present?: boolean): Promise<void>
    reconcileSettled(): Promise<void>
    hasPending(): boolean
    takeRecoveredPublication(
        accountId: string,
        revision: number,
    ): OfficialRecoveredPublication | null
}

export function createNativeOfficialPublicationRecovery(
    initialJobIds: readonly string[],
    dependencies: NativeOfficialPublicationRecoveryDependencies,
): NativeOfficialPublicationRecovery {
    const pending = new Set(initialJobIds)
    const listJobIds = dependencies.listJobIds ?? listNativeOfficialPublicationJobs
    const resumeJob = dependencies.resumeJob ?? resumeNativeOfficialPublication
    const statusJob = dependencies.statusJob ?? ((jobId: string) => invoke<NativeFileJobStatus>('native_file_job_status', { jobId }))
    const recoveredPublications = new Map<string, OfficialRecoveredPublication>()
    let inFlight: Promise<void> | null = null
    const recoveredKey = (accountId: string, revision: number) => `${accountId}\0${revision}`

    const reconcileOnce = async (settledOnly: boolean, options?: NativeFileJobOptions) => {
        for (const jobId of await listJobIds()) pending.add(jobId)
        for (const jobId of [...pending]) {
            if (settledOnly) {
                const status = await statusJob(jobId)
                const awaitingRetry = status.state === 'waitingForInput'
                    && status.phase === 'awaiting-publication-retry'
                if (!isTerminalJob(status) && !awaitingRetry) continue
            }
            const receipt = await resumeJob(jobId, options)
            if (!receipt) {
                pending.delete(jobId)
                continue
            }
            const publication = receipt.result.publication
            const activeAccountId = dependencies.activeAccountId()
            const authenticationOutcome = publication.kind === 'auth-warning'
                || publication.kind === 'reauthentication-needed'
            if (!activeAccountId || publication.accountId !== activeAccountId) {
                if (authenticationOutcome) {
                    await receipt.acknowledge()
                    pending.delete(jobId)
                }
                continue
            }
            if (authenticationOutcome) {
                dependencies.account.adoptRecoveredOfficialWrite({
                    session: publication.session,
                    warning: publication.warning,
                })
                await receipt.acknowledge()
                pending.delete(jobId)
                continue
            }

            const completion = dependencies.account.adoptRecoveredOfficialWrite({
                session: publication.session,
                warning: publication.kind === 'written' ? publication.warning : null,
                reloadSession: publication.kind === 'written'
                    ? publication.reloadSession
                    : false,
            })
            await dependencies.adapter.adoptPublishedRevision({
                accountId: publication.accountId,
                revision: receipt.result.revision,
                databaseFingerprint: receipt.result.sourceSha256,
            })
            await dependencies.flushMetadata()
            await receipt.acknowledge()
            pending.delete(jobId)
            const recovered = {
                accountId: publication.accountId,
                revision: receipt.result.revision,
                databaseFingerprint: receipt.result.sourceSha256,
            }
            recoveredPublications.set(
                recoveredKey(recovered.accountId, recovered.revision),
                recovered,
            )
            await completion.completeReload()
        }
    }

    return {
        reconcile(present = false) {
            if (inFlight) return inFlight
            const operation = present && pending.size > 0
                ? runSharedNativeFileOperation('export', 'official-publication-recovery', context =>
                    reconcileOnce(false, { signal: context.signal, onStatus: context.onStatus }),
                    { presentation: 'dialog', format: 'library-backup', userInitiated: false },
                )
                : runWithMobileBackgroundTask('sync', task => reconcileOnce(false, {
                    signal: task.signal,
                    onStatus: status => task.progress(measuredTaskPercent(status.progress.completedBytes, status.progress.totalBytes)),
                }))
            inFlight = operation.finally(() => {
                inFlight = null
            })
            return inFlight
        },
        async reconcileSettled() {
            if (inFlight) return
            inFlight = reconcileOnce(true).finally(() => { inFlight = null })
            return inFlight
        },
        hasPending: () => pending.size > 0,
        takeRecoveredPublication(accountId, revision) {
            const key = recoveredKey(accountId, revision)
            const recovered = recoveredPublications.get(key) ?? null
            recoveredPublications.delete(key)
            return recovered
        },
    }
}
