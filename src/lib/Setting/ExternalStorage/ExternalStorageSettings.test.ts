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
}))

vi.mock('src/ts/platform', () => ({ isTauri: true, isTauriAndroid: false, isTauriIOS: false }))
vi.mock('@tauri-apps/plugin-os', () => ({ type: () => 'windows' }))
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }))
vi.mock('qrcode', () => ({ default: { toDataURL: vi.fn() } }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(), alertNormal: vi.fn(), alertCheckboxConfirm: vi.fn() }))
vi.mock('src/ts/storage/sync/external/lwwProduction', () => ({ requestExternalLwwNow: state.syncNow, supportsExternalLwwNewDevice: () => state.remedy, subscribeExternalLwwFailures(callback: (value: ReadonlyMap<string, unknown>) => void) { callback(new Map(state.failures)); return () => {} } }))
vi.mock('src/ts/storage/sync/bindingRegistry', () => ({ bindSyncTarget: state.bind, unbindSyncTarget: state.unbind }))
vi.mock('src/lang', () => ({ language: { lwwSync: { newDeviceAction: 'Connect as new device', clockBlocked: 'Correct the device clock and retry.' } } }))
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
    }),
}))

import { alertConfirm, alertCheckboxConfirm } from 'src/ts/alert'
import ExternalStorageSettings from './ExternalStorageSettings.svelte'
import { requestExternalStorageNow, resumeExternalStorageJob, requestExternalStorageDeleteHistory } from 'src/ts/storage/sync/external/production'
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
            selection: { kind: 'none', selectionEpoch: '0', paused: false, decisionRequired: false },
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
            result: { decisionRequired: true, reason: 'publication-unknown' } }
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


    it.each(['mybox', 'github_releases', 'gitlab_packages'])('does not offer Sync on a %s backup connection', async providerId => {
        if (component) await unmount(component)
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false, decisionRequired: false }, connections: [{ ...connection(10, 30), providerId }], jobs: [] })
        component = mount(ExternalStorageSettings, { target }); await settle()
        expect([...target.querySelectorAll('button')].some(item => item.textContent?.trim() === strings.sync)).toBe(false)
        expect(target.textContent).not.toContain(strings.automaticSync)
    })
    it('offers actual native new-device recovery through the shared binding action', async () => {
        if (component) await unmount(component)
        state.failures.set('connection-1', { kind: 'corrupt' }); state.bind.mockResolvedValue({ kind: 'cancelled' })
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'external', connectionId: 'connection-1', selectionEpoch: '0', paused: false, decisionRequired: false }, connections: [{ ...connection(10, 30), purpose: 'sync', strategy: 'sequential' }], jobs: [] })
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
        state.getState.mockResolvedValue({ supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false, decisionRequired: false }, connections: [capable], jobs: [] })
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
            selection: { kind: 'none', selectionEpoch: '0', paused: false, decisionRequired: false },
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
            selection: { kind: 'none', selectionEpoch: '0', paused: false, decisionRequired: false },
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
