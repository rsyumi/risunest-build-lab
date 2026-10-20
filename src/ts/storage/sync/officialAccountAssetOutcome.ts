export interface OfficialAccountAssetCounts {
    required: number
    alreadyPresent: number
    installed: number
    missing: number
    failed: number
    notAttempted: number
}

export interface OfficialAccountAssetProgress {
    phase: 'checking' | 'downloading' | 'finishing'
    state: 'running' | 'succeeded' | 'failed' | 'cancelled'
    counts: OfficialAccountAssetCounts
    reason?: 'authentication' | 'local' | 'account-changed' | 'library-changed' | 'service' | 'files' | 'receipt' | 'checking'
    operationId?: string
    transfer?: import('./officialAccountTransfer').OfficialAccountTransferProgress
}

export interface OfficialAccountAssetOutcome extends OfficialAccountAssetProgress {
    failedKeys: string[]
}

export interface OfficialAccountAssetOptions {
    operationId?: string
    signal?: AbortSignal
    onRestoreStatus?(status: import('../nativeFileJobs').NativeFileJobStatus | undefined): void
    onProgress?(progress: OfficialAccountAssetProgress): void
    previousFailedKeys?: readonly string[]
}

export function emptyOfficialAccountAssetCounts(): OfficialAccountAssetCounts {
    return { required: 0, alreadyPresent: 0, installed: 0, missing: 0, failed: 0, notAttempted: 0 }
}
