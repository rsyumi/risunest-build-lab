import { invoke } from '@tauri-apps/api/core'

import { getAndroidSafExportSourceId } from './androidSafBridge'
import { releaseCasJob } from './nativeAssetRepository'
import { isTerminalJob as isTerminal, type NativeFileJobStatus } from './nativeFileJobs'

export interface NativeFileJobRecoveryDependencies {
    invoke(command: string, args?: Record<string, unknown>): Promise<unknown>
    wait(milliseconds: number): Promise<void>
    androidSafExportId?(): string | null | Promise<string | null>
}

export interface NativeFileJobRecoveryResult {
    pendingRestoreAcknowledgements: string[]
    pendingOfficialPublications: string[]
    interruptedRestores: string[]
    libraryRestoreCommitted: boolean
    pendingSnapshotBodies: NativeFileJobStatus[]
}

export interface NativeFileJobRecoveryOptions {
    reconcileRestores?: boolean
}

const productionDependencies: NativeFileJobRecoveryDependencies = {
    invoke: (command, args) => args === undefined ? invoke(command) : invoke(command, args),
    wait: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
    androidSafExportId: () => getAndroidSafExportSourceId(),
}

// Managed handoff filename grammar per export job kind. Mirrors the Kotlin
// SafFileBridge regexes and the Rust handoff cleanup naming; the alignment is
// pinned by the shared taxonomy golden fixture
// (tests/fixtures/nativeFileTaxonomyV1Golden.json).
export const ANDROID_SAF_HANDOFF_ID_PATTERNS: Partial<
    Record<NativeFileJobStatus['kind'], RegExp>
> = {
    'export-raw-recovery':
        /(?:^|[\\/])risunest-rescue-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\.risunest-rescue\.zip$/,
    'export-portable-backup':
        /(?:^|[\\/])risunest-backup-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\.risunest$/,
    'export-compatible-local-backup':
        /(?:^|[\\/])risu-backup-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\.bin$/,
    'export-character-charx':
        /(?:^|[\\/])risu-charx-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\.(?:charx|jpeg)$/,
    'export-character-card':
        /(?:^|[\\/])risu-character-card-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\.(?:json|png)$/,
    'export-risu-module':
        /(?:^|[\\/])risu-module-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\.risum$/,
    'export-dataset':
        /(?:^|[\\/])risu-dataset-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})\.json$/,
}

function androidSafHandoffId(status: NativeFileJobStatus): string | null {
    const path = status.result?.handoffPath
    if (!path) return null
    return ANDROID_SAF_HANDOFF_ID_PATTERNS[status.kind]?.exec(path)?.[1] ?? null
}

export function shouldReconcileNativeFileJobs(isTauri: boolean): boolean {
    return isTauri
}

async function reconcileRestore(
    initial: NativeFileJobStatus,
    dependencies: NativeFileJobRecoveryDependencies,
): Promise<NativeFileJobStatus> {
    let status = initial
    if (!isTerminal(status) && status.phase !== 'activating-database') {
        await dependencies.invoke('native_file_job_cancel', { jobId: status.jobId })
    }
    while (!isTerminal(status)) {
        await dependencies.wait(100)
        status = await dependencies.invoke('native_file_job_status', {
            jobId: status.jobId,
        }) as NativeFileJobStatus
    }
    return status
}

async function reconcileExportInBackground(
    initial: NativeFileJobStatus,
    dependencies: NativeFileJobRecoveryDependencies,
): Promise<void> {
    let status = initial
    let retainNativeJob = false
    while (!isTerminal(status)) {
        await dependencies.wait(100)
        status = await dependencies.invoke('native_file_job_status', {
            jobId: status.jobId,
        }) as NativeFileJobStatus
    }
    try {
        const handoffId = androidSafHandoffId(status)
        if (handoffId && await dependencies.androidSafExportId?.() === handoffId) {
            retainNativeJob = true
            return
        }
        if (
            (status.kind === 'export-raw-recovery' ||
                status.kind === 'export-compatible-local-backup') &&
            status.result?.handoffPath
        ) {
            retainNativeJob = true
            await dependencies.invoke(
                status.kind === 'export-raw-recovery'
                    ? 'native_raw_recovery_handoff_cleanup'
                    : 'native_legacy_backup_handoff_cleanup', {
                path: status.result.handoffPath,
            })
            retainNativeJob = false
        } else if (
            status.kind === 'export-character-charx' &&
            status.result?.handoffPath
        ) {
            retainNativeJob = true
            await dependencies.invoke(
                'native_character_charx_handoff_cleanup',
                {
                    path: status.result.handoffPath,
                },
            )
            retainNativeJob = false
        } else if (
            status.kind === 'export-character-card' &&
            status.result?.handoffPath
        ) {
            retainNativeJob = true
            await dependencies.invoke('native_character_card_handoff_cleanup', {
                path: status.result.handoffPath,
            })
            retainNativeJob = false
        } else if (
            status.kind === 'export-risu-module' &&
            status.result?.handoffPath
        ) {
            retainNativeJob = true
            await dependencies.invoke('native_risu_module_handoff_cleanup', {
                path: status.result.handoffPath,
            })
            retainNativeJob = false
        } else if (
            status.kind === 'export-dataset' &&
            status.result?.handoffPath
        ) {
            retainNativeJob = true
            await dependencies.invoke('native_dataset_export_handoff_cleanup', {
                path: status.result.handoffPath,
            })
            retainNativeJob = false
        }
    }
    finally {
        if (!retainNativeJob) {
            await dependencies.invoke('native_file_job_forget', { jobId: status.jobId })
        }
    }
}

async function discardContentJob(
    initial: NativeFileJobStatus,
    dependencies: NativeFileJobRecoveryDependencies,
): Promise<void> {
    let status = initial
    if (!isTerminal(status)) {
        await dependencies.invoke('native_file_job_cancel', { jobId: status.jobId })
        do {
            status = await dependencies.invoke('native_file_job_status', {
                jobId: status.jobId,
            }) as NativeFileJobStatus
            if (!isTerminal(status)) await dependencies.wait(100)
        } while (!isTerminal(status))
    }
    if (status.kind === 'prepare-content-import' && status.state === 'succeeded') {
        await releaseCasJob(status.jobId, 'aborted', dependencies.invoke)
    }
    await dependencies.invoke('native_file_job_forget', { jobId: status.jobId })
}

function assertNever(value: never): never {
    throw new Error(`Unsupported native file job kind: ${String(value)}`)
}

export async function reconcileNativeFileJobsBeforeBootstrap(
    dependencies: NativeFileJobRecoveryDependencies = productionDependencies,
    options: NativeFileJobRecoveryOptions = {},
): Promise<NativeFileJobRecoveryResult> {
    const jobs = await dependencies.invoke('native_file_job_list') as NativeFileJobStatus[]
    const pendingRestoreAcknowledgements: string[] = []
    const pendingOfficialPublications: string[] = []
    const interruptedRestores: string[] = []
    const pendingSnapshotBodies: NativeFileJobStatus[] = []
    let libraryRestoreCommitted = false
    for (const job of jobs) {
        const kind = job.kind
        switch (kind) {
            case 'restore-block-risu-save':
            case 'restore-official-account-snapshot':
            case 'restore-native-snapshot':
            case 'restore-portable-backup':
            case 'restore-legacy-local-backup': {
                if (job.kind === 'restore-portable-backup' && job.restoreAdoptionConfirmed === true
                    && (job.portableBodyRetry?.pending || job.phase === 'copying-missing-bodies')) break
                if (options.reconcileRestores === false) break
                const terminal = isTerminal(job)
                    ? job
                    : await reconcileRestore(job, dependencies)
                if (terminal.state === 'succeeded') {
                    pendingRestoreAcknowledgements.push(terminal.jobId)
                    libraryRestoreCommitted ||= terminal.kind !== 'restore-portable-backup'
                        || terminal.replacesLibrary === true
                } else {
                    interruptedRestores.push(terminal.jobId)
                    await dependencies.invoke('native_file_job_forget', {
                        jobId: terminal.jobId,
                    })
                }
                break
            }
            case 'export-portable-backup':
                // The app-owned export intent resumes publication after the
                // maintenance navigation. Never discard its SAF handoff here.
                break
            case 'snapshot-bodies':
                pendingSnapshotBodies.push(job)
                break
            case 'export-block-risu-save':
            case 'export-raw-recovery':
            case 'export-compatible-local-backup':
            case 'export-character-charx':
            case 'export-character-card':
            case 'export-risu-module':
            case 'export-dataset':
            case 'kei-backup-upload':
                void reconcileExportInBackground(job, dependencies).catch(
                    (error) => {
                        console.error(
                            'Native export reconciliation failed',
                            error,
                        )
                    },
                )
                break
            case 'prepare-content-import':
            case 'import-jpeg-asset':
                await discardContentJob(job, dependencies)
                break
            case 'official-publication-upload':
                pendingOfficialPublications.push(job.jobId)
                break
            default:
                assertNever(kind)
        }
    }
    return {
        pendingRestoreAcknowledgements,
        pendingOfficialPublications,
        interruptedRestores,
        libraryRestoreCommitted,
        pendingSnapshotBodies,
    }
}

export async function acknowledgeRecoveredNativeRestores(
    jobIds: string[],
    dependencies: NativeFileJobRecoveryDependencies = productionDependencies,
): Promise<void> {
    for (const jobId of jobIds) {
        await dependencies.invoke('native_file_job_forget', { jobId })
    }
}

export async function listNativeOfficialPublicationJobs(
    dependencies: NativeFileJobRecoveryDependencies = productionDependencies,
): Promise<string[]> {
    const jobs = await dependencies.invoke('native_file_job_list') as NativeFileJobStatus[]
    return jobs
        .filter((job) => job.kind === 'official-publication-upload')
        .map((job) => job.jobId)
}
