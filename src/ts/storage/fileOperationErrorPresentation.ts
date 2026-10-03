import { get } from 'svelte/store'
import { language } from 'src/lang'
import { alertError } from '../alert'
import { NativeFileOperationBusyError, nativeFileOperationOutcome, type NativeFileOperationKind } from './nativeFileJobManager'
import { NativeFileJobActivationCommittedError } from './nativeFileJobs'
import { alertPartialDestinationWarning, hasPartialDestinationWarning } from './risuSaveFileRoute'

export function fileOperationErrorWasPresented(kind: NativeFileOperationKind, callStartedAt: number): boolean {
    const outcome = get(nativeFileOperationOutcome)
    return outcome?.kind === kind && outcome.startedAt >= callStartedAt
}

/** A managed operation owns its terminal dialog; callers own admission failures. */
export function presentFileOperationError(kind: NativeFileOperationKind, error: unknown, callStartedAt: number, options: { fallbackMessage?: string; committedMessage?: string } = {}): void {
    if (fileOperationErrorWasPresented(kind, callStartedAt)) return
    if (error instanceof DOMException && error.name === 'AbortError') {
        alertPartialDestinationWarning(error, language.screenshotPartialDestinationMayRemain, alertError)
        return
    }
    const code = error && typeof error === 'object' && 'code' in error ? error.code : error instanceof Error ? error.message : undefined
    const text = language.risuNest.backup
    const message = error instanceof NativeFileOperationBusyError ? text.fileBusy
        : code === 'generation-active' ? text.generationBusy
        : ['server-sync-busy', 'library-operation-busy', 'library-file-operation-busy'].includes(String(code)) ? text.syncBusy
        : ['resolve-pending-operation-first', 'server-status-unavailable'].includes(String(code)) ? text.syncUnconfirmed
        : code === 'sync-unavailable' ? text.syncUnavailable
        : error instanceof NativeFileJobActivationCommittedError ? options.committedMessage ?? language.risuSaveImportCommittedRefreshFailed
        : code === 'revision-conflict' ? language.risuSaveRevisionConflict
        : options.fallbackMessage ?? text.actionFailed
    alertError(hasPartialDestinationWarning(error) ? `${message} ${language.screenshotPartialDestinationMayRemain}` : message)
}
