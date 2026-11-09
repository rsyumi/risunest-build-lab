import { beforeEach, expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'
import { language } from 'src/lang'
const mocks = vi.hoisted(() => ({ alert: vi.fn(), partial: vi.fn() }))
vi.mock('../alert', () => ({ alertError: mocks.alert }))
vi.mock('./nativeFileJobManager', () => ({ nativeFileOperationOutcome: writable(null), NativeFileOperationBusyError: class extends Error {} }))
vi.mock('./nativeFileJobs', () => ({ NativeFileJobActivationCommittedError: class extends Error {} }))
vi.mock('./risuSaveFileRoute', () => ({ alertPartialDestinationWarning: mocks.partial, hasPartialDestinationWarning: () => false }))
import { nativeFileOperationOutcome, NativeFileOperationBusyError } from './nativeFileJobManager'
import { NativeFileJobActivationCommittedError } from './nativeFileJobs'
import { presentContentExportError, presentFileOperationError } from './fileOperationErrorPresentation'
beforeEach(() => { nativeFileOperationOutcome.set(null); vi.clearAllMocks() })
it.each(['cancelled', 'failed', 'succeeded'] as const)('leaves a fresh managed %s outcome to its dialog', state => {
    nativeFileOperationOutcome.set({ kind: 'import', startedAt: 20, state } as any)
    presentFileOperationError('import', new Error('synthetic'), 10)
    expect(mocks.alert).not.toHaveBeenCalled()
})
it('does not let an older outcome swallow a new preflight error', () => {
    nativeFileOperationOutcome.set({ kind: 'import', startedAt: 1, state: 'failed' } as any)
    presentFileOperationError('import', new NativeFileOperationBusyError(), 2)
    expect(mocks.alert).toHaveBeenCalledExactlyOnceWith(language.risuNest.backup.fileBusy)
})
it.each([
    ['generation-active', 'generationBusy'], ['server-sync-busy', 'syncBusy'], ['library-operation-busy', 'syncBusy'],
    ['library-file-operation-busy', 'syncBusy'], ['resolve-pending-operation-first', 'syncUnconfirmed'], ['server-status-unavailable', 'syncUnconfirmed'],
    ['sync-unavailable', 'syncUnavailable'],
] as const)('presents %s as a safe refusal', (code, key) => {
    presentFileOperationError('import', Object.assign(new Error(), { code }), 0)
    expect(mocks.alert).toHaveBeenCalledExactlyOnceWith(language.risuNest.backup[key])
})
it('keeps cancellation silent while preserving partial-destination warning handling', () => {
    const error = new DOMException('cancel', 'AbortError')
    presentFileOperationError('export', error, 0)
    expect(mocks.alert).not.toHaveBeenCalled()
    expect(mocks.partial).toHaveBeenCalledWith(error, language.screenshotPartialDestinationMayRemain, mocks.alert)
})
it('distinguishes committed activation from revision refusal', () => {
    presentFileOperationError('import', new (NativeFileJobActivationCommittedError as unknown as new () => Error)(), 0)
    expect(mocks.alert).toHaveBeenLastCalledWith(language.risuSaveImportCommittedRefreshFailed)
    presentFileOperationError('import', { code: 'revision-conflict' }, 0)
    expect(mocks.alert).toHaveBeenLastCalledWith(language.risuSaveRevisionConflict)
})
it.each([
    ['library-operation-busy', 'reasonBusy'], ['server-sync-busy', 'reasonBusy'], ['library-file-operation-busy', 'reasonBusy'],
] as const)('presents a content export refused with %s without backup and restore copy', (code, key) => {
    presentContentExportError(Object.assign(new Error('Another library operation is running'), { code }), 0)
    expect(mocks.alert).toHaveBeenCalledExactlyOnceWith(language.risuNest.importDialog[key])
})
it('presents content export admission refusals and keeps other failures as they are', () => {
    presentContentExportError(new Error('library-file-operation-busy'), 0)
    expect(mocks.alert).toHaveBeenLastCalledWith(language.risuNest.importDialog.reasonBusy)
    presentContentExportError(new NativeFileOperationBusyError(), 0)
    expect(mocks.alert).toHaveBeenLastCalledWith(language.risuNest.backup.fileBusy)
    const failure = Object.assign(new Error('synthetic'), { code: 'asset-missing' })
    presentContentExportError(failure, 0)
    expect(mocks.alert).toHaveBeenLastCalledWith(failure)
    mocks.alert.mockClear()
    presentContentExportError(new DOMException('cancel', 'AbortError'), 0)
    nativeFileOperationOutcome.set({ kind: 'export', startedAt: 20, state: 'failed' } as any)
    presentContentExportError(failure, 10)
    expect(mocks.alert).not.toHaveBeenCalled()
})
