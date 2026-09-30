import { beforeEach, expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'
import { languageEnglish } from 'src/lang/en'
const mocks = vi.hoisted(() => ({ alert: vi.fn(), partial: vi.fn() }))
vi.mock('../alert', () => ({ alertError: mocks.alert }))
vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('./nativeFileJobManager', () => ({ nativeFileOperationOutcome: writable(null), NativeFileOperationBusyError: class extends Error {} }))
vi.mock('./nativeFileJobs', () => ({ NativeFileJobActivationCommittedError: class extends Error {} }))
vi.mock('./risuSaveFileRoute', () => ({ alertPartialDestinationWarning: mocks.partial, hasPartialDestinationWarning: () => false }))
import { nativeFileOperationOutcome, NativeFileOperationBusyError } from './nativeFileJobManager'
import { NativeFileJobActivationCommittedError } from './nativeFileJobs'
import { presentFileOperationError } from './fileOperationErrorPresentation'
beforeEach(() => { nativeFileOperationOutcome.set(null); vi.clearAllMocks() })
it.each(['cancelled', 'failed', 'succeeded'] as const)('leaves a fresh managed %s outcome to its dialog', state => {
    nativeFileOperationOutcome.set({ kind: 'import', startedAt: 20, state } as any)
    presentFileOperationError('import', new Error('synthetic'), 10)
    expect(mocks.alert).not.toHaveBeenCalled()
})
it('does not let an older outcome swallow a new preflight error', () => {
    nativeFileOperationOutcome.set({ kind: 'import', startedAt: 1, state: 'failed' } as any)
    presentFileOperationError('import', new NativeFileOperationBusyError(), 2)
    expect(mocks.alert).toHaveBeenCalledExactlyOnceWith(languageEnglish.risuNest.backup.fileBusy)
})
it.each([
    ['generation-active', 'generationBusy'], ['server-sync-busy', 'syncBusy'], ['library-operation-busy', 'syncBusy'],
    ['library-file-operation-busy', 'syncBusy'], ['resolve-pending-operation-first', 'syncUnconfirmed'], ['server-status-unavailable', 'syncUnconfirmed'],
] as const)('presents %s as a safe refusal', (code, key) => {
    presentFileOperationError('import', Object.assign(new Error(), { code }), 0)
    expect(mocks.alert).toHaveBeenCalledExactlyOnceWith(languageEnglish.risuNest.backup[key])
})
it('keeps cancellation silent while preserving partial-destination warning handling', () => {
    const error = new DOMException('cancel', 'AbortError')
    presentFileOperationError('export', error, 0)
    expect(mocks.alert).not.toHaveBeenCalled()
    expect(mocks.partial).toHaveBeenCalledWith(error, languageEnglish.screenshotPartialDestinationMayRemain, mocks.alert)
})
it('distinguishes committed activation from revision refusal', () => {
    presentFileOperationError('import', new (NativeFileJobActivationCommittedError as unknown as new () => Error)(), 0)
    expect(mocks.alert).toHaveBeenLastCalledWith(languageEnglish.risuSaveImportCommittedRefreshFailed)
    presentFileOperationError('import', { code: 'revision-conflict' }, 0)
    expect(mocks.alert).toHaveBeenLastCalledWith(languageEnglish.risuSaveRevisionConflict)
})
