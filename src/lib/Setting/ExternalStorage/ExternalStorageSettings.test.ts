// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const state = vi.hoisted(() => ({
    getState: vi.fn(),
    prepareHistoryDelete: vi.fn(),
    syncNow: vi.fn(), bind: vi.fn(), unbind: vi.fn(),
    failures: new Map<string, unknown>(), remedy: true,
    jobStarted: undefined as (() => void) | undefined,
    stopJobEvents: vi.fn(),
    cancelJob: vi.fn(),
    startJob: vi.fn(),
    setAutomaticBackupPaused: vi.fn(),
    exportSnapshot: vi.fn(),
    cancelExport: vi.fn(),
    exportRetainedPublication: vi.fn(),
    removeRetainedPublication: vi.fn(),
    getQuota: vi.fn(),
    setRetentionPolicy: vi.fn(),
    listHistory: vi.fn(),
    listConflicts: vi.fn(),
    recheckConflict: vi.fn(),
    deleteConflict: vi.fn(),
    beginConnectionSettingsExport: vi.fn(),
    saveConnectionSettingsFile: vi.fn(),
    removeConnection: vi.fn(),
    residency: vi.fn(),
    downloadRemote: vi.fn(),
}))

vi.mock('src/ts/platform', () => ({ isTauri: true, isTauriAndroid: false, isTauriIOS: false }))
vi.mock('@tauri-apps/plugin-os', () => ({ type: () => 'windows' }))
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }))
vi.mock('qrcode', () => ({ default: { toDataURL: vi.fn() } }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(), alertNormal: vi.fn(), alertCheckboxConfirm: vi.fn() }))
vi.mock('src/ts/storage/sync/external/lwwProduction', () => ({ requestExternalLwwNow: state.syncNow, supportsExternalLwwNewDevice: () => state.remedy, subscribeExternalLwwFailures(callback: (value: ReadonlyMap<string, unknown>) => void) { callback(new Map(state.failures)); return () => {} } }))
vi.mock('src/ts/storage/sync/bindingRegistry', () => ({ bindSyncTarget: state.bind, unbindSyncTarget: state.unbind }))
vi.mock('src/lang', () => ({ language: { lwwSync: { newDeviceAction: 'Connect as new device', clockBlocked: 'Correct the device clock and retry.', previousStorageUnavailable: 'The previous storage could not be reached.', downloadFailedNotConnected: 'The files could not be downloaded, so the connection was not made.' } } }))
vi.mock('src/ts/storage/sync/serverAssetResidency', () => ({ getAssetResidencyStatus: state.residency, downloadRemoteAssets: state.downloadRemote }))
vi.mock('src/ts/stores.svelte' , () => ({ DBState: { db: { language: 'en' } } }))
vi.mock('src/ts/storage/sync/external/production', () => ({
    refreshExternalStorageProductionState: vi.fn(async () => {}),
    requestExternalStorageNow: vi.fn(),
    resumeExternalStorageJob: vi.fn(),
    requestExternalConflictExport: vi.fn(),
    requestExternalConflictRestore: vi.fn(),
    requestExternalStorageResolveConflict: vi.fn(),
    requestExternalStorageRestore: vi.fn(),
    requestExternalStorageDeleteHistory: vi.fn(async () => {}),
    stopExternalStorageRestore: vi.fn(async () => {}),
}))
vi.mock('src/ts/storage/sync/external/bridge', () => ({
    getExternalStorageBridge: () => ({
        getState: state.getState,
        prepareHistoryDelete: state.prepareHistoryDelete,
        onJobStarted: async (listener: () => void) => { state.jobStarted = listener; return state.stopJobEvents },
        cancelJob: state.cancelJob,
        startJob: state.startJob,
        setAutomaticBackupPaused: state.setAutomaticBackupPaused,
        exportSnapshot: state.exportSnapshot,
        cancelExport: state.cancelExport,
        exportRetainedPublication: state.exportRetainedPublication,
        removeRetainedPublication: state.removeRetainedPublication,
        getQuota: state.getQuota,
        setRetentionPolicy: state.setRetentionPolicy,
        listHistory: state.listHistory,
        listConflicts: state.listConflicts,
        recheckConflict: state.recheckConflict,
        deleteConflict: state.deleteConflict,
        beginConnectionSettingsExport: state.beginConnectionSettingsExport,
        saveConnectionSettingsFile: state.saveConnectionSettingsFile,
        removeConnection: state.removeConnection,
    }),
}))

import { alertConfirm, alertCheckboxConfirm } from 'src/ts/alert'
import ExternalStorageSettings from './ExternalStorageSettings.svelte'
import { requestExternalStorageNow, resumeExternalStorageJob, requestExternalStorageDeleteHistory, stopExternalStorageRestore } from 'src/ts/storage/sync/external/production'
import { externalStorageStrings } from './strings'

beforeEach(() => { state.failures.clear(); state.remedy = true })
const strings = externalStorageStrings('en')
let target: HTMLDivElement
let component: ReturnType<typeof mount> | undefined

function connection(keepCount: number, keepDays: number) {
    return {
        id: 'connection-1',
        providerId: 'webdav' as const,
        purpose: 'backup' as const,
        strategy: 'backup-only' as const,
        mode: 'existing' as const,
        displayName: 'Synthetic',
        endpoint: {
            providerId: 'webdav' as const,
            authority: 'https://synthetic.invalid',
            repositoryHint: 'RisuNest',
            warnings: [],
            remoteVerified: true,
        },
        retentionPolicy: { keepCount, keepDays },
        capabilities: {
            immutableCreate: true, directCompleteRead: true, atomicCreateHead: false,
            conditionalHeadUpdate: false, stableHeadReplace: false, headReadAfterWrite: false,
            headRetryControl: false, leaseOperations: false, deleteObjects: false,
            conditionalGet: false, resumableUpload: false, range: false,
            snapshotDiscovery: true, maxStoredBytes: null, sdkOverheadBytes: 0, uploadAlignment: 1,
        },
        status: 'ready' as const, automaticBackupPaused: false,
    }
}

function labelled(text: string): HTMLInputElement {
    const label = [...target.querySelectorAll('label')].find(item => item.textContent?.includes(text))
    const control = label?.querySelector('input')
    if (!control) throw new Error(`Missing control: ${text}`)
    return control
}

async function openStorageUsage(): Promise<void> {
    const tab = [...target.querySelectorAll('button')].find(item => item.textContent?.trim() === strings.quota)
    if (!tab) throw new Error('Missing the storage usage tab')
    tab.click()
    await tick()
    await Promise.resolve()
    await tick()
}

async function settle(): Promise<void> {
    for (let step = 0; step < 4; step += 1) {
        await Promise.resolve()
        await tick()
    }
}

describe('the storage usage tab', () => {
    beforeEach(async () => {
        vi.mocked(requestExternalStorageNow).mockResolvedValue({ kind: 'cancelled' })
        state.getState.mockResolvedValue({
            supported: true,
            selection: { kind: 'none', selectionEpoch: '0', paused: false },
            connections: [connection(10, 30)],
            jobs: [],
        })
        // The rows have to stand on their own: usage is a separate request.
        state.getQuota.mockRejectedValue({ kind: 'transient', httpStatus: null, retryAtMs: null })
        state.setRetentionPolicy.mockResolvedValue(undefined)
        state.listHistory.mockResolvedValue({ items: [] })
        state.beginConnectionSettingsExport.mockResolvedValue({
            transferId: 'settings-transfer',
            expiresAtMs: '1000',
            qrPayload: null,
        })
        state.saveConnectionSettingsFile.mockResolvedValue(undefined)
        target = document.createElement('div')
        document.body.append(target)
        component = mount(ExternalStorageSettings, { target })
        await settle()
    })

    afterEach(() => {
        if (component) unmount(component)
        component = undefined
        target.remove()
        vi.clearAllMocks()
    })

    it('observes an automatic job starting after the panel became idle', async () => {
        const active = { id: 'job', connectionId: 'connection-1', kind: 'backup', state: 'running', phase: 'upload',
            completedBytes: '4', totalBytes: '10', completedItems: '1', startedAtMs: '1', updatedAtMs: '1' }
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [active] })
        state.jobStarted?.()
        await settle()
        expect(target.textContent).toContain(strings.jobActive.backup)
        expect([...target.querySelectorAll('button')].some(button => button.textContent?.trim() === strings.cancel)).toBe(true)
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [{ ...active, state: 'conflict' }] })
        state.jobStarted?.()
        await settle()
        expect(target.textContent).toContain(strings.resolveRequired)
    })

    it.each(['backup', 'cleanup', 'restore', 'check-repository'])(
        'shows the paused %s reason while preventing a competing operation', async kind => {
        const job = { id: 'paused', connectionId: 'connection-1', kind, state: 'waiting', phase: 'paused',
            reason: 'automatic', targetRevision: '8', completedBytes: '4', totalBytes: '10', completedItems: '1',
            startedAtMs: '1', updatedAtMs: '1',
            error: { code: 'storageFull', action: 'free-space', retryable: false } }
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [job] })
        state.jobStarted?.()
        await settle()
        expect(target.textContent).toContain(strings.freeSpace)
        expect(target.querySelector('[role="progressbar"]')).toBeNull()
        const backup = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.runBackup)
        expect(backup?.disabled).toBe(true)
        expect([...target.querySelectorAll('button')].some(button => button.textContent?.trim() === strings.retryAction)).toBe(true)
    })

    it('shows a structured start refusal after refreshing native state', async () => {
        vi.mocked(requestExternalStorageNow).mockResolvedValue({ kind: 'blocked', reason: 'preconditionFailed', cause: { kind: 'preconditionFailed' } })
        const backup = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.runBackup)!
        backup.click()
        await settle()
        expect(target.textContent).toContain(strings.stateChanged)
    })

    it('shows a manual backup that stopped on a failure with its retry', async () => {
        const job = { id: 'manual', connectionId: 'connection-1', kind: 'backup', state: 'waiting', phase: 'paused',
            reason: 'manual', targetRevision: '8', completedBytes: '0', completedItems: '0', startedAtMs: '1', updatedAtMs: '2',
            error: { code: 'transient', message: 'The operation could not complete.', action: 'retry', retryable: true } }
        vi.mocked(requestExternalStorageNow).mockImplementation(async () => {
            state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false },
                connections: [connection(10, 30)], jobs: [job] })
            return { kind: 'blocked', reason: 'transient', error: job.error, job } as never
        })
        const backup = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.runBackup)!
        backup.click()
        await settle()
        expect(requestExternalStorageNow).toHaveBeenCalledWith('connection-1', 'backup')
        expect([...target.querySelectorAll('[role="status"]')].some(item => item.textContent === strings.retry)).toBe(true)
        expect([...target.querySelectorAll('button')].some(button => button.textContent?.trim() === strings.retryAction)).toBe(true)
    })

    it('clears a background poll failure once a later poll succeeds', async () => {
        vi.useFakeTimers()
        try {
            const running = { id: 'job', connectionId: 'connection-1', kind: 'backup', state: 'running', phase: 'upload',
                completedBytes: '4', totalBytes: '10', completedItems: '1', startedAtMs: '1', updatedAtMs: '1' }
            state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
                connections: [connection(10, 30)], jobs: [running] })
            state.jobStarted?.()
            await settle()
            const alerts = () => [...target.querySelectorAll('[role="alert"]')].map(item => item.textContent)
            state.getState.mockRejectedValueOnce({ kind: 'transient', httpStatus: null, retryAtMs: null })
            await vi.advanceTimersByTimeAsync(1_200)
            await settle()
            expect(alerts()).toEqual([strings.retry])
            await vi.advanceTimersByTimeAsync(1_200)
            await settle()
            expect(alerts()).toEqual([])
            expect(target.textContent).toContain(strings.jobActive.backup)
        } finally { vi.useRealTimers() }
    })

    it('keeps an action failure through background refreshes', async () => {
        vi.mocked(requestExternalStorageNow).mockResolvedValue({ kind: 'blocked', reason: 'preconditionFailed', cause: { kind: 'preconditionFailed' } })
        const backup = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.runBackup)!
        backup.click()
        await settle()
        const alerts = () => [...target.querySelectorAll('[role="alert"]')].map(item => item.textContent)
        expect(alerts()).toEqual([strings.stateChanged])
        state.getState.mockRejectedValueOnce({ kind: 'transient', httpStatus: null, retryAtMs: null })
        state.jobStarted?.()
        await settle()
        expect(alerts()).toEqual([strings.stateChanged])
        state.jobStarted?.()
        await settle()
        expect(alerts()).toEqual([strings.stateChanged])
    })

    it('retries the retained automatic operation and persists its pause choice', async () => {
        const job = { id: 'retained', connectionId: 'connection-1', kind: 'backup', state: 'waiting', phase: 'paused',
            reason: 'automatic', targetRevision: '8', completedBytes: '0', completedItems: '0', startedAtMs: '1', updatedAtMs: '1',
            error: { code: 'transient', action: 'retry', retryable: true } }
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [job] })
        vi.mocked(resumeExternalStorageJob).mockResolvedValue({ kind: 'cancelled' })
        state.jobStarted?.()
        await settle()
        const retry = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.retryAction)!
        retry.click()
        await settle()
        expect(resumeExternalStorageJob).toHaveBeenCalledWith(job)
        labelled(strings.automaticBackup).click()
        await settle()
        expect(state.setAutomaticBackupPaused).toHaveBeenCalledWith('connection-1', true)
    })

    it('offers an explicit recheck for an uncertain publication without claiming completion', async () => {
        const job = { id: 'uncertain-operation', connectionId: 'connection-1', kind: 'backup', state: 'uncertain',
            phase: 'publication-unknown', reason: 'automatic', targetRevision: '8', completedBytes: '0',
            completedItems: '0', startedAtMs: '1', updatedAtMs: '1',
            result: { reason: 'publication-unknown' } }
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [job] })
        vi.mocked(resumeExternalStorageJob).mockResolvedValue({ kind: 'blocked', reason: 'publication-unknown', job } as never)
        state.jobStarted?.()
        await settle()
        expect(target.textContent).toContain(strings.publicationDecision)
        expect(target.textContent).toContain(strings.statusError)
        expect(target.querySelector('[role="progressbar"]')).toBeNull()
        const recheck = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.recheckPublication)!
        expect(recheck.disabled).toBe(false)
        recheck.click()
        await settle()
        expect(resumeExternalStorageJob).toHaveBeenCalledWith(job)
        expect(requestExternalStorageNow).not.toHaveBeenCalled()
        expect(target.textContent).not.toContain(strings.completed)
        expect(labelled(strings.automaticBackup).disabled).toBe(false)
        expect([...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.remove)?.disabled).toBe(false)
    })

    it('offers one confirmed stop for an unfinished restore in place of its error', async () => {
        const error = { code: 'transient', message: 'synthetic', action: 'retry', retryable: false, reason: 'local-apply-unknown' }
        const job = { id: 'unfinished-restore', connectionId: 'connection-1', kind: 'restore', state: 'uncertain',
            phase: 'local-apply-unknown', reason: 'manual', targetRevision: '8', completedBytes: '0',
            completedItems: '0', startedAtMs: '1', updatedAtMs: '1', applicationStarted: true, error }
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [{ ...connection(10, 30), status: 'error', lastError: error }], jobs: [job] })
        state.jobStarted?.()
        await settle()
        expect(target.textContent).toContain(strings.restoreUnfinished)
        expect(target.textContent).not.toContain(strings.publicationDecision)
        expect(target.textContent).not.toContain(strings.retry)
        expect(target.textContent).not.toContain(strings.uncertain)
        const stop = () => [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.stopRestore)
        vi.mocked(alertConfirm).mockResolvedValueOnce(false)
        stop()!.click()
        await settle()
        expect(alertConfirm).toHaveBeenCalledWith(`${strings.stopRestoreTitle}\n${strings.stopRestoreDescription}`)
        expect(stopExternalStorageRestore).not.toHaveBeenCalled()
        vi.mocked(alertConfirm).mockResolvedValueOnce(true)
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [{ ...job, state: 'failed', phase: 'paused',
                error: { code: 'cancelled', message: 'synthetic', action: 'retry', retryable: true } }] })
        stop()!.click()
        await settle()
        expect(alertConfirm).toHaveBeenCalledTimes(2)
        expect(alertCheckboxConfirm).not.toHaveBeenCalled()
        expect(stopExternalStorageRestore).toHaveBeenCalledWith('unfinished-restore')
        expect(target.textContent).not.toContain(strings.restoreUnfinished)
        expect(stop()).toBeUndefined()
    })

    it('does not offer to stop a restore that is still running', async () => {
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [{ id: 'running-restore', connectionId: 'connection-1', kind: 'restore',
                state: 'running', phase: 'downloading', completedBytes: '0', completedItems: '0', startedAtMs: '1', updatedAtMs: '1' }] })
        state.jobStarted?.()
        await settle()
        expect([...target.querySelectorAll('button')].some(button => button.textContent?.trim() === strings.stopRestore)).toBe(false)
        expect(target.textContent).not.toContain(strings.restoreUnfinished)
    })

    it('owns one snapshot export, reports progress and cancels that export ID', async () => {
        state.listHistory.mockResolvedValue({ items: [{ id: 'point', snapshotId: 'snapshot', kind: 'backup-point',
            createdAtMs: '1', logicalRevision: '1', complete: true, verified: true, pinned: false,
            includedSections: [], sameDevice: true }] })
        const tab = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.history)!
        tab.click()
        await settle()
        let finish!: (result: unknown) => void
        state.exportSnapshot.mockImplementation((_connection, _snapshot, _id, progress) => {
            progress({ completedBytes: '10', totalBytes: '20', completedItems: '1', totalItems: '2' })
            return new Promise(resolve => { finish = resolve })
        })
        state.cancelExport.mockResolvedValue(undefined)
        const download = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.download)!
        download.click()
        await settle()
        expect(download.disabled).toBe(true)
        expect(state.exportSnapshot).toHaveBeenCalledOnce()
        expect(state.exportSnapshot.mock.calls[0].slice(0, 2)).toEqual(['connection-1', 'snapshot'])
        expect(target.textContent).toContain('10 B / 20 B')
        const cancel = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.cancel)!
        cancel.click()
        await settle()
        expect(state.cancelExport).toHaveBeenCalledWith(state.exportSnapshot.mock.calls[0][2])
        finish({ cancelled: true })
        await settle()
        expect(download.disabled).toBe(false)
    })

    it('refreshes history after a pin job settles successfully', async () => {
        const item = { id: 'point', snapshotId: 'snapshot', kind: 'backup-point', createdAtMs: '1', logicalRevision: '1',
            complete: true, verified: true, pinned: false, includedSections: [], sameDevice: true }
        state.listHistory.mockResolvedValue({ items: [item] })
        const tab = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.history)!
        tab.click()
        await settle()
        state.startJob.mockResolvedValue({ id: 'pin-job', connectionId: 'connection-1', kind: 'pin-history', state: 'queued' })
        const pin = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.pin)!
        pin.click()
        await settle()
        state.listHistory.mockResolvedValue({ items: [{ ...item, pinned: true }] })
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [{ id: 'pin-job', connectionId: 'connection-1', kind: 'pin-history', state: 'succeeded' }] })
        state.jobStarted?.()
        await settle()
        expect(state.listHistory).toHaveBeenCalledTimes(2)
        expect(target.textContent).toContain(strings.pinned)
    })

    it('reloads the open history when a backup finishes', async () => {
        const older = { id: 'older', snapshotId: 'older', kind: 'backup-point', createdAtMs: '1', logicalRevision: '1',
            complete: true, verified: true, pinned: false, includedSections: [], sameDevice: true }
        state.listHistory.mockResolvedValue({ items: [older] })
        const tab = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.history)!
        tab.click()
        await settle()
        const backup = { id: 'backup-job', connectionId: 'connection-1', kind: 'backup', state: 'running', phase: 'upload',
            completedBytes: '4', totalBytes: '10', completedItems: '1', startedAtMs: '2', updatedAtMs: '2' }
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [backup] })
        state.jobStarted?.()
        await settle()
        expect(state.listHistory).toHaveBeenCalledTimes(1)
        state.listHistory.mockResolvedValue({ items: [{ ...older, id: 'newer', snapshotId: 'newer', createdAtMs: '2' }, older] })
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [{ ...backup, state: 'succeeded' }] })
        state.jobStarted?.()
        await settle()
        expect(state.listHistory).toHaveBeenCalledTimes(2)
        expect(state.listHistory).toHaveBeenLastCalledWith('connection-1', undefined)
        expect(target.querySelectorAll('[role="tabpanel"] .item')).toHaveLength(2)
    })

    it('reloads the open history after a manual backup that finished between polls', async () => {
        const tab = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.history)!
        tab.click()
        await settle()
        const finished = { id: 'manual-job', connectionId: 'connection-1', kind: 'backup', state: 'succeeded' }
        vi.mocked(requestExternalStorageNow).mockResolvedValue({ kind: 'complete', revision: '2', job: finished } as never)
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [finished] })
        const run = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.runBackup)!
        run.click()
        await settle()
        await settle()
        expect(state.listHistory).toHaveBeenCalledTimes(2)
    })

    it('leaves a closed history alone when a backup finishes', async () => {
        const backup = { id: 'backup-job', connectionId: 'connection-1', kind: 'backup', state: 'running', phase: 'upload',
            completedBytes: '4', totalBytes: '10', completedItems: '1', startedAtMs: '2', updatedAtMs: '2' }
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [backup] })
        state.jobStarted?.()
        await settle()
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0' },
            connections: [connection(10, 30)], jobs: [{ ...backup, state: 'succeeded' }] })
        state.jobStarted?.()
        await settle()
        expect(state.listHistory).not.toHaveBeenCalled()
    })

    it('reloads the open history from its first page on refresh', async () => {
        const tab = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.history)!
        tab.click()
        await settle()
        const refresh = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.refresh)!
        refresh.click()
        await settle()
        expect(state.listHistory).toHaveBeenCalledTimes(2)
        expect(state.listHistory).toHaveBeenLastCalledWith('connection-1', undefined)
    })


    it.each(['mybox', 'github_releases', 'gitlab_packages'])('does not offer Sync on a %s backup connection', async providerId => {
        if (component) await unmount(component)
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false }, connections: [{ ...connection(10, 30), providerId }], jobs: [] })
        component = mount(ExternalStorageSettings, { target }); await settle()
        expect([...target.querySelectorAll('button')].some(item => item.textContent?.trim() === strings.sync)).toBe(false)
        expect(target.textContent).not.toContain(strings.automaticSync)
    })
    it('offers actual native new-device recovery through the shared binding action', async () => {
        if (component) await unmount(component)
        state.failures.set('connection-1', { kind: 'corrupt' }); state.bind.mockResolvedValue({ kind: 'cancelled' })
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'external', connectionId: 'connection-1', selectionEpoch: '0', paused: false }, connections: [{ ...connection(10, 30), purpose: 'sync', strategy: 'sequential' }], jobs: [] })
        component = mount(ExternalStorageSettings, { target }); await settle()
        const recovery = [...target.querySelectorAll('button')].find(item => item.textContent?.trim() === 'Connect as new device')!
        expect(recovery).toBeDefined(); recovery.click(); await settle()
        expect(state.bind).toHaveBeenCalledWith({ kind: 'external', connectionId: 'connection-1' }, { mode: 'new-device' })
        expect(alertConfirm).not.toHaveBeenCalled(); expect(alertCheckboxConfirm).not.toHaveBeenCalled()
    })
    it('combines both history deletion consequences into one checked confirmation', async () => {
        const item = { id: 'point', snapshotId: 'snapshot', pointId: 'point-id', pointObservation: 'observation', deletable: true, kind: 'backup-point', createdAtMs: '1', logicalRevision: '1', pinned: false, complete: true, verified: true, includedSections: [], sameDevice: false }
        state.listHistory.mockResolvedValue({ items: [item] }); state.prepareHistoryDelete.mockResolvedValue({ sameDevice: false, lastRetained: true })
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: true, checked: true })
        const tab = [...target.querySelectorAll('button')].find(item => item.textContent?.trim() === strings.history)!; tab.click(); await settle()
        const remove = [...target.querySelectorAll('button')].find(item => item.textContent?.trim() === strings.deleteHistory)!; remove.click(); await settle()
        expect(alertCheckboxConfirm).toHaveBeenCalledOnce(); expect(alertConfirm).not.toHaveBeenCalled()
        expect(alertCheckboxConfirm).toHaveBeenCalledWith(expect.objectContaining({ requireChecked: true, description: `${strings.deleteOtherDeviceConfirm} ${strings.deleteLastRetainedConfirm}` }))
        expect(requestExternalStorageDeleteHistory).toHaveBeenCalledWith('connection-1', item, true, true)
    })
    it('keeps the quota label scoped to locally known repository data', async () => {
        state.getQuota.mockResolvedValue({ connectionId: 'connection-1', buckets: [], storage: {
            providerPhysicalKnown: false, locallyUploadedBytesLowerBound: '12',
            locallyUploadedObjectCountLowerBound: '1', locallyUploadedCoverage: 'cached-upload-receipts',
        } })
        await openStorageUsage()
        await settle()
        expect(target.textContent).toContain('Known repository data')
        expect(target.textContent).not.toContain('Uploaded from this device')
    })

    it('runs manual cleanup through the production queue only for a capable connection', async () => {
        await openStorageUsage()
        expect([...target.querySelectorAll('button')].some(button => button.textContent?.trim() === strings.cleanup)).toBe(false)
        if (component) unmount(component)
        const capable = connection(10, 30)
        capable.capabilities.leaseOperations = true
        capable.capabilities.deleteObjects = true
        vi.mocked(requestExternalStorageNow).mockResolvedValue({ kind: 'cancelled' })
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false }, connections: [capable], jobs: [] })
        component = mount(ExternalStorageSettings, { target })
        await settle()
        await openStorageUsage()
        const button = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.cleanup)
        expect(button).toBeDefined()
        button!.click()
        await settle()
        expect(requestExternalStorageNow).toHaveBeenCalledWith('connection-1', 'cleanup')
    })

    it('shows the stored limits even when the usage request fails', async () => {
        await openStorageUsage()
        expect(labelled(strings.retentionCount).value).toBe('10')
        expect(labelled(strings.retentionDays).value).toBe('30')
    })

    it('exports connection settings without offering recovery-key replacement', async () => {
        const button = [...target.querySelectorAll('button')].find(
            item => item.textContent?.trim() === strings.connectionSettings,
        )
        expect(button).toBeDefined()
        button?.click()
        await settle()

        expect(state.beginConnectionSettingsExport).toHaveBeenCalledWith('connection-1')
        expect(target.getAttribute('role')).not.toBe('dialog')
        expect(target.textContent).toContain(strings.connectionSettingsNotice)
        expect(target.textContent).not.toMatch(/reissue|rotate/i)

        const save = [...target.querySelectorAll('button')].find(
            item => item.textContent?.trim() === strings.saveConnectionSettings,
        )
        save?.click()
        await settle()
        expect(state.saveConnectionSettingsFile).toHaveBeenCalledWith('settings-transfer')
    })

    it('does not render provider-supplied prose from ordinary history rows', async () => {
        state.listHistory.mockResolvedValue({
            items: [{
                id: 'snapshot', snapshotId: 'snapshot', kind: 'backup-point',
                createdAtMs: '1', logicalRevision: 1, storedBytes: '1', pinned: false,
                complete: true, verified: true, includedSections: [], sameDevice: true,
                warning: 'UNTRUSTED REMOTE WARNING',
            }],
        })
        const tab = [...target.querySelectorAll('button')].find(
            item => item.textContent?.trim() === strings.history,
        )
        tab?.click()
        await settle()

        expect(target.textContent).not.toContain('UNTRUSTED REMOTE WARNING')
        expect(target.textContent).toContain(strings.historyKinds['backup-point'])
    })

    it('sends a changed limit and leaves the other one alone', async () => {
        await openStorageUsage()
        const count = labelled(strings.retentionCount)
        count.value = '25'
        count.dispatchEvent(new Event('change', { bubbles: true }))
        await settle()
        expect(state.setRetentionPolicy).toHaveBeenCalledWith('connection-1', {
            keepCount: 25,
            keepDays: 30,
        })
    })

    it.each([false, true])('shows local request estimates only when provided (%s)', async (hasCounters) => {
        state.getQuota.mockResolvedValue({
            connectionId: 'connection-1',
            buckets: hasCounters ? [{
                id: 'mybox-download-day', used: '4', limit: '500',
                unit: 'requests', localEstimate: true,
            }] : [],
            storage: {
                providerPhysicalBytes: null, providerPhysicalKnown: false,
                locallyUploadedBytesLowerBound: '0', locallyUploadedObjectCountLowerBound: '0',
                locallyUploadedCoverage: 'cached-upload-receipts',
            },
        })
        await openStorageUsage()
        await settle()
        expect(target.textContent?.includes(strings.requestUsage)).toBe(hasCounters)
        expect(target.textContent?.includes('4 / 500 requests')).toBe(hasCounters)
        expect(target.textContent).not.toContain('496')
    })

    it('puts back the stored value when the typed one is out of range', async () => {
        await openStorageUsage()
        const days = labelled(strings.retentionDays)
        days.value = '3'
        days.dispatchEvent(new Event('change', { bubbles: true }))
        await settle()
        expect(state.setRetentionPolicy).not.toHaveBeenCalled()
        expect(days.value).toBe('30')
    })
})

describe('the storage usage of two connections', () => {
    const second = { ...connection(10, 30), id: 'connection-2', displayName: 'Second' }
    const status = (entries: { connectionId: string, objects: number }[]) => ({ policy: 'remote', localBytes: 0, remoteBytes: 3,
        remoteObjects: 3, serverBytes: 0, serverObjects: 0, externalObjects: entries, unavailableObjects: 0, evictedBytes: 0 })
    const remoteOnly = (id: string) => {
        const panel = target.querySelector(`#${id}-quota-panel`)
        if (!panel) throw new Error(`Missing the storage usage of ${id}`)
        const term = [...panel.querySelectorAll('dt')].find(item => item.textContent?.trim() === strings.remoteOnlyFiles)
        return term?.nextElementSibling?.textContent?.trim()
    }
    async function openBoth(): Promise<void> {
        const tabs = [...target.querySelectorAll('button')].filter(item => item.textContent?.trim() === strings.quota)
        expect(tabs).toHaveLength(2)
        for (const tab of tabs) {
            tab.click()
            await settle()
        }
    }
    beforeEach(async () => {
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false },
            connections: [connection(10, 30), second], jobs: [] })
        // The row does not wait on the service's own usage.
        state.getQuota.mockRejectedValue({ kind: 'transient', httpStatus: null, retryAtMs: null })
        state.listHistory.mockResolvedValue({ items: [] })
        target = document.createElement('div')
        document.body.append(target)
        component = mount(ExternalStorageSettings, { target })
        await settle()
    })
    afterEach(() => {
        if (component) unmount(component)
        component = undefined
        target.remove()
        vi.clearAllMocks()
    })

    it('shows for each connection the files only it holds', async () => {
        state.residency.mockResolvedValue(status([{ connectionId: 'connection-1', objects: 2 }]))
        await openBoth()
        expect(remoteOnly('connection-1')).toBe('2')
        expect(remoteOnly('connection-2')).toBe('0')
    })

    it('shows unknown when the status cannot be read', async () => {
        state.residency.mockRejectedValue({ code: 'local-storage' })
        await openBoth()
        expect(remoteOnly('connection-1')).toBe(strings.unknownUsage)
        expect(remoteOnly('connection-2')).toBe(strings.unknownUsage)
    })
})

describe('a running job', () => {
    function job(counters?: 'prepared' | 'transferred') {
        return {
            id: 'job-1',
            connectionId: 'connection-1',
            kind: 'backup' as const,
            state: 'running' as const,
            phase: 'packing',
            ...(counters ? { counters } : {}),
            completedBytes: '400',
            totalBytes: '800',
            completedItems: '4',
            totalItems: '8',
            startedAtMs: '1',
            updatedAtMs: '2',
        }
    }

    async function show(counters?: 'prepared' | 'transferred'): Promise<string> {
        vi.mocked(requestExternalStorageNow).mockResolvedValue({ kind: 'cancelled' })
        state.getState.mockResolvedValue({
            supported: true,
            selection: { kind: 'none', selectionEpoch: '0', paused: false },
            connections: [connection(10, 30)],
            jobs: [job(counters)],
        })
        state.getQuota.mockRejectedValue({ kind: 'transient', httpStatus: null, retryAtMs: null })
        state.listHistory.mockResolvedValue({ items: [] })
        target = document.createElement('div')
        document.body.append(target)
        component = mount(ExternalStorageSettings, { target })
        await settle()
        return target.textContent ?? ''
    }

    afterEach(() => {
        if (component) unmount(component)
        component = undefined
        target.remove()
        vi.clearAllMocks()
    })

    it('says whether the counted bytes are prepared or sent', async () => {
        expect(await show('prepared')).toContain(`${strings.jobCounters.prepared} 400 B / 800 B`)
        if (component) unmount(component)
        component = undefined
        target.remove()
        expect(await show('transferred')).toContain(`${strings.jobCounters.transferred} 400 B / 800 B`)
    })

    it('leaves a job that counts neither unlabelled', async () => {
        const shown = await show()
        expect(shown).toContain('400 B / 800 B')
        expect(shown).not.toContain(strings.jobCounters.prepared)
        expect(shown).not.toContain(strings.jobCounters.transferred)
    })

    it('reports a check that could not finish on its root without a verified count', async () => {
        vi.mocked(requestExternalStorageNow).mockResolvedValue({ kind: 'cancelled' })
        state.getState.mockResolvedValue({
            supported: true,
            selection: { kind: 'none', selectionEpoch: '0', paused: false },
            connections: [connection(10, 30)],
            jobs: [{
                ...job(),
                kind: 'check-repository' as const,
                state: 'succeeded' as const,
                phase: 'complete',
                result: { stopReason: 'expired' },
            }],
        })
        state.getQuota.mockRejectedValue({ kind: 'transient', httpStatus: null, retryAtMs: null })
        state.listHistory.mockResolvedValue({ items: [] })
        target = document.createElement('div')
        document.body.append(target)
        component = mount(ExternalStorageSettings, { target })
        await settle()
        const shown = target.textContent ?? ''
        expect(shown).toContain(strings.checkExpired)
        expect(shown).not.toContain(strings.completed)
        expect(shown).not.toContain(strings.checkSummary.split('{0}')[0])
    })
})

describe('removing a connection', () => {
    const held = (entries: { connectionId: string, objects: number }[]) => ({ policy: 'remote', localBytes: 0, remoteBytes: 1, remoteObjects: 1,
        serverBytes: 0, serverObjects: 0, externalObjects: entries, unavailableObjects: 0, evictedBytes: 0 })
    const removeButton = () => [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.remove)!
    beforeEach(async () => {
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false },
            connections: [connection(10, 30)], jobs: [] })
        state.listHistory.mockResolvedValue({ items: [] })
        state.removeConnection.mockResolvedValue(undefined)
        target = document.createElement('div')
        document.body.append(target)
        component = mount(ExternalStorageSettings, { target })
        await settle()
    })
    afterEach(() => {
        if (component) unmount(component)
        component = undefined
        target.remove()
        vi.clearAllMocks()
    })

    it.each([
        { name: 'holds no file', status: () => state.residency.mockResolvedValue(held([])) },
        { name: 'is not the one holding files', status: () => state.residency.mockResolvedValue(held([{ connectionId: 'other', objects: 3 }])) },
    ])('asks the plain confirmation when the status $name', async ({ status }) => {
        status()
        vi.mocked(alertConfirm).mockResolvedValue(true)
        removeButton().click(); await settle()
        expect(alertCheckboxConfirm).not.toHaveBeenCalled(); expect(alertConfirm).toHaveBeenCalledOnce()
        expect(state.removeConnection).toHaveBeenCalledExactlyOnceWith('connection-1'); expect(state.downloadRemote).not.toHaveBeenCalled()
    })
    it('offers a download with unknown wording when residency cannot be read', async () => {
        state.residency.mockRejectedValue({ code: 'local-storage' })
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: false, checked: false })
        removeButton().click(); await settle()
        expect(alertCheckboxConfirm).toHaveBeenCalledWith(expect.objectContaining({ description: strings.removeRemoteOnlyUnknown }))
        expect(alertConfirm).not.toHaveBeenCalled()
        expect(state.removeConnection).not.toHaveBeenCalled()
    })
    it('asks once with an unchecked download option for files only this connection holds', async () => {
        state.residency.mockResolvedValue(held([{ connectionId: 'connection-1', objects: 2 }]))
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: false, checked: false })
        removeButton().click(); await settle()
        expect(alertCheckboxConfirm).toHaveBeenCalledExactlyOnceWith({
            title: strings.removeRemoteOnlyTitle, description: strings.removeRemoteOnly, checkboxLabel: strings.downloadThenRemove,
            actionLabel: strings.remove, cancelLabel: strings.cancel, requireChecked: false,
        })
        expect(alertConfirm).not.toHaveBeenCalled()
        expect(state.removeConnection).not.toHaveBeenCalled(); expect(state.downloadRemote).not.toHaveBeenCalled()
    })
    it('removes without downloading when the option stays unchecked', async () => {
        state.residency.mockResolvedValue(held([{ connectionId: 'connection-1', objects: 2 }]))
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: true, checked: false })
        removeButton().click(); await settle()
        expect(state.downloadRemote).not.toHaveBeenCalled(); expect(state.removeConnection).toHaveBeenCalledExactlyOnceWith('connection-1')
    })
    it('downloads the files this connection holds, then removes it', async () => {
        state.residency.mockResolvedValue(held([{ connectionId: 'connection-1', objects: 2 }]))
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: true, checked: true })
        state.downloadRemote.mockResolvedValue(held([]))
        removeButton().click(); await settle()
        expect(state.downloadRemote).toHaveBeenCalledExactlyOnceWith('connection-1', { signal: expect.any(AbortSignal) })
        expect(state.removeConnection).toHaveBeenCalledExactlyOnceWith('connection-1')
        expect(state.downloadRemote.mock.invocationCallOrder[0]).toBeLessThan(state.removeConnection.mock.invocationCallOrder[0])
        expect(target.querySelector('[role="alert"]')).toBeNull()
    })
    it('keeps the connection and explains it when the download fails', async () => {
        state.residency.mockResolvedValue(held([{ connectionId: 'connection-1', objects: 2 }]))
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: true, checked: true })
        state.downloadRemote.mockRejectedValue({ code: 'required-asset-unavailable', status: 409, retryable: false })
        removeButton().click(); await settle()
        expect(state.removeConnection).not.toHaveBeenCalled()
        expect(target.querySelector('[role="alert"]')?.textContent).toBe(strings.downloadFailedKeptConnection)
        expect(state.residency).toHaveBeenCalledTimes(2)
    })
    it('keeps the connection without a message when the download is cancelled', async () => {
        state.residency.mockResolvedValue(held([{ connectionId: 'connection-1', objects: 2 }]))
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: true, checked: true })
        state.downloadRemote.mockRejectedValue({ code: 'cancelled', status: 409, retryable: true })
        removeButton().click(); await settle()
        expect(state.removeConnection).not.toHaveBeenCalled()
        expect(target.querySelector('[role="alert"]')).toBeNull()
    })
    it('keeps busy ownership after cancellation until the download settles, then refreshes remaining files', async () => {
        state.residency.mockResolvedValue(held([{ connectionId: 'connection-1', objects: 2 }]))
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: true, checked: true })
        let finish!: () => void
        state.downloadRemote.mockImplementation(() => new Promise<void>(resolve => { finish = resolve }))
        removeButton().click(); await settle()
        const signal = state.downloadRemote.mock.calls[0][1].signal as AbortSignal
        const cancel = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === strings.cancel)!
        cancel.click(); await settle()
        expect(signal.aborted).toBe(true)
        expect(removeButton().disabled).toBe(true)
        expect(state.residency).toHaveBeenCalledOnce()
        expect(state.removeConnection).not.toHaveBeenCalled()
        state.residency.mockResolvedValue(held([{ connectionId: 'connection-1', objects: 1 }]))
        finish(); await settle()
        expect(state.residency).toHaveBeenCalledTimes(2)
        expect(removeButton().disabled).toBe(false)
        expect(state.removeConnection).not.toHaveBeenCalled()
        expect(target.querySelector('[role="alert"]')).toBeNull()
    })
    it('preserves the download failure when the settled residency refresh also fails', async () => {
        state.residency.mockResolvedValueOnce(held([{ connectionId: 'connection-1', objects: 2 }])).mockRejectedValue({ code: 'local-storage' })
        vi.mocked(alertCheckboxConfirm).mockResolvedValue({ confirmed: true, checked: true })
        state.downloadRemote.mockRejectedValue({ code: 'required-asset-unavailable' })
        removeButton().click(); await settle()
        expect(state.residency).toHaveBeenCalledTimes(2)
        expect(target.querySelector('[role="alert"]')?.textContent).toBe(strings.downloadFailedKeptConnection)
        expect(state.removeConnection).not.toHaveBeenCalled()
    })
})

it('explains a sync stopped because the previous storage could not be reached', async () => {
    state.failures.set('connection-1', { kind: 'previousStorageUnavailable' })
    state.getState.mockResolvedValue({ supported: true, selection: { kind: 'external', connectionId: 'connection-1', selectionEpoch: '0', paused: false },
        connections: [{ ...connection(10, 30), purpose: 'sync', strategy: 'sequential' }], jobs: [] })
    state.listHistory.mockResolvedValue({ items: [] })
    target = document.createElement('div')
    document.body.append(target)
    component = mount(ExternalStorageSettings, { target }); await settle()
    expect(target.textContent).toContain('The previous storage could not be reached.')
    expect(target.textContent).not.toContain(strings.errorGeneric)
    unmount(component); component = undefined; target.remove()
})

it('explains a connection not made because the files could not be downloaded first', async () => {
    state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false },
        connections: [{ ...connection(10, 30), purpose: 'sync', strategy: 'sequential' }], jobs: [] })
    state.listHistory.mockResolvedValue({ items: [] })
    state.bind.mockRejectedValue(Object.assign(new Error('download failed'), { code: 'previous-files-download-failed' }))
    target = document.createElement('div')
    document.body.append(target)
    component = mount(ExternalStorageSettings, { target }); await settle()
    const toggle = [...target.querySelectorAll('label')].find(item => item.textContent?.trim() === strings.sync)?.querySelector('input')
    expect(toggle).toBeDefined()
    toggle!.click(); await settle()
    expect(state.bind).toHaveBeenCalledWith({ kind: 'external', connectionId: 'connection-1' })
    expect(target.querySelector('[role="alert"]')?.textContent).toBe('The files could not be downloaded, so the connection was not made.')
    unmount(component); component = undefined; target.remove()
})
