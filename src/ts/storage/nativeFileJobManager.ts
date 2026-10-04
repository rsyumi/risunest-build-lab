import { isBackgroundExpiryReason } from '../iosNative'
import { measuredTaskPercent, runWithMobileBackgroundTask } from '../mobileBackgroundTask'
import { Mutex } from '../mutex'
import { get, writable } from 'svelte/store'
import { invoke } from '@tauri-apps/api/core'
import { doingChat } from '../process/generationState'
import { reserveLibraryFileOperation, waitForLibraryFileOperation } from './libraryFileOperation'
import { PayloadTooLargeError } from './nativePersistenceValue'

import {
    NativeFileJobActivationCommittedError,
    NativeFileJobError,
    resolveNativeFileJobStage,
    portableBodyRetryRequest,
    retryNativePortableRestoreBodies,
    type NativeFileJobResult,
    type NativeFileJobStage,
    type NativeFileJobStatus,
    type NativeFileOperationFormat,
    type NativeSnapshotBodiesStarted,
    type NativePortableBodyRetryRequest,
} from './nativeFileJobs'

export type { NativeFileOperationFormat } from './nativeFileJobs'

export type NativeFileOperationKind = 'import' | 'export'

/**
 * How the renderer presents the operation. `inline` keeps the settings-page
 * status row; `dialog` opens the shared progress dialog and publishes a
 * terminal outcome for it to show until the user dismisses it.
 */
export type NativeFileOperationPresentation = 'inline' | 'dialog'

export interface NativeFileOperationSource {
    name: string
    bytes?: number
}

export interface NativeFileOperationState {
    kind: NativeFileOperationKind
    presentation: NativeFileOperationPresentation
    format?: NativeFileOperationFormat
    startedAt: number
    source?: NativeFileOperationSource
    status?: NativeFileJobStatus
    /** Every distinct stage seen so far, in order, including phase-derived ones. */
    observedStages: NativeFileJobStage[]
    blocking: boolean
    cancelRequested: boolean
    partialWritesPossible: boolean
    waitingForSync?: boolean
}

export type NativeFileOperationOutcomeState = 'succeeded' | 'failed' | 'cancelled'

export interface NativeFileOperationError {
    code: string
    message: string
    recoveryRequired: boolean
    /** The kind of a refused item that is too large to save. */
    itemKind?: string
}

export interface NativeFileOperationOutcome {
    kind: NativeFileOperationKind
    format?: NativeFileOperationFormat
    startedAt: number
    finishedAt: number
    state: NativeFileOperationOutcomeState
    source?: NativeFileOperationSource
    /** Last status observed before the operation settled, including `detail` and `result`. */
    status?: NativeFileJobStatus
    observedStages: NativeFileJobStage[]
    result?: NativeFileJobResult
    error?: NativeFileOperationError
    warningCodes: string[]
    partialWritesPossible: boolean
    interruption?: 'background-expired'
}

export interface SharedNativeFileOperationContext {
    signal: AbortSignal
    onStatus(status: NativeFileJobStatus): void
    setBlocking(blocking: boolean): void
    setSource(source: NativeFileOperationSource): void
    setPartialWritesPossible(value: boolean): void
    releaseLibraryAfterPortableAdoption(receipt: NativeFileJobStatus): Promise<void>
}

export interface SharedNativeFileOperationOptions {
    userInitiated?: boolean
    presentation?: NativeFileOperationPresentation
    format?: NativeFileOperationFormat
    snapshotBodyOwner?: NativeSnapshotBodiesStarted
    portableBodyOwner?: NativePortableBodyRetryRequest
}

export const nativeFileOperation = writable<NativeFileOperationState | null>(null)
export const nativeFileOperationOutcome = writable<NativeFileOperationOutcome | null>(null)

/**
 * Which surface draws a dialog-presented operation. The onboarding takes
 * backup restores into its own panel while it is on screen; the shared
 * dialog draws everything else.
 */
export type NativeFileJobHost = 'dialog' | 'onboarding'
export const nativeFileJobHost = writable<NativeFileJobHost>('dialog')

let activeOperation: Promise<unknown> | null = null
let activeOperationKey: string | null = null
let activeController: AbortController | null = null
let activeState: NativeFileOperationState | null = null
const externalAndroidOperationMutex = new Mutex()

export class NativeFileOperationBusyError extends Error {
    constructor() {
        super('Another native file operation is already running')
        this.name = 'NativeFileOperationBusyError'
    }
}

type Settlement = { value: unknown } | { error: unknown }

function isAbortError(error: unknown): boolean {
    return error instanceof DOMException && error.name === 'AbortError'
}

function stringWarningCodes(value: unknown): string[] {
    if (!value || typeof value !== 'object' || !('warningCodes' in value)) return []
    const codes = (value as { warningCodes: unknown }).warningCodes
    if (!Array.isArray(codes)) return []
    return codes.filter((code): code is string => typeof code === 'string')
}

function mergeWarningCodes(...groups: string[][]): string[] {
    return [...new Set(groups.flat())]
}

function outcomeError(kind: NativeFileOperationKind, error: unknown): NativeFileOperationError {
    if (error instanceof NativeFileJobActivationCommittedError) {
        const cause = error.cause
        return {
            code: error.code,
            message: cause instanceof Error ? cause.message : String(cause),
            recoveryRequired: true,
        }
    }
    if (error instanceof PayloadTooLargeError) {
        return { code: error.code, message: error.message, recoveryRequired: false, itemKind: error.kind }
    }
    if (
        error !== null && typeof error === 'object' &&
        'code' in error && typeof error.code === 'string' &&
        'message' in error && typeof error.message === 'string'
    ) {
        return { code: error.code, message: error.message, recoveryRequired: false }
    }
    return {
        code: `${kind}-error`,
        message: error instanceof Error ? error.message : String(error),
        recoveryRequired: false,
    }
}

function outcomeFromSettlement(
    state: NativeFileOperationState,
    settlement: Settlement,
    finishedAt: number,
): NativeFileOperationOutcome | null {
    const statusWarnings = state.status?.result?.warningCodes ?? []
    const base = {
        kind: state.kind,
        ...(state.format ? { format: state.format } : {}),
        startedAt: state.startedAt,
        finishedAt,
        ...(state.source ? { source: state.source } : {}),
        ...(state.status ? { status: state.status } : {}),
        observedStages: state.observedStages,
        ...(state.status?.result ? { result: state.status.result } : {}),
        partialWritesPossible: state.partialWritesPossible,
    }
    if ('error' in settlement) {
        const warningCodes = mergeWarningCodes(statusWarnings, stringWarningCodes(settlement.error))
        if (isAbortError(settlement.error)) {
            return {
                ...base, state: 'cancelled', warningCodes,
                ...(isBackgroundExpiryReason(settlement.error) ? { interruption: 'background-expired' as const } : {}),
            }
        }
        return {
            ...base,
            state: 'failed',
            error: outcomeError(state.kind, settlement.error),
            warningCodes,
        }
    }
    if (settlement.value && typeof settlement.value === 'object' && 'kind' in settlement.value
        && ['missing', 'unchanged', 'kept-local'].includes(String(settlement.value.kind))) return null
    // A null or undefined result means the user backed out of the picker
    // before any work started, so there is nothing to summarize.
    if (settlement.value === null || settlement.value === undefined) return null
    return {
        ...base,
        state: 'succeeded',
        warningCodes: mergeWarningCodes(stringWarningCodes(settlement.value), statusWarnings),
    }
}

function publishActiveState(): void {
    nativeFileOperation.set(activeState ? { ...activeState } : null)
}

function updateActiveState(patch: Partial<NativeFileOperationState>): void {
    if (!activeState) return
    activeState = { ...activeState, ...patch }
    publishActiveState()
}

function recordStatus(status: NativeFileJobStatus): void {
    if (!activeState) return
    const stage = resolveNativeFileJobStage(status, activeState.format)
    const observedStages = stage && activeState.observedStages.at(-1) !== stage
        ? [...activeState.observedStages, stage]
        : activeState.observedStages
    updateActiveState({ status, observedStages })
}

export function runSharedNativeFileOperation<T>(
    kind: NativeFileOperationKind,
    operationKey: string,
    operation: (context: SharedNativeFileOperationContext) => Promise<T>,
    options: SharedNativeFileOperationOptions = {},
): Promise<T> {
    if (options.portableBodyOwner) {
        const receipt = options.portableBodyOwner
        return invoke<NativeFileJobStatus>('native_file_job_status', {jobId: receipt.jobId}).then(status => {
            const actual = portableBodyRetryRequest(status)
            if (kind !== 'import' || !status.portableBodyRetry?.available
                || Object.keys(receipt).some(key => receipt[key as keyof typeof receipt] !== actual[key as keyof typeof actual])) {
                throw new NativeFileJobError('portable-body-receipt-mismatch', 'Portable body retry receipt differs')
            }
            return runSharedNativeFileOperationWithAdmission(kind, operationKey, operation, options, true)
        })
    }
    if (options.snapshotBodyOwner) {
        const receipt = options.snapshotBodyOwner
        return invoke<NativeFileJobStatus>('native_file_job_status', {jobId: receipt.jobId}).then(status => {
            if (kind !== 'import' || receipt.kind !== 'snapshot-bodies' || status.kind !== 'snapshot-bodies'
                || status.jobId !== receipt.jobId || status.snapshotStagingId !== receipt.stagingId
                || String(status.activationRevision) !== receipt.activationRevision
                || status.activationAuthority !== receipt.bindingAuthority) {
                throw new NativeFileJobError('snapshot-body-receipt-mismatch', 'Snapshot body transfer receipt differs')
            }
            return runSharedNativeFileOperationWithAdmission(kind, operationKey, operation, options, true)
        })
    }
    return runSharedNativeFileOperationWithAdmission(kind, operationKey, operation, options, false)
}

function runSharedNativeFileOperationWithAdmission<T>(
    kind: NativeFileOperationKind,
    operationKey: string,
    operation: (context: SharedNativeFileOperationContext) => Promise<T>,
    options: SharedNativeFileOperationOptions,
    bodyOnly: boolean,
): Promise<T> {
    if (activeOperation) {
        return activeOperationKey === operationKey
            ? activeOperation as Promise<T>
            : Promise.reject(new NativeFileOperationBusyError())
    }

    if (!bodyOnly && options.format === 'library-backup' && get(doingChat)) {
        return Promise.reject(
            new NativeFileJobError(
                'generation-active',
                'A response is being generated. Finish or stop it before starting a backup or restore.',
            ),
        )
    }
    const releaseReservation = bodyOnly ? () => {} : reserveLibraryFileOperation()
    let reservationReleased=false
    const releaseAdmission=()=> {
        if (!reservationReleased) {reservationReleased=true;releaseReservation()}
    }

    const controller = new AbortController()
    const presentation = options.presentation ?? 'inline'
    activeController = controller
    activeOperationKey = operationKey
    activeState = {
        kind,
        presentation,
        ...(options.format ? { format: options.format } : {}),
        startedAt: Date.now(),
        observedStages: [],
        blocking: false,
        cancelRequested: false,
        partialWritesPossible: false,
    }
    if (presentation === 'dialog') nativeFileOperationOutcome.set(null)

    const settle = (settlement: Settlement) => {
        if (activeOperation !== promise) return
        const state = activeState
        activeOperation = null
        activeOperationKey = null
        activeController = null
        activeState = null
        releaseAdmission()
        if (state?.presentation === 'dialog') {
            const outcome = outcomeFromSettlement(state, settlement, Date.now())
            if (outcome) nativeFileOperationOutcome.set(outcome)
        }
        publishActiveState()
    }

    let resolveOperation!: (value: T | PromiseLike<T>) => void
    let rejectOperation!: (cause: unknown) => void
    const promise = new Promise<T>((resolve, reject) => { resolveOperation = resolve; rejectOperation = reject })
    activeOperation = promise
    publishActiveState()
    const context: SharedNativeFileOperationContext = {
        signal: controller.signal,
        onStatus: recordStatus,
        setBlocking: (blocking) => updateActiveState({ blocking }),
        setSource: (source) => updateActiveState({ source }),
        setPartialWritesPossible: (value) => updateActiveState({ partialWritesPossible: value }),
        async releaseLibraryAfterPortableAdoption(receipt) {
            const status=await invoke<NativeFileJobStatus>('native_file_job_status',{jobId:receipt.jobId})
            if (receipt.kind!=='restore-portable-backup' || status.kind!==receipt.kind || status.jobId!==receipt.jobId
                || status.activationRevision!==receipt.activationRevision || !Number.isSafeInteger(status.activationRevision)
                || status.activationAuthority!==receipt.activationAuthority || !status.activationAuthority
                || status.deviceSessionId!==receipt.deviceSessionId || !status.deviceSessionId
                || status.restoreAdoptionConfirmed!==true) {
                throw new NativeFileJobError('portable-adoption-receipt-mismatch','Portable restore adoption could not be confirmed')
            }
            releaseAdmission()
        },
    }
    try {
        const taskKind = options.format === 'library-backup' || options.format === 'risu-save'
            ? (kind === 'export' ? 'backup' : 'restore') : kind
        runWithMobileBackgroundTask(taskKind, async task => {
            const waiting = bodyOnly ? null : waitForLibraryFileOperation(task.signal ?? context.signal)
            if (waiting) {
                updateActiveState({ waitingForSync: true })
                await waiting
                updateActiveState({ waitingForSync: false })
            }
            return operation({
            ...context,
            signal: task.signal ?? context.signal,
            onStatus: status => {
                recordStatus(status)
                task.progress(measuredTaskPercent(status.progress.completedBytes, status.progress.totalBytes))
            },
            })
        }, controller.signal, options.userInitiated ?? true).then(
            value => { settle({ value }); resolveOperation(value) },
            error => { settle({ error }); rejectOperation(error) },
        )
    } catch (error) {
        settle({ error })
        rejectOperation(error)
    }
    return promise
}

export function runExternalAndroidNativeFileOperation<T>(
    kind: NativeFileOperationKind,
    operation: (context: SharedNativeFileOperationContext) => Promise<T>,
    options: SharedNativeFileOperationOptions = {},
): Promise<T> {
    if (options.format === 'library-backup') {
        return runSharedNativeFileOperation(
            kind,
            `external-android:${kind}`,
            operation,
            options,
        )
    }
    return externalAndroidOperationMutex.runExclusive(async () => {
        while (activeOperation) {
            try {
                await activeOperation
            } catch {}
        }
        return await runSharedNativeFileOperation(
            kind,
            `external-android:${kind}`,
            operation,
            options,
        )
    })
}

export function cancelActiveNativeFileOperation(): void {
    if (!activeController) return
    activeController.abort()
    updateActiveState({ cancelRequested: true })
}

export function dismissNativeFileOperationOutcome(): void {
    nativeFileOperationOutcome.set(null)
}

export async function retryPortableRestoreBodiesFromOutcome(): Promise<NativeFileJobResult> {
    const outcome = get(nativeFileOperationOutcome)
    if (!outcome?.status) return Promise.reject(new NativeFileJobError('portable-body-retry-refused', 'Portable body retry is unavailable'))
    const receipt = portableBodyRetryRequest(outcome.status)
    return runSharedNativeFileOperation('import', `portable-bodies:${receipt.jobId}`, context => {
        context.setBlocking(false)
        return retryNativePortableRestoreBodies(receipt, {signal: context.signal, onStatus: context.onStatus})
    }, {presentation: 'dialog', format: 'library-backup', portableBodyOwner: receipt})
}

/**
 * Whether the shared dialog is showing how the last operation of this kind ended. Callers that
 * report outcomes themselves skip their message while it is, so the user reads one account.
 */
export function nativeFileOperationOutcomeShown(kind: NativeFileOperationKind): boolean {
    return get(nativeFileOperationOutcome)?.kind === kind
}
