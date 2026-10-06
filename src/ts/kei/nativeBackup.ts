import { runWithMobileBackgroundTask, measuredTaskPercent } from '../mobileBackgroundTask'
import { invoke } from '@tauri-apps/api/core'

import { isTauri } from '../platform'
import {
    hasNativePersistentRevisionLease,
    nativePersistentRevisionLease,
} from '../storage/nativePersistentExport'
import type { PersistentDataRuntime } from '../storage/persistentDataRuntime'
import type { NativeFileJobStatus } from '../storage/nativeFileJobs'
import {
    releasePersistentRevisionLease,
    withPersistentRevisionLease,
} from '../storage/persistentRecordIterator'

export interface NativeKeiBackupRequest {
    runtime: PersistentDataRuntime
    url: string
    accountId: string
    token: string
}

export interface NativeKeiBackupDependencies {
    isTauri(): boolean
    invoke(command: string, args: Record<string, unknown>): Promise<unknown>
    wait?(milliseconds: number): Promise<void>
    warn?(warningCode: string): void
}

const productionDependencies: NativeKeiBackupDependencies = {
    isTauri: () => isTauri,
    invoke: (command, args) => invoke(command, args),
    wait: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
    warn: (warningCode) => console.warn(`Native KEI backup warning: ${warningCode}`),
}

export interface NativeKeiBackupJobOptions {
    signal?: AbortSignal
    pollIntervalMs?: number
}

export class NativeKeiBackupJobError extends Error {
    constructor(readonly code: string, message: string) {
        super(message)
        this.name = 'NativeKeiBackupJobError'
    }
}

function abortError(): Error {
    return new DOMException('Native KEI backup was cancelled', 'AbortError')
}

async function invokeJob(
    dependencies: NativeKeiBackupDependencies,
    command: string,
    args: Record<string, unknown>,
): Promise<unknown> {
    try {
        return await dependencies.invoke(command, args)
    }
    catch (error) {
        if (
            error
            && typeof error === 'object'
            && 'code' in error
            && typeof error.code === 'string'
            && 'message' in error
            && typeof error.message === 'string'
        ) {
            throw new NativeKeiBackupJobError(error.code, error.message)
        }
        throw error
    }
}

function isTerminal(status: NativeFileJobStatus): boolean {
    return status.state === 'succeeded'
        || status.state === 'failed'
        || status.state === 'cancelled'
}

export async function runNativeKeiBackupJob(
    request: NativeKeiBackupRequest,
    dependencies: NativeKeiBackupDependencies = productionDependencies,
    options: NativeKeiBackupJobOptions = {},
): Promise<boolean> {
    if (!dependencies.isTauri()) return false
    if (options.signal?.aborted) throw abortError()

    return runWithMobileBackgroundTask('backup', async background => {
        options = { ...options, signal: background.signal }
        await request.runtime.flushPendingData('kei-auto-backup')
        const revision = request.runtime.revision
        const lease = await request.runtime.store.acquireRevision(revision)
        let started: { jobId: string } | undefined
        let startError: unknown
        let releaseError: unknown
        try {
            if (!hasNativePersistentRevisionLease(lease)) {
                throw new NativeKeiBackupJobError('native-lease-unavailable', 'Native KEI backup requires a native revision lease')
            }
            started = await invokeJob(dependencies, 'native_file_job_start', {
                request: {
                    kind: 'kei-backup-upload',
                    lease: lease[nativePersistentRevisionLease],
                    expectedRevision: revision,
                    url: request.url,
                    expectedAccountId: request.accountId,
                    token: request.token,
                },
            }) as { jobId: string }
        }
        catch (error) {
            startError = error
        }
        finally {
            try {
                await releasePersistentRevisionLease(lease)
            } catch (error) {
                releaseError = error
            }
        }
        if (!started) {
            if (releaseError !== undefined) {
                if (startError !== undefined) {
                    console.error('Native KEI backup revision release failed after job start failed', releaseError)
                } else {
                    throw releaseError
                }
            }
            throw startError
        }
        if (releaseError !== undefined) {
            dependencies.warn?.('revision-release-failed')
            console.error('Native KEI backup revision release failed after the job was accepted', releaseError)
        }

        let cancellationRequested = false
        let terminal: NativeFileJobStatus | undefined
        try {
            while (!terminal) {
                if (options.signal?.aborted && !cancellationRequested) {
                    cancellationRequested = true
                    await invokeJob(dependencies, 'native_file_job_cancel', {
                        jobId: started.jobId,
                    })
                }
                const status = await invokeJob(dependencies, 'native_file_job_status', {
                    jobId: started.jobId,
                }) as NativeFileJobStatus
                background.progress(measuredTaskPercent(status.progress.completedBytes, status.progress.totalBytes))
                if (isTerminal(status)) {
                    terminal = status
                    break
                }
                await (dependencies.wait ?? productionDependencies.wait!)(
                    options.pollIntervalMs ?? 100,
                )
            }

            if (terminal.state === 'succeeded') {
                for (const warningCode of terminal.result?.warningCodes ?? []) {
                    dependencies.warn?.(warningCode)
                }
                return true
            }
            if (terminal.state === 'cancelled') throw abortError()
            throw new NativeKeiBackupJobError(
                terminal.error?.code ?? 'kei-backup-failed',
                terminal.error?.message ?? 'Native KEI backup failed',
            )
        }
        finally {
            try {
                await invokeJob(dependencies, 'native_file_job_forget', { jobId: started.jobId })
            }
            catch {}
        }
    }, options.signal)
}

export async function tryNativeKeiBackup(
    request: NativeKeiBackupRequest,
    dependencies: NativeKeiBackupDependencies = productionDependencies,
): Promise<boolean> {
    if (!dependencies.isTauri()) return false

    await request.runtime.flushPendingData('kei-auto-backup')
    const revision = request.runtime.revision
    const lease = await request.runtime.store.acquireRevision(revision)
    await withPersistentRevisionLease(lease, async () => {
        if (!hasNativePersistentRevisionLease(lease)) {
            throw new NativeKeiBackupJobError('native-lease-unavailable', 'Native KEI backup requires a native revision lease')
        }
        await dependencies.invoke('pds_kei_backup_upload', {
            lease: lease[nativePersistentRevisionLease],
            url: request.url,
            expectedAccountId: request.accountId,
            token: request.token,
        })
    })
    return true
}
