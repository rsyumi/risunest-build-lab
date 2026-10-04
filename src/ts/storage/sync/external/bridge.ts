import { listen } from '@tauri-apps/api/event'
import { Channel, invoke } from '@tauri-apps/api/core'
import { isTauri } from '../../../platform'
import type {
    ExternalSnapshotExportProgress,
    DecimalString,
    ExternalConnectionResult,
    ExternalConnectionConfig,
    ExternalHistoryPage,
    ExternalHistoryDeletePreparation,
    ExternalJobSummary,
    ExternalReceivedApplicationResult,
    ExternalProviderDescriptor,
    ExternalProviderSecretInput,
    ExternalQuotaSummary,
    ExternalConnectionSettingsMaterial,
    ExternalRetentionPolicy,
    ExternalStorageState,
    ExternalSnapshotExportResult,
    LibrarySyncSelection,
    PendingExternalAuthorization,
    PreparedExternalConnection,
    PrepareExternalConnectionRequest,
    StartExternalJobRequest,
    ExternalExitCapture,
    ExternalAuthorizationOutcome,
    ExternalFolderPage,
    ExternalFolderSelection,
} from './types'

export class ExternalStorageUnsupportedError extends Error {
    readonly code = 'external-storage-unsupported'

    constructor() {
        super('External storage is available in the native app only.')
        this.name = 'ExternalStorageUnsupportedError'
    }
}

export interface ExternalStorageBridgeDependencies {
    supported(): boolean
    invoke(command: string, args?: Record<string, unknown>): Promise<unknown>
}

const productionDependencies: ExternalStorageBridgeDependencies = {
    supported: () => isTauri,
    invoke: (command, args) => invoke(command, args),
}

export const unsupportedExternalStorageState: ExternalStorageState = {
    supported: false,
    selection: {
        kind: 'none',
        selectionEpoch: '0',
        paused: false,
    },
    connections: [],
    jobs: [],
}

export class ExternalStorageBridge {
    constructor(
        private readonly dependencies: ExternalStorageBridgeDependencies = productionDependencies,
    ) {}

    get supported(): boolean {
        return this.dependencies.supported()
    }

    private native<T>(command: string, args?: Record<string, unknown>): Promise<T> {
        if (!this.supported) return Promise.reject(new ExternalStorageUnsupportedError())
        return this.dependencies.invoke(command, args) as Promise<T>
    }

    getState(): Promise<ExternalStorageState> {
        if (!this.supported) return Promise.resolve(unsupportedExternalStorageState)
        return this.native('external_storage_get_state')
    }

    onJobStarted(listener: () => void): Promise<() => void> {
        if (!this.supported) return Promise.resolve(() => {})
        return listen('external-storage-job-started', listener)
    }

    prepareRenewal(connectionId: string): Promise<PreparedExternalConnection> {
        return this.native('external_storage_prepare_renewal', { connectionId })
    }

    unlockConnection(connectionId: string, recoveryKey: string): Promise<void> {
        return this.native('external_storage_unlock_connection', { connectionId, recoveryKey })
    }

    setSyncPaused(paused: boolean, expectedSelectionEpoch: string): Promise<LibrarySyncSelection> {
        return this.native('external_storage_set_sync_paused', { request: { paused, expectedSelectionEpoch } })
    }

    setAutomaticBackupPaused(connectionId: string, paused: boolean): Promise<void> {
        return this.native('external_storage_set_automatic_backup_paused', { connectionId, paused })
    }

    captureExitTarget(): Promise<ExternalExitCapture> {
        return this.native('external_storage_capture_exit_target')
    }

    listProviders(): Promise<ExternalProviderDescriptor[]> {
        return this.native('external_storage_list_providers')
    }

    validateSyncRoot(config: ExternalConnectionConfig): Promise<void> {
        return this.native('external_storage_validate_sync_root', { config })
    }

    prepareConnection(request: PrepareExternalConnectionRequest): Promise<PreparedExternalConnection> {
        return this.native('external_storage_prepare_connection', { request })
    }

    commitConnection(
        preparationId: string,
        secret?: ExternalProviderSecretInput,
    ): Promise<ExternalConnectionResult> {
        return this.native('external_storage_commit_connection', {
            request: { preparationId, secret },
        })
    }

    beginAuthorization(
        preparationId: string,
        currentPlatformClientId?: string,
    ): Promise<PendingExternalAuthorization> {
        return this.native('external_storage_begin_authorization', {
            request: {
                preparationId,
                ...(currentPlatformClientId ? { currentPlatformClientId } : {}),
            },
        })
    }

    completeAuthorization(
        authorizationId: string,
        redirectUrl?: string,
        clientSecret?: string,
    ): Promise<ExternalAuthorizationOutcome> {
        return this.native('external_storage_complete_authorization', {
            request: {
                authorizationId,
                ...(redirectUrl ? { redirectUrl } : {}),
                ...(clientSecret ? { clientSecret } : {}),
            },
        })
    }

    listFolders(request: { selectionId: string; folder?: string; cursor?: string }): Promise<ExternalFolderPage> {
        return this.native('external_storage_list_folders', { request })
    }

    selectFolder(request: { selectionId: string; folder: string }): Promise<ExternalFolderSelection> {
        return this.native('external_storage_select_folder', { request })
    }

    cancelFolderSelection(selectionId: string): Promise<void> {
        return this.native('external_storage_cancel_folder_selection', { selectionId })
    }

    cancelAuthorization(authorizationId: string): Promise<void> {
        return this.native('external_storage_cancel_authorization', { authorizationId })
    }



    setRetentionPolicy(connectionId: string, policy: ExternalRetentionPolicy): Promise<void> {
        return this.native('external_storage_set_retention_policy', { connectionId, policy })
    }

    removeConnection(connectionId: string): Promise<void> {
        return this.native('external_storage_remove_connection', { connectionId })
    }

    setSyncTarget(
        connectionId: string | null,
        expectedSelectionEpoch: string,
    ): Promise<LibrarySyncSelection> {
        return this.native('external_storage_set_sync_target', {
            request: { connectionId, expectedSelectionEpoch },
        })
    }

    startJob(request: StartExternalJobRequest, jobId?: string): Promise<ExternalJobSummary> {
        return this.native<ExternalJobSummary>('external_storage_start_job', {
            request, ...(jobId === undefined ? {} : { jobId }),
        })
    }

    cancelJob(jobId: string): Promise<ExternalJobSummary> {
        return this.native('external_storage_cancel_job', { jobId })
    }

    stopRestore(jobId: string): Promise<ExternalJobSummary> {
        return this.native('external_storage_stop_restore', { jobId })
    }

    getJob(jobId: string): Promise<ExternalJobSummary> {
        return this.native('external_storage_get_job', { jobId })
    }

    confirmRestoreAdoption(jobId: string, receivedRevision: DecimalString, selectedCharacterId?: string): Promise<ExternalReceivedApplicationResult> {
        return this.native('external_storage_confirm_restore_adoption', {
            request: { jobId, receivedRevision, selectedCharacterId: selectedCharacterId ?? null },
        })
    }


    setExecutionSession(request: {
        kind: 'foreground' | 'hidden'
        id: string
    }): Promise<void> {
        return this.native('external_storage_set_execution_session', {
            request,
        })
    }

    listHistory(connectionId: string, cursor?: string): Promise<ExternalHistoryPage> {
        return this.native('external_storage_list_history', {
            request: { connectionId, ...(cursor ? { cursor } : {}) },
        })
    }

    prepareHistoryDelete(
        connectionId: string,
        pointId: string,
        pointObservation: string,
    ): Promise<ExternalHistoryDeletePreparation> {
        return this.native('external_storage_prepare_history_delete', {
            request: { connectionId, pointId, pointObservation },
        })
    }

    getQuota(connectionId: string): Promise<ExternalQuotaSummary> {
        return this.native('external_storage_get_quota', { connectionId })
    }

    beginConnectionSettingsExport(connectionId: string): Promise<ExternalConnectionSettingsMaterial> {
        return this.native('external_storage_begin_connection_settings_export', { connectionId })
    }

    saveConnectionSettingsFile(transferId: string): Promise<void> {
        return this.native('external_storage_save_connection_settings_file', { transferId })
    }

    exportSnapshot(connectionId: string, snapshotId: string, exportId: string,
        onProgress: (progress: ExternalSnapshotExportProgress) => void): Promise<ExternalSnapshotExportResult> {
        if (!this.supported) return Promise.reject(new ExternalStorageUnsupportedError())
        const progress = new Channel<ExternalSnapshotExportProgress>()
        progress.onmessage = onProgress
        return this.native('external_storage_export_snapshot', { request: { connectionId, snapshotId, exportId }, progress })
    }

    cancelExport(exportId: string): Promise<void> {
        return this.native('external_storage_cancel_export', { exportId })
    }


    prepareConnectionSettingsImport(payload: string, recoveryKey: string): Promise<PreparedExternalConnection> {
        return this.native('external_storage_prepare_connection_settings_import', {
            request: { payload, recoveryKey },
        })
    }
}

let productionBridge: ExternalStorageBridge | undefined

export function getExternalStorageBridge(): ExternalStorageBridge {
    productionBridge ??= new ExternalStorageBridge()
    return productionBridge
}
