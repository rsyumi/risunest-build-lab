import '../androidNativeControl'
import { invoke } from '@tauri-apps/api/core'
import { relaunch } from '../desktopRelaunch'
import { isTauriIOS, isTauriMobile } from '../platform'
import { NativeFileJobError, runNativeSnapshotRestore, type NativeFileJobStatus, type NativeSnapshotBodiesStarted, type NativeSnapshotBodyResult, type NativeSnapshotRestoreActivation } from './nativeFileJobs'
import { nativeFileOperationOutcomeShown, runSharedNativeFileOperation } from './nativeFileJobManager'
import type {
    DataHealthResult,
    RepairApplied,
    RepairCandidate,
    RepairJournalSummary,
    RepairPreview,
    RepairUndone,
} from './dataHealth'

const PERIODIC_SNAPSHOT_INTERVAL_MS = 24 * 60 * 60 * 1000
const PERIODIC_SNAPSHOT_CHECK_INTERVAL_MS = 60 * 60 * 1000

export type NativeCheckpointMode = 'passive' | 'truncate'

export interface NativeSnapshotInfo {
    id: string
    reason: string
    reclaimableBytes: number
    bytes: number
    modifiedAt: number
}

export interface NativeSnapshotCreated {
    id: string
    revision: number
    bytes: number
    durationMs: number
}

export interface NativeStorageBytes {
    count: number
    bytes: number
}

export interface NativePersistentStorageStats {
    snapshotBytes: number
    databaseBytes: number
    assetObjects: NativeStorageBytes
    assetAliases: NativeStorageAliasStats[]
    pluginStorage: NativeStorageBytes
    characters: { active: NativeStorageBytes; trashedCount: number }
    conversations: { count: number; messageCount: number }
    assetObjectDeletions: NativeStorageDeletionStats[]
}

export interface NativeStorageAliasStats extends NativeStorageBytes {
    kind: string
    inlayType: string | null
}

export interface NativeStorageDeletionStats extends NativeStorageBytes {
    state: string
}

/** One stored file the cleanup looked at, and what it decided about it. */
export interface NativeAssetGcCandidate {
    objectHash: string
    bytes: number
    createdAtMs: number
    state: 'deletable' | 'recent' | 'held' | 'blocked'
    /** What is holding a kept file. Empty with `held` means the library still uses it. */
    holders: string[]
}

export interface NativeAssetGcResult {
    candidateCount: number
    candidateBytes: number
    deletedCount: number
    deletedBytes: number
    blockers: string[]
    candidates?: NativeAssetGcCandidate[]
    omitted?: number
}

export interface NativeSnapshotRestoreActions {
    choose(snapshots: readonly NativeSnapshotInfo[]): Promise<string | null>
    onEmpty(): void | Promise<void>
}

interface NativeRestartBridge {
    requestRestart?: () => void
}

function nativeRestartBridge(): NativeRestartBridge | undefined {
    return (
        window as Window & {
            RisuLifecycleBridge?: NativeRestartBridge
        }
    ).RisuLifecycleBridge
}

export async function checkpointNativePersistentStore(mode: NativeCheckpointMode): Promise<void> {
    await invoke('pds_checkpoint', { mode })
}

export function createNativePersistentSnapshot(reason: string): Promise<NativeSnapshotCreated> {
    return invoke('pds_snapshot_create', { reason })
}

export function listNativePersistentSnapshots(): Promise<NativeSnapshotInfo[]> {
    return invoke('pds_snapshot_list')
}

export function getNativePersistentStorageStats(): Promise<NativePersistentStorageStats> {
    return invoke('pds_storage_stats')
}

export function deleteNativePersistentSnapshot(id: string): Promise<void> {
    return invoke('pds_snapshot_delete', { id })
}

export function previewNativePersistentAssetGc(): Promise<NativeAssetGcResult> {
    return invoke('pds_asset_gc_preview')
}

export function executeNativePersistentAssetGc(): Promise<NativeAssetGcResult> {
    return invoke('pds_asset_gc_execute')
}

export function scanNativeDataHealth(): Promise<DataHealthResult> {
    return invoke('pds_data_health_scan')
}

export function deepScanNativeDataHealth(resume: boolean): Promise<DataHealthResult> {
    return invoke('pds_data_health_deep_scan', { resume })
}

export function getNativeDataHealthResult(): Promise<DataHealthResult | null> {
    return invoke('pds_data_health_result')
}

export async function cancelNativeDataHealthScan(): Promise<void> {
    await invoke('pds_data_health_cancel')
}

export function planNativeDataHealthRepair(): Promise<RepairCandidate[]> {
    return invoke('pds_data_health_repair_plan')
}

export function previewNativeDataHealthRepair(selection: string[], expectedScannedAt: number): Promise<RepairPreview> {
    return invoke('pds_data_health_repair_preview', { selection, expectedScannedAt })
}

export function applyNativeDataHealthRepair(
    selection: string[],
    snapshot: boolean,
    expectedRevision: number,
    expectedScannedAt: number,
): Promise<RepairApplied> {
    return invoke('pds_data_health_repair_apply', { selection, snapshot, expectedRevision, expectedScannedAt })
}

export function listNativeDataHealthJournals(): Promise<RepairJournalSummary[]> {
    return invoke('pds_data_health_journals')
}

export function undoNativeDataHealthRepair(journalId: string, expectedRevision: number): Promise<RepairUndone> {
    return invoke('pds_data_health_undo', { journalId, expectedRevision })
}

export class NativeSnapshotBodiesCommittedError extends NativeFileJobError {
    constructor(public readonly receipt: Pick<NativeSnapshotBodiesStarted,'stagingId'|'activationRevision'|'bindingAuthority'> & Partial<Pick<NativeSnapshotBodiesStarted,'jobId'|'kind'>>, public readonly bodies: NativeSnapshotBodyResult | undefined, code: string, message: string) {
        super(code, message)
        this.name = 'NativeSnapshotBodiesCommittedError'
    }
}

export async function completeNativeSnapshotRestoreBodies(stagingId: string, activationRevision: number, bindingAuthority: string): Promise<NativeSnapshotBodyResult> {
    const identity={stagingId,activationRevision:String(activationRevision),bindingAuthority}
    let receipt:NativeSnapshotBodiesStarted
    try {receipt=await invoke<NativeSnapshotBodiesStarted>('native_snapshot_restore_bodies_start',identity)}
    catch {throw new NativeSnapshotBodiesCommittedError(identity,undefined,'snapshot-body-start-unknown','Snapshot library is restored, but body transfer could not be confirmed')}
    return runSharedNativeFileOperation('import', `snapshot-bodies:${stagingId}`, async context => {
        let cancelled = false
        while (true) {
            if (context.signal.aborted && !cancelled) {
                cancelled = true
                await invoke('native_file_job_cancel', {jobId: receipt.jobId})
            }
            const status = await invoke<NativeFileJobStatus>('native_file_job_status', {jobId: receipt.jobId})
            context.onStatus(status)
            if (status.kind !== 'snapshot-bodies' || status.snapshotStagingId !== stagingId
                || status.activationRevision !== activationRevision || status.activationAuthority !== bindingAuthority) {
                throw new NativeSnapshotBodiesCommittedError(receipt, status.snapshotBodies, 'snapshot-body-receipt-mismatch', 'Snapshot library is restored, but the body transfer receipt differs')
            }
            if (status.state === 'succeeded') {
                const bodies = status.snapshotBodies
                if (!bodies?.settled || bodies.stageId !== stagingId || bodies.activatedRevision !== activationRevision || bodies.bindingAuthority !== bindingAuthority) {
                    throw new NativeSnapshotBodiesCommittedError(receipt, bodies, 'snapshot-body-result-mismatch', 'Snapshot library is restored, but body completion could not be confirmed')
                }
                return bodies
            }
            if (status.state === 'failed' || status.state === 'cancelled') {
                throw new NativeSnapshotBodiesCommittedError(receipt, status.snapshotBodies, status.error?.code ?? 'snapshot-bodies-incomplete', status.error?.message ?? 'Snapshot library is restored, but body transfer is incomplete')
            }
            await new Promise(resolve => setTimeout(resolve, 100))
        }
    }, {presentation: 'dialog', format: 'library-backup', snapshotBodyOwner: receipt})
}

export async function requestNativePersistentSnapshotRestore(id: string): Promise<boolean> {
    const {getPersistentDataRuntime} = await import('./persistentDataRuntime.svelte')
    const runtime = getPersistentDataRuntime()
    const copyBodies = async (activation: NativeSnapshotRestoreActivation): Promise<void> => {
        await completeNativeSnapshotRestoreBodies(activation.stagingId, activation.activationRevision, activation.bindingAuthority)
    }
    let activation: NativeSnapshotRestoreActivation
    try {
        activation = await runSharedNativeFileOperation('import', `snapshot-restore:${id}`, context => runNativeSnapshotRestore(runtime, {snapshotId: id}, {
            signal: context.signal,
            onStatus: context.onStatus,
            onBlockingChange: context.setBlocking,
            afterActivationRecovery: copyBodies,
        }), {presentation: 'dialog', format: 'library-backup'})
    } catch (error) {
        // The operation dialog already shows how an admitted restore ended.
        if ((error instanceof DOMException && error.name === 'AbortError') || nativeFileOperationOutcomeShown('import')) return false
        throw error
    }
    await copyBodies(activation)
    return true
}

export async function restartNativeApp(): Promise<void> {
    if (isTauriIOS) {
        await invoke('ios_prepare_restart')
        window.location.reload()
        return
    }
    if (!isTauriMobile) {
        await relaunch()
        return
    }

    const bridge = nativeRestartBridge()
    if (typeof bridge?.requestRestart !== 'function') {
        throw new Error('Android restart bridge is unavailable')
    }
    bridge.requestRestart()
}

export async function createPeriodicNativeSnapshotIfDue(
    now = Date.now(),
): Promise<NativeSnapshotCreated | null> {
    const snapshots = await listNativePersistentSnapshots()
    const newestModifiedAt = snapshots.reduce(
        (newest, snapshot) => Math.max(newest, snapshot.modifiedAt),
        Number.NEGATIVE_INFINITY,
    )
    if (
        newestModifiedAt <= now
        && now - newestModifiedAt < PERIODIC_SNAPSHOT_INTERVAL_MS
    ) return null
    return createNativePersistentSnapshot('periodic')
}

export function sweepNativeMessageObjects(): Promise<void> {
    return invoke('pds_message_object_sweep')
}

export function schedulePeriodicNativeSnapshot(): void {
    const run = () => {
        void createPeriodicNativeSnapshotIfDue()
            .catch((error) => {
                console.error('Periodic native snapshot failed', error)
            })
            .then(() => sweepNativeMessageObjects())
            .catch((error) => {
                console.error('Periodic native object sweep failed', error)
            })
    }
    if (typeof globalThis.requestIdleCallback === 'function') {
        globalThis.requestIdleCallback(run)
    } else {
        globalThis.setTimeout(run, 0)
    }
    globalThis.setInterval(run, PERIODIC_SNAPSHOT_CHECK_INTERVAL_MS)
}

export async function restoreNativePersistentSnapshot(
    actions: NativeSnapshotRestoreActions,
): Promise<boolean> {
    const snapshots = await listNativePersistentSnapshots()
    if (snapshots.length === 0) {
        await actions.onEmpty()
        return false
    }

    const id = await actions.choose(snapshots)
    if (id === null) return false
    if (!snapshots.some((snapshot) => snapshot.id === id)) {
        throw new Error('Selected native snapshot is not available')
    }
    return requestNativePersistentSnapshotRestore(id)
}
