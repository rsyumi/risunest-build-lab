<script lang="ts">
    import { onDestroy, onMount } from 'svelte'
    import QRCode from 'qrcode'
    import { runWithMobileBackgroundTask, measuredTaskPercent } from 'src/ts/mobileBackgroundTask'
    import TextInput from 'src/lib/UI/GUI/TextInput.svelte'
    import NumberInput from 'src/lib/UI/GUI/NumberInput.svelte'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingButton from '../RisuNest/SettingButton.svelte'
    import SettingProgress from '../RisuNest/SettingProgress.svelte'
    import SettingToggle from '../RisuNest/SettingToggle.svelte'
    import SettingNotice from '../RisuNest/SettingNotice.svelte'
    import StatusBadge from '../RisuNest/StatusBadge.svelte'
    import { CloudIcon, DatabaseIcon, PackageIcon, PinIcon, ServerIcon } from '@lucide/svelte'
    import { alertConfirm, alertNormal, alertCheckboxConfirm } from 'src/ts/alert'
    import { DBState } from 'src/ts/stores.svelte'
    import { getExternalStorageBridge } from 'src/ts/storage/sync/external/bridge'
    import { bindSyncTarget, unbindSyncTarget } from 'src/ts/storage/sync/bindingRegistry'
    import { createNativeSyncBindingBridge } from 'src/ts/storage/sync/bindingNative'
    import { subscribeSyncBindingChanges } from 'src/ts/storage/sync/bindingChanges'
    import { requestExternalLwwNow, subscribeExternalLwwFailures, supportsExternalLwwNewDevice } from 'src/ts/storage/sync/external/lwwProduction'
    import BindingTargetSwitch from 'src/ts/storage/sync/BindingTargetSwitch.svelte'
    import { PREVIOUS_FILES_DOWNLOAD_FAILED } from 'src/ts/storage/sync/bindingFlow'
    import { language } from 'src/lang'
    import { externalJobIsActive, externalJobIsPaused, externalJobProgress, mergeExternalHistoryItems } from 'src/ts/storage/sync/external/connection'
    import {
        refreshExternalStorageProductionState,
        requestExternalStorageNow,
        resumeExternalStorageJob,
        requestExternalStorageRestore,
        requestExternalStorageDeleteHistory,
        stopExternalStorageRestore,
    } from 'src/ts/storage/sync/external/production'
    import type {
        ExternalConnectionResult,
        ExternalConnectionSummary,
        ExternalHistoryItem,
        ExternalJobSummary,
        ExternalQuotaSummary,
        ExternalConnectionSettingsMaterial,
        ExternalRetentionPolicy,
        ExternalRestoreArea,
        ExternalStorageState,
        ExternalSnapshotExportProgress,
    } from 'src/ts/storage/sync/external/types'
    import { externalRestoreAreas } from 'src/ts/storage/sync/external/restoreScope'
    import { downloadRemoteAssets, getAssetResidencyStatus } from 'src/ts/storage/sync/serverAssetResidency'
    import { formatRisuNestStorageBytes } from 'src/ts/storage/risuNestStorageDashboard'
    import ConnectionForm from './ConnectionForm.svelte'
    import { externalConnectionTitle, externalErrorKind, externalErrorMessage, externalStorageStrings } from './strings'

    const bridge = getExternalStorageBridge()
    /** How many empty history pages one request reads past before stopping. */
    const EMPTY_HISTORY_STEPS = 4
    /** What a repository accepts for each retention limit. */
    const RETENTION_LIMITS = { keepCount: [1, 1000], keepDays: [7, 3650] } as const
    /** How long disconnecting waits for the file check before it asks without the check's answer. */
    const REMOTE_ONLY_CHECK_MS = 3_000
    const strings = $derived(externalStorageStrings(DBState.db.language))
    let storageState = $state<ExternalStorageState | null>(null)
    let adding = $state(false)
    let renewalConnection = $state<ExternalConnectionSummary | undefined>()
    let unlockConnection = $state<ExternalConnectionSummary | undefined>()
    let unlockKey = $state('')
    let destroyed = false
    let stopJobEvents: (() => void) | undefined
    let stopSyncFailures: (() => void) | undefined
    let syncFailures = $state<ReadonlyMap<string, unknown>>(new Map())
    let busy = $state(false)
    /** Which action is running, so only the button that started it shows a spinner. */
    let activeAction = $state('')
    let error = $state('')
    /** A failed background refresh, which the next successful refresh clears. */
    let pollError = $state('')
    /** The details tab picked on each card. A card shows its history until another tab is picked. */
    let expanded = $state<Record<string, 'history' | 'quota'>>({})
    let history = $state<Record<string, ExternalHistoryItem[]>>({})
    let historyError = $state<Record<string, string>>({})
    /** Cards whose history was read once, so a failing remote is not read again on every poll. */
    const historyRequested = new Set<string>()
    let historyCursor = $state<Record<string, string | undefined>>({})
    let historyLoading = $state<Record<string, boolean>>({})
    let quota = $state<Record<string, ExternalQuotaSummary>>({})
    let remoteOnly = $state<Record<string, number | null>>({})
    let removalDownload = $state<{ connectionId: string; controller: AbortController; cancelling: boolean } | null>(null)
    let recoveryKey = $state('')
    let recoveryPanel = $state<HTMLDivElement | undefined>()
    let connectionSettings = $state<ExternalConnectionSettingsMaterial | null>(null)
    let connectionSettingsPanel = $state<HTMLDivElement | undefined>()
    let connectionSettingsQr = $state('')
    /** The history entry whose restore scope is open, and what is ticked in it. */
    let exportRun = $state<{ id: string; connectionId: string; progress?: ExternalSnapshotExportProgress } | null>(null)
    /** Jobs that add or change history rows, by job ID, so the open history reloads when they finish. */
    const historyJobs = new Map<string, string>()
    let pollTimer: ReturnType<typeof setTimeout> | undefined
    /** The state a sync switch was set to while its change runs. Otherwise it shows the device's sync target. */
    let syncRequest = $state<Record<string, boolean>>({})
    /** The state an automatic backup switch was set to while its change runs. Otherwise it shows the stored setting. */
    let automaticRequest = $state<Record<string, boolean>>({})
    let stopBindingChanges: (() => void) | undefined

    async function setSyncBinding(connection: ExternalConnectionSummary, enabled: boolean): Promise<void> {
        busy = true
        syncRequest[connection.id] = enabled
        try {
            if (enabled) await bindSyncTarget({ kind: 'external', connectionId: connection.id })
            else {
                // A switch left over from an older state must not stop another sync target.
                const current = (await createNativeSyncBindingBridge().state()).target
                if (current.kind === 'external' && current.connectionId === connection.id) await unbindSyncTarget()
            }
            await refreshExternalStorageProductionState()
            error = ''
        } catch (reason) {
            error = externalErrorKind(reason) === PREVIOUS_FILES_DOWNLOAD_FAILED ? language.lwwSync.downloadFailedNotConnected : externalErrorMessage(strings, reason)
        }
        finally {
            await refresh(true)
            delete syncRequest[connection.id]
            busy = false
        }
    }
    async function syncNow(connection: ExternalConnectionSummary): Promise<void> {
        busy = true
        try { await requestExternalLwwNow(connection.id) }
        catch (reason) { error = externalErrorMessage(strings, reason) }
        finally { busy = false }
    }
    function beginRestore(connection: ExternalConnectionSummary, item: ExternalHistoryItem): void {
        try {
            void runJob(connection, 'restore', {
                snapshotId: item.snapshotId,
                restoreAreas: externalRestoreAreas(item, []),
            })
        } catch (reason) { error = externalErrorMessage(strings, reason) }
    }
    onMount(() => {
        const openExternalStorage = (event: Event) => {
            const providerId = (event as CustomEvent<{ providerId?: string }>).detail?.providerId
            if (providerId && providerId !== 'google_drive') return
            adding = true
            requestAnimationFrame(() => {
                document.getElementById('risunest-external-storage')?.scrollIntoView({ block: 'start' })
            })
        }
        window.addEventListener('risunest:open-external-storage', openExternalStorage)
        return () => window.removeEventListener('risunest:open-external-storage', openExternalStorage)
    })

    async function refresh(silent = false): Promise<void> {
        if (!silent) {
            busy = true
            activeAction = 'refresh'
        }
        try {
            storageState = await bridge.getState()
            for (const connection of storageState.connections) {
                if (historyRequested.has(connection.id)) continue
                historyRequested.add(connection.id)
                void loadHistory(connection, false)
            }
            for (const job of storageState.jobs) {
                if (job.kind === 'backup' && externalJobIsActive(job)) historyJobs.set(job.id, job.connectionId)
            }
            for (const [jobId, connectionId] of historyJobs) {
                const completed = storageState.jobs.find(job => job.id === jobId)
                if (!completed || externalJobIsActive(completed)) continue
                historyJobs.delete(jobId)
                const connection = storageState.connections.find(item => item.id === connectionId)
                if (completed.state === 'succeeded' && connection && selectedTab(connectionId) === 'history') {
                    await loadHistory(connection, false)
                }
            }
            if (!silent) error = ''
            pollError = ''
        } catch (reason) {
            if (silent) pollError = externalErrorMessage(strings, reason)
            else error = externalErrorMessage(strings, reason)
        } finally {
            if (!silent) {
                busy = false
                activeAction = ''
            }
        }
    }

    async function refreshWithHistory(): Promise<void> {
        await refresh()
        const open = storageState?.connections.filter(connection => selectedTab(connection.id) === 'history') ?? []
        await Promise.all(open.map(connection => loadHistory(connection, false)))
    }

    function schedulePoll(whileBusy = false): void {
        clearTimeout(pollTimer)
        if (destroyed) return
        if (!storageState?.jobs.some(externalJobIsActive) && !(whileBusy && busy)) return
        pollTimer = setTimeout(async () => {
            await refresh(true)
            schedulePoll(whileBusy)
        }, 1200)
    }

    async function onConnected(result: ExternalConnectionResult): Promise<void> {
        busy = false
        adding = false
        const renewed = renewalConnection !== undefined
        renewalConnection = undefined
        if (result.recovery) recoveryKey = result.recovery.key
        await refreshExternalStorageProductionState()
        await refresh()
        schedulePoll()
        if (renewed) {
            const job = activeJob(result.connection)
            if (job && externalJobIsPaused(job)) await resumeJob(result.connection, job)
        }
    }

    /** Re-enters the paused job instead of starting one beside it. */
    async function resumeJob(connection: ExternalConnectionSummary, job: ExternalJobSummary): Promise<void> {
        if (job.kind === 'restore' && job.restoreRequest) {
            await runJob(connection, 'restore', job.restoreRequest)
            return
        }
        busy = true
        try {
            const operation = resumeExternalStorageJob(job)
            schedulePoll(true)
            const result = await operation
            if (result.kind === 'complete' && (job.kind === 'pin-history' || job.kind === 'delete-history')) await loadHistory(connection, false)
            if (result.kind === 'blocked' && !result.job) error = externalErrorMessage(strings, result.error ?? result.cause)
            await refresh(true)
            schedulePoll()
        } catch (reason) { error = externalErrorMessage(strings, reason) }
        finally { busy = false }
    }

    async function runJob(
        connection: ExternalConnectionSummary,
        job: 'backup' | 'restore' | 'pin-history' | 'cleanup' | 'check-repository',
        details: {
            snapshotId?: string
            jobId?: string
            restoreAreas?: ExternalRestoreArea[]
        } = {},
    ): Promise<void> {
        if (job === 'restore') {
            const binding = await createNativeSyncBindingBridge().state()
            const answer = await alertCheckboxConfirm({
                title: language.lwwSync.restoreTitle,
                description: binding.target.kind === 'none'
                    ? language.lwwSync.restoreDescription
                    : language.lwwSync.restoreDescriptionBound,
                checkboxLabel: language.lwwSync.restoreAcknowledge,
                actionLabel: language.lwwSync.restoreAction,
                cancelLabel: strings.cancel,
                requireChecked: true,
            })
            if (!answer.confirmed) return
        }
        busy = true
        activeAction = `${job}:${connection.id}`
        try {
            if (job === 'backup' || job === 'cleanup') {
                const operation = requestExternalStorageNow(connection.id, job)
                schedulePoll(true)
                const result = await operation
                if (job === 'backup' && result.kind === 'complete') historyJobs.set(result.job.id, connection.id)
                if (result.kind === 'blocked' && !result.job) {
                    error = externalErrorMessage(strings, result.error ?? result.cause ?? { code: result.reason })
                }
            } else if (job === 'restore') {
                if (!details.snapshotId || !details.restoreAreas) {
                    throw new Error('Missing restore request')
                }
                const operation = requestExternalStorageRestore(
                    connection.id,
                    details.snapshotId,
                    details.restoreAreas,
                )
                schedulePoll(true)
                await operation
            } else {
                const started = await bridge.startJob({
                    connectionId: connection.id,
                    kind: job,
                    reason: 'manual',
                    ...details,
                })
                if (job === 'pin-history') {
                    if (started.state === 'succeeded') await loadHistory(connection, false)
                    else historyJobs.set(started.id, connection.id)
                }
            }
            await refresh(true)
            schedulePoll()
        } catch (reason) {
            const message = externalErrorMessage(strings, reason)
            if (job === 'restore') await refresh(true)
            if (job !== 'restore' || !rowShowsStoppedRestore(connection.id, message)) error = message
        } finally {
            busy = false
            activeAction = ''
        }
    }

    /** A restore the app stopped shows its error on the connection row, which the section does not repeat. */
    function rowShowsStoppedRestore(connectionId: string, message: string): boolean {
        const current = storageState?.connections.find(item => item.id === connectionId)
        const latest = current && activeJob(current)
        return !!current && !current.lastError && latest?.state === 'cancelled' && latest.stoppedByApp === true
            && jobLabel(latest) === message
    }

    async function deleteHistory(
        connection: ExternalConnectionSummary,
        item: ExternalHistoryItem,
    ): Promise<void> {
        if (!item.pointId || !item.pointObservation || !item.deletable) return
        busy = true
        try {
            const preparation = await bridge.prepareHistoryDelete(
                connection.id,
                item.pointId,
                item.pointObservation,
            )
            const descriptions = [
                ...(!preparation.sameDevice ? [strings.deleteOtherDeviceConfirm] : []),
                ...(preparation.lastRetained ? [strings.deleteLastRetainedConfirm] : []),
            ]
            if (descriptions.length) {
                const answer = await alertCheckboxConfirm({
                    title: strings.deleteHistoryConfirm,
                    description: descriptions.join(' '),
                    checkboxLabel: strings.deleteHistoryAcknowledge,
                    actionLabel: strings.deleteHistory,
                    cancelLabel: strings.cancel,
                    requireChecked: true,
                })
                if (!answer.confirmed) return
            } else if (!(await alertConfirm(strings.deleteHistoryConfirm))) return
            const operation = requestExternalStorageDeleteHistory(
                connection.id,
                item,
                !preparation.sameDevice,
                preparation.lastRetained,
            )
            schedulePoll(true)
            await operation
            await loadHistory(connection, false)
            await refresh(true)
            schedulePoll()
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
            busy = false
        }
    }

    async function cancelJob(job: ExternalJobSummary): Promise<void> {
        try {
            await bridge.cancelJob(job.id)
            await refresh(true)
            schedulePoll()
        } catch (reason) { error = externalErrorMessage(strings, reason) }
    }

    async function stopRestore(job: ExternalJobSummary): Promise<void> {
        if (busy || !(await alertConfirm(`${strings.stopRestoreTitle}\n${strings.stopRestoreDescription}`))) return
        busy = true
        try {
            await stopExternalStorageRestore(job.id)
            await refresh(true)
        } catch (reason) { error = externalErrorMessage(strings, reason) }
        finally { busy = false }
    }


    async function setAutomaticWork(connection: ExternalConnectionSummary, enabled: boolean): Promise<void> {
        if (!storageState || busy) return
        busy = true
        automaticRequest[connection.id] = enabled
        try {
            await bridge.setAutomaticBackupPaused(connection.id, !enabled)
            await refreshExternalStorageProductionState()
            error = ''
        } catch (reason) { error = externalErrorMessage(strings, reason) }
        finally {
            await refresh(true)
            delete automaticRequest[connection.id]
            busy = false
        }
    }

    async function unlock(): Promise<void> {
        const connection = unlockConnection
        if (!connection || busy) return
        busy = true
        try {
            await bridge.unlockConnection(connection.id, unlockKey)
            unlockKey = ''
            unlockConnection = undefined
            await refreshExternalStorageProductionState()
            await refresh(true)
        } catch (reason) { error = externalErrorMessage(strings, reason) }
        finally { busy = false }
        const job = activeJob(connection)
        if (!unlockConnection && job && externalJobIsPaused(job)) await resumeJob(connection, job)
    }


    function selectedTab(connectionId: string): 'history' | 'quota' {
        return expanded[connectionId] ?? 'history'
    }

    async function openDetails(connection: ExternalConnectionSummary, kind: 'history' | 'quota'): Promise<void> {
        expanded[connection.id] = kind
        try {
            if (kind === 'history') await loadHistory(connection, false)
            if (kind === 'quota') {
                void loadRemoteOnlyFiles(connection)
                quota[connection.id] = await bridge.getQuota(connection.id)
            }
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        }
    }

    /** Files only this connection holds, as the residency status counts them. */
    async function remoteOnlyFiles(connection: ExternalConnectionSummary): Promise<number> {
        return (await getAssetResidencyStatus()).externalObjects.find(entry => entry.connectionId === connection.id)?.objects ?? 0
    }

    async function loadRemoteOnlyFiles(connection: ExternalConnectionSummary): Promise<void> {
        try {
            remoteOnly[connection.id] = await remoteOnlyFiles(connection)
        } catch {
            remoteOnly[connection.id] = null
        }
    }


    async function loadHistory(connection: ExternalConnectionSummary, append: boolean): Promise<void> {
        if (historyLoading[connection.id]) return
        historyLoading[connection.id] = true
        try {
            let items = append ? history[connection.id] ?? [] : []
            let cursor = append ? historyCursor[connection.id] : undefined
            // The first page covers kept and conflict copies only, so a
            // repository that just holds backups answers it with nothing. Keep
            // reading until this step has something to show.
            for (let step = 0; step < EMPTY_HISTORY_STEPS; step += 1) {
                const page = await bridge.listHistory(connection.id, cursor)
                items = mergeExternalHistoryItems(items, page.items)
                cursor = page.nextCursor
                if (page.items.length > 0 || !cursor) break
            }
            history[connection.id] = items
            historyCursor[connection.id] = cursor
            delete historyError[connection.id]
        } catch (reason) {
            historyError[connection.id] = externalErrorMessage(strings, reason)
        } finally {
            historyLoading[connection.id] = false
        }
    }


    async function changeRetentionPolicy(
        connection: ExternalConnectionSummary,
        policy: ExternalRetentionPolicy,
    ): Promise<void> {
        busy = true
        try {
            await bridge.setRetentionPolicy(connection.id, policy)
            await refresh(true)
        } catch (failure) {
            error = externalErrorMessage(strings, failure)
        } finally {
            busy = false
        }
    }

    /** A value outside what the repository accepts goes back to the stored one. */
    function commitRetentionLimit(
        connection: ExternalConnectionSummary,
        limit: keyof ExternalRetentionPolicy,
        input: HTMLInputElement,
    ): void {
        const policy = connection.retentionPolicy
        const [lowest, highest] = RETENTION_LIMITS[limit]
        const value = Number(input.value)
        if (!Number.isInteger(value) || value < lowest || value > highest) {
            input.value = String(policy[limit])
            return
        }
        if (value !== policy[limit]) void changeRetentionPolicy(connection, { ...policy, [limit]: value })
    }

    async function removeConnection(connection: ExternalConnectionSummary): Promise<void> {
        busy = true
        activeAction = `remove:${connection.id}`
        let held: number | undefined
        let timer: ReturnType<typeof setTimeout> | undefined
        try {
            held = await Promise.race([
                remoteOnlyFiles(connection),
                new Promise<undefined>(resolve => { timer = setTimeout(() => resolve(undefined), REMOTE_ONLY_CHECK_MS) }),
            ])
        } catch {} finally {
            clearTimeout(timer)
            busy = false
            activeAction = ''
        }
        const remoteOnly = held !== 0
        // The card's own two lines tell apart connections to one service and account.
        const name = `${externalConnectionTitle(strings, connection)}\n${connectionPlace(connection)}`
        const choice = await alertCheckboxConfirm({
            title: strings.removeTitle,
            description: remoteOnly ? `${name}\n\n${held === undefined ? strings.removeRemoteOnlyUnknown : strings.removeRemoteOnly}` : name,
            checkboxLabel: remoteOnly ? strings.downloadThenRemove : strings.removeAcknowledge,
            actionLabel: strings.remove,
            cancelLabel: strings.cancel,
            requireChecked: !remoteOnly,
        })
        if (!choice.confirmed) return
        const download = remoteOnly && choice.checked
        busy = true
        activeAction = `remove:${connection.id}`
        try {
            if (download) {
                const controller = new AbortController()
                removalDownload = { connectionId: connection.id, controller, cancelling: false }
                try {
                    await downloadRemoteAssets(connection.id, { signal: controller.signal })
                    if (controller.signal.aborted) return
                } catch (reason) {
                    if (!controller.signal.aborted && externalErrorKind(reason) !== 'cancelled') error = strings.downloadFailedKeptConnection
                    return
                } finally {
                    removalDownload = null
                }
            }
            await bridge.removeConnection(connection.id)
            await refreshExternalStorageProductionState()
            await refresh(true)
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
            await loadRemoteOnlyFiles(connection)
            busy = false
            activeAction = ''
        }
    }


    async function displayConnectionSettings(material: ExternalConnectionSettingsMaterial): Promise<void> {
        connectionSettings = material
        if (!material.qrPayload) {
            connectionSettingsQr = ''
            return
        }
        try {
            connectionSettingsQr = await QRCode.toDataURL(material.qrPayload, { margin: 2, width: 240 })
        } catch {
            connectionSettingsQr = ''
        }
    }

    async function createConnectionSettings(connection: ExternalConnectionSummary): Promise<void> {
        busy = true
        activeAction = `settings:${connection.id}`
        try {
            await displayConnectionSettings(await bridge.beginConnectionSettingsExport(connection.id))
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
            busy = false
            activeAction = ''
        }
    }

    async function saveConnectionSettingsFile(): Promise<void> {
        if (!connectionSettings) return
        try {
            await bridge.saveConnectionSettingsFile(connectionSettings.transferId)
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        }
    }

    function closeRecoveryKey(): void {
        recoveryKey = ''
    }

    function closeConnectionSettings(): void {
        connectionSettings = null
        connectionSettingsQr = ''
    }

    async function exportSnapshot(connectionId: string, snapshotId: string): Promise<void> {
        if (busy || exportRun) return
        const id = crypto.randomUUID()
        exportRun = { id, connectionId }
        busy = true
        try {
            await runWithMobileBackgroundTask('export', async background => {
                const abort = () => { void bridge.cancelExport(id).catch(() => {}) }
                background.signal?.addEventListener('abort', abort, { once: true })
                try {
                    background.signal?.throwIfAborted()
                    const result = await bridge.exportSnapshot(connectionId, snapshotId, id, progress => {
                        if (exportRun?.id === id) exportRun = { id, connectionId, progress }
                        background.progress(measuredTaskPercent(Number(progress.completedBytes), Number(progress.totalBytes)))
                    })
                    return result.cancelled ? null : result
                } finally { background.signal?.removeEventListener('abort', abort) }
            }, undefined, true)
        } catch (reason) {
            if (externalErrorKind(reason) !== 'cancelled') error = externalErrorMessage(strings, reason)
        } finally {
            if (exportRun?.id === id) exportRun = null
            busy = false
        }
    }

    function bytes(value?: string): string {
        return value ? formatRisuNestStorageBytes(Number(value)) : '—'
    }

    function jobLabel(job: ExternalJobSummary): string {
        if (job.state === 'succeeded') return strings.completed
        if (job.state === 'failed') return errorLabel(job.error)
        if (job.state === 'uncertain') return strings.uncertain
        if (externalJobIsPaused(job)) return errorLabel(job.error)
        if (job.state === 'waiting') return strings.waiting
        if (job.state === 'queued') return strings.queued
        if (job.state === 'running') return strings.running
        if (job.state === 'conflict') return strings.resolveRequired
        if (job.stoppedByApp) return errorLabel(job.error)
        return strings.cancelled
    }

    function errorLabel(value?: ExternalJobSummary['error']): string {
        if (!value) return strings.failed
        if (value.action === 'reauthenticate') return strings.reauthenticate
        if (value.action === 'unlock-key') return strings.unlockKey
        return externalErrorMessage(strings, value)
    }

    /** A connection whose automatic backup is off reports `paused`, and it runs every other job. */
    function connectionUsable(connection: ExternalConnectionSummary): boolean {
        return connection.status === 'ready' || connection.status === 'paused'
    }

    function connectionStatusLabel(status: ExternalConnectionSummary['status']): string {
        if (status === 'ready' || status === 'paused') return strings.statusReady
        if (status === 'reauth-required') return strings.statusReauth
        if (status === 'key-locked') return strings.statusLocked
        return strings.statusError
    }

    function activeJob(connection: ExternalConnectionSummary): ExternalJobSummary | undefined {
        return storageState?.jobs.find(job => job.connectionId === connection.id)
    }

    function unfinishedRestore(job: ExternalJobSummary | undefined): boolean {
        return job?.kind === 'restore' && job.state === 'uncertain'
    }

    // An unfinished restore reports its own error through the connection, which
    // its message replaces.
    function restoreReportsError(connection: ExternalConnectionSummary, job: ExternalJobSummary | undefined): boolean {
        return unfinishedRestore(job) && connection.lastError?.code === job?.error?.code
            && connection.lastError?.reason === job?.error?.reason
    }

    function connectionTone(connection: ExternalConnectionSummary): 'connected' | 'working' | 'attention' {
        const job = activeJob(connection)
        if (job && (externalJobIsPaused(job) || job.state === 'uncertain')) return 'attention'
        if (job && externalJobIsActive(job)) return 'working'
        if (connectionUsable(connection)) return 'connected'
        return 'attention'
    }

    // The notice under the heading explains a paused job, so the badge names only the state.
    function connectionStatus(connection: ExternalConnectionSummary): string {
        const job = activeJob(connection)
        if (job?.state === 'uncertain') return strings.statusError
        if (job && externalJobIsPaused(job)) return ['reauth-required', 'key-locked'].includes(connection.status) ? connectionStatusLabel(connection.status) : strings.statusError
        if (job && externalJobIsActive(job)) return strings.jobActive[job.kind]
        return connectionStatusLabel(connection.status)
    }

    // Restore phases after the download apply data locally, so their progress has no byte fraction.
    const RESTORE_APPLY_PHASES = new Set(['preparing-local', 'applying-local', 'awaiting-adoption'])

    function activeJobLabel(job: ExternalJobSummary): string {
        return (job.kind === 'restore' && strings.restorePhases[job.phase]) || strings.jobActive[job.kind]
    }

    function activeJobFraction(job: ExternalJobSummary): number | null {
        return job.kind === 'restore' && RESTORE_APPLY_PHASES.has(job.phase) ? null : externalJobProgress(job)
    }

    function jobSize(job: ExternalJobSummary): string {
        const size = `${bytes(job.completedBytes)}${job.totalBytes ? ` / ${bytes(job.totalBytes)}` : ''}`
        return job.counters ? `${strings.jobCounters[job.counters]} ${size}` : size
    }

    function jobSummary(job: ExternalJobSummary): string {
        if (job.kind === 'check-repository' && job.state === 'succeeded' && job.result?.stopReason === 'expired') return strings.checkExpired
        if (job.kind === 'check-repository' && job.state === 'succeeded' && job.result?.verifiedObjects !== undefined && job.result.verifiedBytes !== undefined) {
            const summary = strings.checkSummary.replace('{0}', job.result.verifiedObjects).replace('{1}', bytes(job.result.verifiedBytes))
            const damaged = Number(job.result.damagedObjects ?? '0')
            return damaged > 0 ? `${summary} ${strings.checkDamaged.replace('{0}', String(damaged))}` : summary
        }
        if (job.kind === 'cleanup' && job.state === 'succeeded' && job.result?.deletedObjects !== undefined && job.result.deletedBytes !== undefined) {
            const summary = strings.cleanupSummary.replace('{0}', job.result.deletedObjects).replace('{1}', bytes(job.result.deletedBytes))
            return job.result.stopReason === 'complete' ? summary : `${summary} ${strings.cleanupPartial}`
        }
        const progress = externalJobProgress(job)
        const size = jobSize(job)
        const state = externalJobIsActive(job) ? strings.jobActive[job.kind] : `${strings.jobKinds[job.kind]} · ${jobLabel(job)}`
        return progress === null ? `${state} · ${size}` : `${state} · ${size} (${Math.round(progress * 100)}%)`
    }


    function when(value?: string): string {
        return value ? new Date(Number(value)).toLocaleString() : '—'
    }

    function connectionPlace(connection: ExternalConnectionSummary): string {
        return [connection.endpoint.authority, connection.endpoint.repositoryHint].join(' · ')
    }

    function providerIcon(providerId: ExternalConnectionSummary['providerId']) {
        if (providerId === 'webdav') return ServerIcon
        if (providerId === 's3') return DatabaseIcon
        if (providerId === 'github_releases' || providerId === 'gitlab_packages') return PackageIcon
        return CloudIcon
    }

    function atLeast(value: string): string {
        return strings.atLeast.replace('{0}', bytes(value))
    }

    $effect(() => {
        if (recoveryKey) recoveryPanel?.focus()
    })
    $effect(() => {
        if (connectionSettings) connectionSettingsPanel?.focus()
    })

    onMount(async () => {
        stopSyncFailures = subscribeExternalLwwFailures(value => syncFailures = value)
        stopBindingChanges = subscribeSyncBindingChanges(() => { if (!destroyed) void refresh(true) })
        try {
            const stop = await bridge.onJobStarted(() => {
                if (destroyed) return
                void refresh(true).then(() => { if (!destroyed) schedulePoll() })
            })
            if (destroyed) { stop(); return }
            stopJobEvents = stop
        } catch (reason) { error = externalErrorMessage(strings, reason) }
        if (destroyed) return
        await refresh()
        schedulePoll()
    })
    onDestroy(() => {
        destroyed = true
        removalDownload?.controller.abort()
        clearTimeout(pollTimer)
        stopJobEvents?.()
        stopSyncFailures?.()
        stopBindingChanges?.()
        if (exportRun) void bridge.cancelExport(exportRun.id).catch(() => {})
    })
</script>


<svelte:window onkeydown={event => { if (event.key === 'Escape') { closeRecoveryKey(); closeConnectionSettings() } }} />

<SettingGroup id="risunest-external-storage" title={strings.title} description={strings.help}>
    {#snippet actions()}
        {#if storageState?.supported && !adding && !renewalConnection}<SettingButton disabled={busy} onclick={() => adding = true}>{strings.add}</SettingButton>{/if}
        {#if storageState?.supported !== false}<SettingButton variant="secondary" busy={activeAction === 'refresh'} disabled={busy} onclick={refreshWithHistory}>{strings.refresh}</SettingButton>{/if}
    {/snippet}

    {#if !storageState && busy}<p class="placeholder">{strings.loading}</p>
    {:else if storageState && !storageState.supported}<p class="placeholder">{strings.unsupported}</p>
    {:else if adding || renewalConnection}
        <ConnectionForm {renewalConnection} {strings} onconnected={onConnected} oncancel={() => { adding = false; renewalConnection = undefined }} onbusychange={value => busy = value} />
    {:else if storageState}
        {#if !storageState.connections.length}
            <div class="blank">
                <span class="blank-icon" aria-hidden="true"><CloudIcon size={20} /></span>
                <p>{strings.noConnections}</p>
            </div>
        {/if}
        {#each storageState.connections as connection (connection.id)}
            {@const job = activeJob(connection)}
            {@const ProviderIcon = providerIcon(connection.providerId)}
            {@const renewable = connection.status === 'reauth-required' || job?.error?.action === 'reauthenticate'}
            {@const lockable = connection.status === 'key-locked' || job?.error?.action === 'unlock-key'}
            {@const recheckable = job?.state === 'uncertain' && job.kind === 'backup'}
            {@const retryable = !!job && externalJobIsPaused(job) && ['retry', 'wait', 'free-space'].includes(job.error?.action ?? '') && ['backup', 'cleanup', 'restore', 'check-repository', 'pin-history', 'delete-history'].includes(job.kind)}
            {@const remedy = renewable || lockable || recheckable || retryable}
            {@const syncTarget = storageState.selection.kind === 'external' && storageState.selection.connectionId === connection.id}
            {@const backedUp = connection.purpose === 'backup' || !!connection.lastBackupAtMs}
            {@const synced = connection.purpose === 'sync' && !!connection.lastSyncAtMs}
            <article class="card">
                <header class="card-head">
                    <span class="provider" aria-hidden="true"><ProviderIcon size={18} /></span>
                    <div class="card-name">
                        <h3 class="card-title">{externalConnectionTitle(strings, connection)}</h3>
                        <p class="card-sub">{connectionPlace(connection)}</p>
                    </div>
                    <div class="card-status"><StatusBadge label={connectionStatus(connection)} tone={connectionTone(connection)} /></div>
                </header>

                {#if connection.lastError && !restoreReportsError(connection, job)}<SettingNotice text={errorLabel(connection.lastError)} />{/if}
                {#if syncFailures.has(connection.id)}
                    {@const failure = syncFailures.get(connection.id)}
                    <SettingNotice role="status" text={externalErrorKind(failure) === 'clockSkew' ? language.lwwSync.clockBlocked
                        : externalErrorKind(failure) === 'previousStorageUnavailable' ? language.lwwSync.previousStorageUnavailable
                        : externalErrorMessage(strings, failure)} />
                    {#if ['clockSkew', 'corrupt'].includes(externalErrorKind(failure) ?? '') && supportsExternalLwwNewDevice(connection.id)}
                        <div class="actions">
                            <BindingTargetSwitch target={{kind:'external',connectionId:connection.id}} options={{mode:'new-device'}} label={language.lwwSync.newDeviceAction} onBound={() => refreshExternalStorageProductionState().then(() => refresh())} onError={reason => error = externalErrorMessage(strings, reason)} />
                        </div>
                    {/if}
                {/if}
                {#if job && (!externalJobIsActive(job) || externalJobIsPaused(job)) && job.state !== 'succeeded' && !connection.lastError && !unfinishedRestore(job)}<SettingNotice role="status" tone={job.state === 'cancelled' && !job.stoppedByApp ? 'info' : 'danger'} text={jobLabel(job)} />{/if}

                {#if unfinishedRestore(job)}
                    <SettingNotice role="status" text={strings.restoreUnfinished} />
                {:else if job?.state === 'uncertain'}
                    <SettingNotice role="status" text={strings.publicationDecision} />
                {/if}

                {#if exportRun?.connectionId === connection.id}
                    {@const amount = exportRun.progress}
                    <div class="running">
                        <SettingProgress label={strings.download} detail={amount ? `${bytes(amount.completedBytes)}${amount.totalBytes ? ` / ${bytes(amount.totalBytes)}` : ''}` : ''} fraction={amount?.totalBytes && Number(amount.totalBytes) > 0 ? Number(amount.completedBytes) / Number(amount.totalBytes) : null} />
                        <div class="actions"><SettingButton variant="secondary" size="sm" onclick={() => exportRun && bridge.cancelExport(exportRun.id)}>{strings.cancel}</SettingButton></div>
                    </div>
                {/if}
                {#if backedUp || synced || job?.state === 'succeeded'}
                    <dl class="kv">
                        {#if synced}<dt>{language.risuNest.serverSync.lastSuccess}</dt><dd>{when(connection.lastSyncAtMs)}</dd>{/if}
                        {#if backedUp}<dt>{strings.lastBackup}</dt><dd>{when(connection.lastBackupAtMs)}</dd>{/if}
                        {#if job && job.state === 'succeeded'}<dt>{strings.progress}</dt><dd role="status" aria-live="polite">{jobSummary(job)}</dd>{/if}
                    </dl>
                {/if}
                {#if job && externalJobIsPaused(job) && job.error?.retryAtMs}<p class="note">{strings.retryAt.replace('{0}', when(job.error.retryAtMs))}</p>{/if}
                {#if job && externalJobIsActive(job) && !externalJobIsPaused(job)}
                    <SettingProgress label={activeJobLabel(job)} detail={jobSize(job)} fraction={activeJobFraction(job)} />
                {/if}

                {#if unlockConnection?.id === connection.id}
                    <div class="unlock">
                        <label class="unlock-field"><span>{strings.recoveryCode}</span><TextInput hideText bind:value={unlockKey} /></label>
                        <div class="actions">
                            <SettingButton disabled={busy || !unlockKey.trim()} onclick={unlock}>{strings.unlock}</SettingButton>
                            <SettingButton variant="secondary" disabled={busy} onclick={() => { unlockConnection = undefined; unlockKey = '' }}>{strings.cancel}</SettingButton>
                        </div>
                    </div>
                {/if}

                <div class="controls">
                    {#if connection.purpose === 'backup'}
                        <SettingToggle showLabel label={strings.automaticBackup} disabled={busy} checked={automaticRequest[connection.id] ?? !connection.automaticBackupPaused} onchange={enabled => setAutomaticWork(connection, enabled)} />
                    {:else}
                        <SettingToggle showLabel label={strings.makeSyncTarget} disabled={busy} checked={syncRequest[connection.id] ?? syncTarget} onchange={enabled => setSyncBinding(connection, enabled)} />
                    {/if}
                    <div class="actions">
                        {#if renewable}<SettingButton disabled={busy} onclick={() => renewalConnection = connection}>{strings.renew}</SettingButton>{/if}
                        {#if lockable}<SettingButton disabled={busy} onclick={() => unlockConnection = connection}>{strings.unlock}</SettingButton>{/if}
                        {#if recheckable && job}<SettingButton disabled={busy} onclick={() => resumeJob(connection, job)}>{strings.recheckPublication}</SettingButton>{/if}
                        {#if retryable && job}<SettingButton disabled={busy || Number(job.error?.retryAtMs ?? 0) > Date.now()} onclick={() => resumeJob(connection, job)}>{strings.retryAction}</SettingButton>{/if}
                        {#if connection.purpose === 'backup'}<SettingButton variant={remedy ? 'secondary' : 'primary'} busy={activeAction === `backup:${connection.id}`} disabled={busy || (job && externalJobIsActive(job))} onclick={() => runJob(connection, 'backup')}>{strings.runBackup}</SettingButton>
                        {:else if syncTarget}<SettingButton variant={remedy ? 'secondary' : 'primary'} disabled={busy} onclick={() => syncNow(connection)}>{strings.runSync}</SettingButton>{/if}
                        {#if job && externalJobIsActive(job)}<SettingButton variant="secondary" onclick={() => cancelJob(job)}>{strings.cancel}</SettingButton>{/if}
                        {#if job && unfinishedRestore(job)}<SettingButton variant="secondary" disabled={busy} onclick={() => stopRestore(job)}>{strings.stopRestore}</SettingButton>{/if}
                    </div>
                </div>

                <div class="details">
                    <div class="tabs" role="tablist">
                        {#each (['history', 'quota'] as const) as tab (tab)}
                            <button type="button" role="tab" id="{connection.id}-{tab}-tab" aria-controls="{connection.id}-{tab}-panel" aria-selected={selectedTab(connection.id) === tab} class="tab" onclick={() => openDetails(connection, tab)}>{tab === 'history' ? strings.history : strings.quota}</button>
                        {/each}
                    </div>

                    {#if selectedTab(connection.id) === 'history'}
                        {@const items = history[connection.id] ?? []}
                        <div class="panel" role="tabpanel" id="{connection.id}-history-panel" aria-labelledby="{connection.id}-history-tab">
                            {#if historyLoading[connection.id]}<p class="empty">{strings.loading}</p>
                            {:else if historyError[connection.id]}<SettingNotice role="status" text={historyError[connection.id]} />
                            {:else if items.length === 0}<p class="empty">{strings.noHistory}</p>{/if}
                            {#if items.length > 0}
                                <ul class="rows">
                                    {#each items as item (item.id)}
                                        {@const usable = item.complete && item.verified}
                                        <li class="item">
                                            <div class="item-head">
                                                <span class="kind" data-kind={item.kind}>{strings.historyKinds[item.kind]}</span>
                                                <span class="item-time">{when(item.createdAtMs)}</span>
                                                {#if item.pinned}<span class="kept"><PinIcon size={12} aria-hidden="true" />{strings.pinned}</span>{/if}
                                            </div>
                                            {#if item.sameDevice}<p class="item-meta">{strings.thisDevice}</p>{/if}
                                            <div class="item-actions">
                                                <SettingButton size="sm" variant="secondary" disabled={busy || (job && externalJobIsActive(job)) || !usable} onclick={() => beginRestore(connection, item)}>{strings.restore}</SettingButton>
                                                <SettingButton size="sm" variant="quiet" disabled={busy || (job && externalJobIsActive(job)) || !usable} onclick={() => exportSnapshot(connection.id, item.snapshotId)}>{strings.download}</SettingButton>
                                                <SettingButton size="sm" variant="quiet" disabled={busy || !usable} onclick={() => runJob(connection, 'check-repository', { snapshotId: item.snapshotId })}>{strings.check}</SettingButton>
                                                {#if !item.pinned}<SettingButton size="sm" variant="quiet" disabled={busy || (job && externalJobIsActive(job))} onclick={() => runJob(connection, 'pin-history', { snapshotId: item.snapshotId })}>{strings.pin}</SettingButton>{/if}
                                                {#if item.deletable}<SettingButton size="sm" variant="danger" class="ml-auto" disabled={busy} onclick={() => deleteHistory(connection, item)}>{strings.deleteHistory}</SettingButton>{/if}
                                            </div>
                                        </li>
                                    {/each}
                                </ul>
                            {/if}
                            {#if historyCursor[connection.id]}<div class="more"><SettingButton variant="secondary" size="sm" busy={historyLoading[connection.id]} onclick={() => loadHistory(connection, true)}>{strings.loadMore}</SettingButton></div>{/if}
                        </div>
                    {:else}
                        {@const usage = quota[connection.id]}
                        {@const retention = connection.retentionPolicy}
                        <div class="panel usage" role="tabpanel" id="{connection.id}-quota-panel" aria-labelledby="{connection.id}-quota-tab">
                            {#if usage || remoteOnly[connection.id] !== undefined}
                                <dl class="stats">
                                    {#if usage}
                                        {#if usage.storage.providerPhysicalKnown && usage.storage.providerPhysicalBytes !== null}
                                            <div class="stat">
                                                <dt>{strings.usedByService}</dt>
                                                <dd class="stat-value">{bytes(usage.storage.providerPhysicalBytes)}</dd>
                                            </div>
                                        {/if}
                                        <div class="stat">
                                            <dt>{strings.uploadedLowerBound}</dt>
                                            <dd class="stat-value">{bytes(usage.storage.locallyUploadedBytesLowerBound)}</dd>
                                            <dd class="stat-note">{strings.files.replace('{0}', Number(usage.storage.locallyUploadedObjectCountLowerBound).toLocaleString())}</dd>
                                        </div>
                                        {#if usage.storage.latestReachable}
                                            <div class="stat">
                                                <dt>{strings.latestReachable}</dt>
                                                <dd class="stat-value">{atLeast(usage.storage.latestReachable.knownDirectBytes)}</dd>
                                            </div>
                                        {/if}
                                    {/if}
                                    {#if remoteOnly[connection.id] !== undefined}
                                        {@const count = remoteOnly[connection.id]}
                                        <div class="stat">
                                            <dt>{strings.remoteOnlyFiles}</dt>
                                            <dd class="stat-value" data-unknown={count === null}>{count === null ? strings.unknownUsage : count.toLocaleString()}</dd>
                                        </div>
                                    {/if}
                                </dl>
                            {/if}
                            {#if usage && usage.buckets.length > 0}
                                <section class="usage-section">
                                    <h4 class="section-title">{strings.requestUsage}</h4>
                                    <ul class="buckets">
                                        {#each usage.buckets as bucket (bucket.id)}
                                            {@const share = Number(bucket.limit) > 0 ? Math.min(1, Number(bucket.used) / Number(bucket.limit)) : 0}
                                            <li class="bucket">
                                                <div class="bucket-line">
                                                    <span class="bucket-name">{strings.quotaBuckets[bucket.id] ?? bucket.id}</span>
                                                    <span class="bucket-count">{strings.requestCount.replace('{0}', Number(bucket.used).toLocaleString()).replace('{1}', Number(bucket.limit).toLocaleString())}</span>
                                                </div>
                                                <div class="meter" data-high={share >= 0.9} aria-hidden="true"><span style:width="{share * 100}%"></span></div>
                                            </li>
                                        {/each}
                                    </ul>
                                </section>
                            {/if}
                            <section class="usage-section">
                                <div class="fields">
                                    <label class="field">
                                        <span>{strings.retentionCount}</span>
                                        <span class="amount">
                                            <NumberInput size="sm" className="w-20 text-right tabular-nums" disabled={busy} min={RETENTION_LIMITS.keepCount[0]} max={RETENTION_LIMITS.keepCount[1]} value={retention.keepCount} onChange={event => commitRetentionLimit(connection, 'keepCount', event.currentTarget)} />
                                        </span>
                                    </label>
                                    <label class="field">
                                        <span>{strings.retentionDays}</span>
                                        <span class="amount">
                                            <NumberInput size="sm" className="w-20 text-right tabular-nums" disabled={busy} min={RETENTION_LIMITS.keepDays[0]} max={RETENTION_LIMITS.keepDays[1]} value={retention.keepDays} onChange={event => commitRetentionLimit(connection, 'keepDays', event.currentTarget)} />
                                            <span class="unit">{strings.retentionDaysUnit}</span>
                                        </span>
                                    </label>
                                </div>
                                <p class="help">{strings.retentionHelp}</p>
                            </section>
                            <section class="usage-section">
                                {#if connection.capabilities.snapshotDiscovery && connection.capabilities.leaseOperations && connection.capabilities.deleteObjects}
                                    <div class="actions">
                                        <SettingButton variant="secondary" busy={activeAction === `cleanup:${connection.id}`} disabled={busy || !connectionUsable(connection) || storageState.jobs.some(job => job.connectionId === connection.id && externalJobIsActive(job))} onclick={() => runJob(connection, 'cleanup')}>{strings.cleanup}</SettingButton>
                                    </div>
                                {/if}
                                <p class="help">{strings.cleanupTrashNotice} {strings.providerCapacityHelp}</p>
                            </section>
                        </div>
                    {/if}
                </div>

                <footer class="foot">
                    <SettingButton variant="secondary" busy={activeAction === `settings:${connection.id}`} disabled={busy} onclick={() => createConnectionSettings(connection)}>{strings.connectionSettings}</SettingButton>
                    <span class="foot-end">
                        {#if removalDownload?.connectionId === connection.id}
                            <SettingButton variant="secondary" disabled={removalDownload.cancelling} onclick={() => { if (removalDownload) { removalDownload.cancelling = true; removalDownload.controller.abort() } }}>{strings.cancel}</SettingButton>
                        {/if}
                        <SettingButton variant="danger" busy={activeAction === `remove:${connection.id}`} disabled={busy} onclick={() => removeConnection(connection)}>{strings.remove}</SettingButton>
                    </span>
                </footer>
            </article>
        {/each}
    {/if}
    {#if error || pollError}<div class="group-alert"><SettingNotice role="alert" text={error || pollError} /></div>{/if}
</SettingGroup>

{#if recoveryKey}
    <div class="fixed inset-0 z-[1000] flex items-center justify-center bg-black/60 p-4">
        <div
            bind:this={recoveryPanel}
            tabindex="-1"
            role="dialog"
            aria-modal="true"
            aria-labelledby="external-recovery-title"
            class="dialog outline-hidden"
        >
            <h3 id="external-recovery-title" class="text-lg font-bold">{strings.recovery}</h3>
            <p class="text-sm text-textcolor2">{strings.recoveryNotice}</p>
            <label class="block text-sm">
                <span class="font-medium">{strings.recoveryCode}</span>
                <input readonly class="mt-1 w-full select-all rounded-md border border-darkborderc bg-darkbg p-2 font-mono shadow-xs" value={recoveryKey} />
                <span class="mt-1 block text-[13px] text-textcolor2">{strings.recoveryCodeHelp}</span>
            </label>
            <div class="actions"><SettingButton onclick={closeRecoveryKey}>{strings.closeRecovery}</SettingButton></div>
        </div>
    </div>
{/if}

{#if connectionSettings}
    <div class="fixed inset-0 z-[1000] flex items-center justify-center bg-black/60 p-4">
        <div
            bind:this={connectionSettingsPanel}
            tabindex="-1"
            role="dialog"
            aria-modal="true"
            aria-labelledby="external-connection-settings-title"
            class="dialog outline-hidden"
        >
            <h3 id="external-connection-settings-title" class="text-lg font-bold">{strings.connectionSettings}</h3>
            <p class="text-sm text-textcolor2">{strings.connectionSettingsNotice}</p>
            {#if connectionSettingsQr}<img class="mx-auto rounded-md bg-white p-2" src={connectionSettingsQr} alt={strings.connectionSettingsQr} />
            {:else}<p class="rounded-md border border-darkborderc bg-darkbg p-3 text-sm">{strings.connectionSettingsFileOnly}</p>{/if}
            <div class="actions"><SettingButton onclick={saveConnectionSettingsFile}>{strings.saveConnectionSettings}</SettingButton><SettingButton variant="secondary" onclick={closeConnectionSettings}>{strings.closeRecovery}</SettingButton></div>
        </div>
    </div>
{/if}

<style>
    .placeholder {
        padding: 1rem;
        font-size: 14px;
        color: var(--risu-theme-textcolor2);
    }
    .blank {
        display: grid;
        justify-items: center;
        gap: 0.625rem;
        padding: 1.75rem 1.25rem;
        text-align: center;
    }
    .blank-icon {
        display: grid;
        place-items: center;
        width: 2.75rem;
        height: 2.75rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 50%;
        background: var(--risu-theme-bgcolor);
        color: var(--risu-theme-textcolor2);
    }
    .blank p {
        max-width: 46ch;
        margin: 0;
        font-size: 13px;
        line-height: 1.55;
        color: var(--risu-theme-textcolor2);
    }
    .card {
        display: grid;
        gap: 0.875rem;
        min-width: 0;
        padding: 1rem;
    }
    .card-head {
        display: grid;
        grid-template-columns: auto minmax(0, 1fr);
        align-items: center;
        gap: 0.5rem 0.75rem;
    }
    .card-status {
        grid-column: 2;
        min-width: 0;
    }
    @container (min-width: 36rem) {
        .card-head {
            grid-template-columns: auto minmax(0, 1fr) auto;
        }
        .card-status {
            grid-column: auto;
            justify-self: end;
            max-width: 16rem;
        }
    }
    .provider {
        display: grid;
        place-items: center;
        width: 2.25rem;
        height: 2.25rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 0.5rem;
        background: var(--risu-theme-bgcolor);
        color: var(--risu-theme-textcolor2);
    }
    .card-name {
        min-width: 0;
    }
    .card-title {
        margin: 0;
        font-size: 15px;
        font-weight: 600;
        line-height: 1.35;
        overflow-wrap: anywhere;
    }
    .card-sub {
        margin: 0.125rem 0 0;
        font-size: 12.5px;
        line-height: 1.4;
        color: var(--risu-theme-textcolor2);
        overflow-wrap: anywhere;
    }
    .kv {
        display: grid;
        grid-template-columns: auto minmax(0, 1fr);
        gap: 0.375rem 1.25rem;
        margin: 0;
        font-size: 13px;
        line-height: 1.45;
    }
    .kv dt {
        color: var(--risu-theme-textcolor2);
        white-space: nowrap;
    }
    .kv dd {
        margin: 0;
        min-width: 0;
        overflow-wrap: anywhere;
        font-variant-numeric: tabular-nums;
    }
    .note {
        margin: 0;
        font-size: 13px;
        color: var(--risu-theme-textcolor2);
    }
    .running {
        display: grid;
        gap: 0.5rem;
    }
    .unlock {
        display: grid;
        gap: 0.625rem;
        padding: 0.75rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 0.5rem;
        background: var(--risu-theme-bgcolor);
    }
    .unlock-field {
        display: grid;
        gap: 0.25rem;
        font-size: 13px;
    }
    .controls {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        justify-content: space-between;
        gap: 0.75rem 1rem;
    }
    .actions {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.5rem;
    }
    .details {
        min-width: 0;
        overflow: hidden;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 0.5rem;
        background: var(--risu-theme-bgcolor);
    }
    .tabs {
        display: flex;
        gap: 0.25rem;
        padding: 0 0.5rem;
        border-bottom: 1px solid var(--risu-theme-darkborderc);
    }
    .tabs:only-child {
        border-bottom: 0;
    }
    .tab {
        margin-bottom: -1px;
        padding: 0.55rem 0.625rem;
        border: 0;
        border-bottom: 2px solid transparent;
        background: transparent;
        font-size: 13px;
        color: var(--risu-theme-textcolor2);
        cursor: pointer;
    }
    .tabs:only-child .tab {
        margin-bottom: 0;
    }
    .tab:hover {
        color: var(--risu-theme-textcolor);
    }
    .tab[aria-selected='true'] {
        border-bottom-color: var(--risu-theme-primary-500);
        color: var(--risu-theme-textcolor);
        font-weight: 600;
    }
    .tab:focus-visible {
        outline: 2px solid var(--risu-theme-selected);
        outline-offset: -2px;
    }
    .panel {
        display: grid;
        gap: 0.75rem;
        min-width: 0;
        padding: 0.875rem;
    }
    .empty {
        margin: 0;
        font-size: 13px;
        color: var(--risu-theme-textcolor2);
    }
    .rows {
        display: grid;
        margin: 0;
        padding: 0;
        list-style: none;
    }
    .item {
        display: grid;
        gap: 0.375rem;
        min-width: 0;
        padding: 0.75rem 0;
    }
    .item:first-child {
        padding-top: 0;
    }
    .item:last-child {
        padding-bottom: 0;
    }
    .item + .item {
        border-top: 1px solid var(--risu-theme-darkborderc);
    }
    .item-head {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.375rem 0.5rem;
    }
    .kind {
        padding: 0.1rem 0.5rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 99px;
        background: var(--risu-theme-darkbg);
        font-size: 11.5px;
        font-weight: 600;
        line-height: 1.5;
        color: var(--risu-theme-textcolor2);
    }
    .kind[data-kind='snapshot'] {
        border-color: color-mix(in srgb, var(--risu-theme-primary-500) 45%, transparent);
        background: color-mix(in srgb, var(--risu-theme-primary-500) 12%, var(--risu-theme-bgcolor));
        color: var(--risu-theme-textcolor);
    }
    .kind[data-kind='conflict'] {
        border-color: color-mix(in srgb, var(--risu-theme-danger-400) 50%, transparent);
        background: color-mix(in srgb, var(--risu-theme-danger-400) 10%, var(--risu-theme-bgcolor));
        color: color-mix(in srgb, var(--risu-theme-danger-400) 65%, var(--risu-theme-textcolor));
    }
    .kind[data-kind='recovery-candidate'] {
        border-color: color-mix(in srgb, var(--risu-theme-secondary-500) 45%, transparent);
        background: color-mix(in srgb, var(--risu-theme-secondary-500) 12%, var(--risu-theme-bgcolor));
        color: var(--risu-theme-textcolor);
    }
    .item-time {
        font-size: 14px;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
    }
    .kept {
        display: inline-flex;
        align-items: center;
        gap: 0.25rem;
        font-size: 12px;
        color: var(--risu-theme-textcolor2);
    }
    .item-meta {
        margin: 0;
        font-size: 12.5px;
        color: var(--risu-theme-textcolor2);
        overflow-wrap: anywhere;
    }
    .item-actions {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.25rem 0.375rem;
        margin-top: 0.125rem;
    }
    .more {
        display: flex;
        justify-content: center;
        padding-top: 0.75rem;
        border-top: 1px solid var(--risu-theme-darkborderc);
    }
    .usage {
        gap: 1rem;
    }
    .stats {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(min(100%, 10rem), 1fr));
        gap: 0.5rem;
        margin: 0;
    }
    .stat {
        display: grid;
        align-content: start;
        gap: 0.125rem;
        min-width: 0;
        padding: 0.625rem 0.75rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 0.5rem;
        background: var(--risu-theme-darkbg);
    }
    .stat dt {
        font-size: 13px;
        line-height: 1.4;
        color: var(--risu-theme-textcolor2);
    }
    .stat dd {
        margin: 0;
        min-width: 0;
        overflow-wrap: anywhere;
    }
    .stat-value {
        font-size: 16px;
        font-weight: 600;
        line-height: 1.35;
        font-variant-numeric: tabular-nums;
    }
    .stat-value[data-unknown='true'] {
        font-weight: 500;
        color: color-mix(in srgb, var(--risu-theme-textcolor2) 62%, var(--risu-theme-textcolor) 38%);
    }
    .stat-note {
        font-size: 13px;
        line-height: 1.45;
        color: color-mix(in srgb, var(--risu-theme-textcolor2) 62%, var(--risu-theme-textcolor) 38%);
    }
    .usage-section {
        display: grid;
        gap: 0.625rem;
        min-width: 0;
        padding-top: 1rem;
        border-top: 1px solid var(--risu-theme-darkborderc);
    }
    .usage-section:first-child {
        padding-top: 0;
        border-top: 0;
    }
    .section-title {
        margin: 0;
        font-size: 13px;
        font-weight: 600;
    }
    .buckets {
        display: grid;
        gap: 0.625rem;
        margin: 0;
        padding: 0;
        list-style: none;
    }
    .bucket {
        display: grid;
        gap: 0.3rem;
        min-width: 0;
    }
    .bucket-line {
        display: flex;
        align-items: baseline;
        justify-content: space-between;
        gap: 0.75rem;
        font-size: 13px;
    }
    .bucket-name {
        min-width: 0;
    }
    .bucket-count {
        color: var(--risu-theme-textcolor2);
        font-variant-numeric: tabular-nums;
        white-space: nowrap;
    }
    .meter {
        height: 4px;
        overflow: hidden;
        border-radius: 99px;
        background: var(--risu-theme-darkbutton);
    }
    .meter span {
        display: block;
        height: 100%;
        border-radius: inherit;
        background: var(--risu-theme-primary-500);
    }
    .meter[data-high='true'] span {
        background: var(--risu-theme-danger-400);
    }
    .fields {
        display: grid;
        gap: 0.5rem;
    }
    .field {
        display: grid;
        grid-template-columns: minmax(8rem, max-content) auto;
        justify-content: start;
        align-items: center;
        gap: 0.75rem;
        font-size: 14px;
    }
    .amount {
        display: inline-flex;
        align-items: center;
        gap: 0.4rem;
    }
    .unit {
        font-size: 13px;
        color: var(--risu-theme-textcolor2);
    }
    .help {
        max-width: 62ch;
        margin: 0;
        font-size: 13px;
        line-height: 1.5;
        color: color-mix(in srgb, var(--risu-theme-textcolor2) 62%, var(--risu-theme-textcolor) 38%);
    }
    .foot {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.5rem;
        padding-top: 0.875rem;
        border-top: 1px solid color-mix(in srgb, var(--risu-theme-darkborderc) 70%, transparent);
    }
    .foot-end {
        display: flex;
        flex-wrap: wrap;
        gap: 0.5rem;
        margin-left: auto;
    }
    .group-alert {
        padding: 0.75rem 1rem;
    }
    .dialog {
        display: grid;
        gap: 1rem;
        max-height: 90vh;
        max-width: 28rem;
        overflow-y: auto;
        padding: 1.25rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 0.5rem;
        background: var(--risu-theme-bgcolor);
        color: var(--risu-theme-textcolor);
    }
</style>
