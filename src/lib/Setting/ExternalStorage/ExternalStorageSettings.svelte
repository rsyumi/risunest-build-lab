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
    import { alertConfirm, alertNormal, alertCheckboxConfirm } from 'src/ts/alert'
    import { DBState } from 'src/ts/stores.svelte'
    import { getExternalStorageBridge } from 'src/ts/storage/sync/external/bridge'
    import { bindSyncTarget, unbindSyncTarget } from 'src/ts/storage/sync/bindingRegistry'
    import { createNativeSyncBindingBridge } from 'src/ts/storage/sync/bindingNative'
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
    import ConnectionForm from './ConnectionForm.svelte'
    import { externalConnectionTitle, externalErrorKind, externalErrorMessage, externalStorageStrings } from './strings'

    const bridge = getExternalStorageBridge()
    /** How many empty history pages one request reads past before stopping. */
    const EMPTY_HISTORY_STEPS = 4
    /** What a repository accepts for each retention limit. */
    const RETENTION_LIMITS = { keepCount: [1, 1000], keepDays: [7, 3650] } as const
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
    let expanded = $state<Record<string, 'history' | 'quota' | ''>>({})
    let history = $state<Record<string, ExternalHistoryItem[]>>({})
    let historyCursor = $state<Record<string, string | undefined>>({})
    let historyLoading = $state<Record<string, boolean>>({})
    let quota = $state<Record<string, ExternalQuotaSummary>>({})
    let remoteOnly = $state<Record<string, number>>({})
    let recoveryKey = $state('')
    let recoveryPanel = $state<HTMLDivElement | undefined>()
    let connectionSettings = $state<ExternalConnectionSettingsMaterial | null>(null)
    let connectionSettingsPanel = $state<HTMLDivElement | undefined>()
    let connectionSettingsQr = $state('')
    /** The history entry whose restore scope is open, and what is ticked in it. */
    let exportRun = $state<{ id: string; connectionId: string; progress?: ExternalSnapshotExportProgress } | null>(null)
    const pendingPins = new Map<string, string>()
    let pollTimer: ReturnType<typeof setTimeout> | undefined

    async function setSyncBinding(connection: ExternalConnectionSummary, enabled: boolean): Promise<void> {
        busy = true
        try {
            if (enabled) await bindSyncTarget({ kind: 'external', connectionId: connection.id })
            else await unbindSyncTarget()
            await refreshExternalStorageProductionState()
            await refresh()
        } catch (reason) {
            error = externalErrorKind(reason) === PREVIOUS_FILES_DOWNLOAD_FAILED ? language.lwwSync.downloadFailedNotConnected : externalErrorMessage(strings, reason)
        }
        finally { busy = false }
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
            for (const [connectionId, jobId] of pendingPins) {
                const completed = storageState.jobs.find(job => job.id === jobId)
                if (!completed || externalJobIsActive(completed)) continue
                pendingPins.delete(connectionId)
                const connection = storageState.connections.find(item => item.id === connectionId)
                if (completed.state === 'succeeded' && connection) await loadHistory(connection, false)
            }
            if (!silent) error = ''
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
            if (!silent) {
                busy = false
                activeAction = ''
            }
        }
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
            return
        }
        // An already existing repository is opened to read what is in it, so
        // its contents are shown without asking for the history tab first.
        if (result.connection.mode === 'existing') {
            expanded[result.connection.id] = 'history'
            await loadHistory(result.connection, false)
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
                    else pendingPins.set(connection.id, started.id)
                }
            }
            await refresh(true)
            schedulePoll()
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
            busy = false
            activeAction = ''
        }
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
                    checkboxLabel: strings.deleteHistory,
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
        try {
            await bridge.setAutomaticBackupPaused(connection.id, !enabled)
            await refreshExternalStorageProductionState()
            await refresh(true)
        } catch (reason) { error = externalErrorMessage(strings, reason) }
        finally { busy = false }
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
            delete remoteOnly[connection.id]
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
            error = ''
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
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
        let held = 0
        try {
            held = await remoteOnlyFiles(connection)
        } catch {}
        let download = false
        if (held) {
            const choice = await alertCheckboxConfirm({
                title: strings.removeRemoteOnlyTitle,
                description: strings.removeRemoteOnly,
                checkboxLabel: strings.downloadThenRemove,
                actionLabel: strings.remove,
                cancelLabel: strings.cancel,
                requireChecked: false,
            })
            if (!choice.confirmed) return
            download = choice.checked
        } else if (!(await alertConfirm(`${strings.remove}: ${externalConnectionTitle(strings, connection)}`))) return
        busy = true
        activeAction = `remove:${connection.id}`
        try {
            if (download) {
                try {
                    await downloadRemoteAssets(connection.id)
                } catch (reason) {
                    if (externalErrorKind(reason) !== 'cancelled') error = strings.downloadFailedKeptConnection
                    return
                }
            }
            await bridge.removeConnection(connection.id)
            await refreshExternalStorageProductionState()
            await refresh(true)
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
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
        if (!value) return '—'
        const amount = BigInt(value)
        const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
        let divisor = 1n
        let index = 0
        while (index < units.length - 1 && amount >= divisor * 1024n) {
            divisor *= 1024n
            index += 1
        }
        if (index === 0) return `${amount} B`
        return `${Number((amount * 10n) / divisor) / 10} ${units[index]}`
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
        return strings.cancelled
    }

    function errorLabel(value?: ExternalJobSummary['error']): string {
        if (!value) return strings.failed
        if (value.action === 'reauthenticate') return strings.reauthenticate
        if (value.action === 'unlock-key') return strings.unlockKey
        return externalErrorMessage(strings, value)
    }

    function connectionStatusLabel(status: ExternalConnectionSummary['status']): string {
        if (status === 'ready') return strings.statusReady
        if (status === 'paused') return strings.statusPaused
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

    function connectionTone(connection: ExternalConnectionSummary): 'connected' | 'working' | 'paused' | 'attention' {
        const job = activeJob(connection)
        if (job && (externalJobIsPaused(job) || job.state === 'uncertain')) return 'attention'
        if (job && externalJobIsActive(job)) return 'working'
        if (connection.status === 'ready') return 'connected'
        if (connection.status === 'paused') return 'paused'
        return 'attention'
    }

    function connectionStatus(connection: ExternalConnectionSummary): string {
        const job = activeJob(connection)
        if (job?.state === 'uncertain') return strings.statusError
        if (job && externalJobIsPaused(job)) return errorLabel(job.error)
        if (job && externalJobIsActive(job)) return strings.jobActive[job.kind]
        return connectionStatusLabel(connection.status)
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

    function historyLine(item: ExternalHistoryItem): string {
        return [
            when(item.createdAtMs),
            strings.historyKinds[item.kind],
            item.deviceName,
            item.storedBytes ? bytes(item.storedBytes) : undefined,
        ].filter(Boolean).join(' · ')
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
        clearTimeout(pollTimer)
        stopJobEvents?.()
        stopSyncFailures?.()
        if (exportRun) void bridge.cancelExport(exportRun.id).catch(() => {})
    })
</script>


<svelte:window onkeydown={event => { if (event.key === 'Escape') { closeRecoveryKey(); closeConnectionSettings() } }} />

<SettingGroup id="risunest-external-storage" title={strings.title} description={strings.help}>
    {#snippet actions()}
        {#if storageState?.supported && !adding && !renewalConnection}<SettingButton disabled={busy} onclick={() => adding = true}>{strings.add}</SettingButton>{/if}
        {#if storageState?.supported !== false}<SettingButton variant="secondary" busy={activeAction === 'refresh'} disabled={busy} onclick={() => refresh()}>{strings.refresh}</SettingButton>{/if}
    {/snippet}

    {#if !storageState && busy}<p class="p-4 text-sm text-textcolor2">{strings.loading}</p>
    {:else if storageState && !storageState.supported}<p class="p-4 text-sm text-textcolor2">{strings.unsupported}</p>
    {:else if adding || renewalConnection}
        <ConnectionForm {renewalConnection} {strings} onconnected={onConnected} oncancel={() => { adding = false; renewalConnection = undefined }} onbusychange={value => busy = value} />
    {:else if storageState}
        {#if !storageState.connections.length}<p class="p-4 text-sm text-textcolor2">{strings.noConnections}</p>{/if}
        {#each storageState.connections as connection (connection.id)}
            {@const job = activeJob(connection)}
            <article class="card">
                <div class="card-head">
                    <div class="min-w-0">
                        <h3 class="card-title">{externalConnectionTitle(strings, connection)}</h3>
                        <p class="card-sub">{[connection.endpoint.authority, connection.endpoint.repositoryHint].join(' · ')}</p>
                    </div>
                    <div class="pills">
                        <span class="status border border-darkborderc" data-tone={connectionTone(connection)} aria-live="polite"><span class="status-dot" aria-hidden="true"></span>{connectionStatus(connection)}</span>
                    </div>
                </div>

                {#if connection.lastError && !restoreReportsError(connection, job)}<p class="text-sm text-danger-400">{errorLabel(connection.lastError)}</p>{/if}
                {#if syncFailures.has(connection.id)}
                    {@const failure = syncFailures.get(connection.id)}
                    <p class="text-sm text-danger-400" role="status">{externalErrorKind(failure) === 'clockSkew' ? language.lwwSync.clockBlocked
                        : externalErrorKind(failure) === 'previousStorageUnavailable' ? language.lwwSync.previousStorageUnavailable
                        : externalErrorMessage(strings, failure)}</p>
                    {#if ['clockSkew', 'corrupt'].includes(externalErrorKind(failure) ?? '') && supportsExternalLwwNewDevice(connection.id)}
                        <BindingTargetSwitch target={{kind:'external',connectionId:connection.id}} options={{mode:'new-device'}} label={language.lwwSync.newDeviceAction} onBound={() => refreshExternalStorageProductionState().then(() => refresh())} onError={reason => error = externalErrorMessage(strings, reason)} />
                    {/if}
                {/if}
                {#if job && (!externalJobIsActive(job) || externalJobIsPaused(job)) && job.state !== 'succeeded' && !connection.lastError && !unfinishedRestore(job)}<p class="text-sm text-danger-400" role="status">{jobLabel(job)}</p>{/if}

                {#if unfinishedRestore(job)}
                    <p class="text-sm" role="status">{strings.restoreUnfinished}</p>
                {:else if job?.state === 'uncertain'}
                    <p class="text-sm" role="status">{strings.publicationDecision}</p>
                {/if}

                {#if exportRun?.connectionId === connection.id}
                    {@const amount = exportRun.progress}
                    <SettingProgress label={strings.download} detail={amount ? `${bytes(amount.completedBytes)}${amount.totalBytes ? ` / ${bytes(amount.totalBytes)}` : ''}` : ''} fraction={amount?.totalBytes && Number(amount.totalBytes) > 0 ? Number(amount.completedBytes) / Number(amount.totalBytes) : null} />
                    <SettingButton variant="secondary" onclick={() => exportRun && bridge.cancelExport(exportRun.id)}>{strings.cancel}</SettingButton>
                {/if}
                <dl class="kv">
                    <dt>{strings.lastBackup}</dt><dd>{when(connection.lastBackupAtMs)}</dd>
                    {#if job && job.state === 'succeeded'}<dt>{strings.progress}</dt><dd role="status" aria-live="polite">{jobSummary(job)}</dd>{/if}
                </dl>
                {#if job && externalJobIsPaused(job) && job.error?.retryAtMs}<p class="text-sm">{strings.retryAt.replace('{0}', when(job.error.retryAtMs))}</p>{/if}
                {#if job && externalJobIsActive(job) && !externalJobIsPaused(job)}
                    {@const progress = externalJobProgress(job)}
                    <SettingProgress label={strings.jobActive[job.kind]} detail={jobSize(job)} fraction={progress} />
                {/if}

                {#if connection.purpose === 'backup'}
                    <SettingToggle showLabel label={strings.automaticBackup} disabled={busy} checked={!connection.automaticBackupPaused} onchange={enabled => setAutomaticWork(connection, enabled)} />
                {:else}
                    <SettingToggle showLabel label={strings.sync} disabled={busy} checked={storageState.selection.kind === 'external' && storageState.selection.connectionId === connection.id} onchange={enabled => setSyncBinding(connection, enabled)} />
                {/if}
                {#if unlockConnection?.id === connection.id}
                    <label>{strings.recoveryCode}<TextInput hideText bind:value={unlockKey} /></label>
                    <SettingButton disabled={busy || !unlockKey.trim()} onclick={unlock}>{strings.unlock}</SettingButton>
                    <SettingButton variant="secondary" disabled={busy} onclick={() => { unlockConnection = undefined; unlockKey = '' }}>{strings.cancel}</SettingButton>
                {/if}


                <div class="actions">
                    {#if connection.status === 'reauth-required' || job?.error?.action === 'reauthenticate'}
                        <SettingButton disabled={busy} onclick={() => renewalConnection = connection}>{strings.renew}</SettingButton>
                    {/if}
                    {#if connection.status === 'key-locked' || job?.error?.action === 'unlock-key'}
                        <SettingButton disabled={busy} onclick={() => unlockConnection = connection}>{strings.unlock}</SettingButton>
                    {/if}
                    {#if job?.state === 'uncertain' && job.kind === 'backup'}
                        <SettingButton disabled={busy} onclick={() => resumeJob(connection, job)}>{strings.recheckPublication}</SettingButton>
                    {/if}
                    {#if job && externalJobIsPaused(job) && ['retry', 'wait', 'free-space'].includes(job.error?.action ?? '') && ['backup', 'cleanup', 'restore', 'check-repository', 'pin-history', 'delete-history'].includes(job.kind)}
                        <SettingButton disabled={busy || Number(job.error?.retryAtMs ?? 0) > Date.now()} onclick={() => resumeJob(connection, job)}>{strings.retryAction}</SettingButton>
                    {/if}
                    {#if connection.purpose === 'backup'}<SettingButton busy={activeAction === `backup:${connection.id}`} disabled={busy || (job && externalJobIsActive(job))} onclick={() => runJob(connection, 'backup')}>{strings.runBackup}</SettingButton>
                    {:else if storageState.selection.kind === 'external' && storageState.selection.connectionId === connection.id}<SettingButton disabled={busy} onclick={() => syncNow(connection)}>{strings.sync}</SettingButton>{/if}
                    {#if job && externalJobIsActive(job)}<SettingButton variant="secondary" onclick={() => cancelJob(job)}>{strings.cancel}</SettingButton>{/if}
                    {#if job && unfinishedRestore(job)}<SettingButton variant="secondary" disabled={busy} onclick={() => stopRestore(job)}>{strings.stopRestore}</SettingButton>{/if}
                </div>

                <div class="tabs" role="tablist">
                    {#each (['history', 'quota'] as const) as tab (tab)}
                        <button type="button" role="tab" id="{connection.id}-{tab}-tab" aria-controls="{connection.id}-{tab}-panel" aria-selected={expanded[connection.id] === tab} class="tab" onclick={() => openDetails(connection, tab)}>{tab === 'history' ? strings.history : strings.quota}</button>
                    {/each}
                </div>

                {#if expanded[connection.id] === 'history'}
                    <div class="list" role="tabpanel" id="{connection.id}-history-panel" aria-labelledby="{connection.id}-history-tab">
                        {#if historyLoading[connection.id]}<p class="empty">{strings.loading}</p>
                        {:else if (history[connection.id] ?? []).length === 0}<p class="empty">{strings.noHistory}</p>{/if}
                        {#each history[connection.id] ?? [] as item (item.id)}
                            <div class="item">
                                <div class="line">
                                    <span>{historyLine(item)}</span>
                                    <span class="item-actions">
                                        <SettingButton variant="secondary" disabled={busy || (job && externalJobIsActive(job)) || !item.complete || !item.verified} onclick={() => beginRestore(connection, item)}>{strings.restore}</SettingButton>
                                        <SettingButton variant="secondary" disabled={busy || (job && externalJobIsActive(job)) || !item.complete || !item.verified} onclick={() => exportSnapshot(connection.id, item.snapshotId)}>{strings.download}</SettingButton>
                                        <SettingButton variant="secondary" disabled={busy || !item.complete || !item.verified} onclick={() => runJob(connection, 'check-repository', { snapshotId: item.snapshotId })}>{strings.check}</SettingButton>
                                        {#if item.pinned}<SettingButton variant="secondary" disabled>{strings.pinned}</SettingButton>
                                        {:else}<SettingButton variant="secondary" disabled={busy || (job && externalJobIsActive(job))} onclick={() => runJob(connection, 'pin-history', { snapshotId: item.snapshotId })}>{strings.pin}</SettingButton>{/if}
                                        {#if item.deletable}<SettingButton variant="danger" disabled={busy} onclick={() => deleteHistory(connection, item)}>{strings.deleteHistory}</SettingButton>{/if}
                                    </span>
                                </div>

                            </div>
                        {/each}
                        {#if historyCursor[connection.id]}<div><SettingButton variant="secondary" busy={historyLoading[connection.id]} onclick={() => loadHistory(connection, true)}>{strings.loadMore}</SettingButton></div>{/if}
                    </div>
                {:else if expanded[connection.id] === 'quota'}
                    {@const usage = quota[connection.id]}
                    {@const retention = connection.retentionPolicy}
                    <div class="usage" role="tabpanel" id="{connection.id}-quota-panel" aria-labelledby="{connection.id}-quota-tab">
                        {#if usage || remoteOnly[connection.id] !== undefined}
                            <dl class="kv">
                                {#if usage}
                                    <dt>{strings.usedByService}</dt>
                                    <dd>{#if usage.storage.providerPhysicalKnown && usage.storage.providerPhysicalBytes !== null}{bytes(usage.storage.providerPhysicalBytes ?? undefined)}{:else}{strings.unknownUsage} <span class="text-textcolor2">({strings.unknownUsageHelp})</span>{/if}</dd>
                                    <dt>{strings.uploadedLowerBound}</dt>
                                    <dd>{atLeast(usage.storage.locallyUploadedBytesLowerBound)} · {strings.files.replace('{0}', usage.storage.locallyUploadedObjectCountLowerBound)}</dd>
                                    {#if usage.storage.latestReachable}
                                        <dt>{strings.latestReachable}</dt>
                                        <dd>{atLeast(usage.storage.latestReachable.knownDirectBytes)}</dd>
                                    {/if}
                                {/if}
                                {#if remoteOnly[connection.id] !== undefined}
                                    <dt>{strings.remoteOnlyFiles}</dt>
                                    <dd>{remoteOnly[connection.id]}</dd>
                                {/if}
                            </dl>
                        {/if}
                        {#if usage && usage.buckets.length > 0}
                            <p>{strings.requestUsage}</p>
                            <dl class="kv">
                                {#each usage.buckets as bucket (bucket.id)}<dt>{bucket.id}</dt><dd>{bucket.used} / {bucket.limit} {bucket.unit}</dd>{/each}
                            </dl>
                        {/if}
                        {#if connection.capabilities.snapshotDiscovery && connection.capabilities.leaseOperations && connection.capabilities.deleteObjects}
                            <SettingButton busy={activeAction === `cleanup:${connection.id}`} disabled={busy || connection.status !== 'ready' || storageState.jobs.some(job => job.connectionId === connection.id && externalJobIsActive(job))} onclick={() => runJob(connection, 'cleanup')}>{strings.cleanup}</SettingButton>
                        {/if}
                        <p class="text-textcolor2">{strings.retentionHelp} {strings.retentionOtherDevices}</p>
                        <p class="text-textcolor2">{strings.cleanupTrashNotice} {strings.providerCapacityHelp}</p>
                        <label class="policy-row spread">
                            <span>{strings.retentionCount}</span>
                            <span class="amount">
                                <NumberInput size="sm" className="w-20 text-right tabular-nums" disabled={busy} min={RETENTION_LIMITS.keepCount[0]} max={RETENTION_LIMITS.keepCount[1]} value={retention.keepCount} onChange={event => commitRetentionLimit(connection, 'keepCount', event.currentTarget)} />
                            </span>
                        </label>
                        <label class="policy-row spread">
                            <span>{strings.retentionDays}</span>
                            <span class="amount">
                                <NumberInput size="sm" className="w-20 text-right tabular-nums" disabled={busy} min={RETENTION_LIMITS.keepDays[0]} max={RETENTION_LIMITS.keepDays[1]} value={retention.keepDays} onChange={event => commitRetentionLimit(connection, 'keepDays', event.currentTarget)} />
                                <span class="value">{strings.retentionDaysUnit}</span>
                            </span>
                        </label>
                    </div>
                {/if}

                <div class="foot">
                    <SettingButton variant="secondary" busy={activeAction === `settings:${connection.id}`} disabled={busy} onclick={() => createConnectionSettings(connection)}>{strings.connectionSettings}</SettingButton>
                    <SettingButton variant="danger" busy={activeAction === `remove:${connection.id}`} disabled={busy} onclick={() => removeConnection(connection)}>{strings.remove}</SettingButton>
                </div>
            </article>
        {/each}
    {/if}
    {#if error}<p class="px-4 py-3 text-sm text-danger-400" role="alert">{error}</p>{/if}
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
    .card {
        display: grid;
        gap: 0.75rem;
        padding: 0.75rem 1rem;
        min-width: 0;
    }
    .card-head {
        display: flex;
        flex-wrap: wrap;
        align-items: flex-start;
        justify-content: space-between;
        gap: 0.5rem 1rem;
    }
    .card-title {
        margin: 0;
        font-size: 0.9375rem;
        font-weight: 600;
        overflow-wrap: anywhere;
    }
    .card-sub {
        margin: 0.15rem 0 0;
        font-size: 0.8125rem;
        color: var(--risu-theme-textcolor2);
        overflow-wrap: anywhere;
    }
    .pills {
        display: flex;
        flex-wrap: wrap;
        gap: 0.4rem;
    }
    .policy-row {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        margin: 0;
        font-size: 0.875rem;
    }
    .policy-row.spread {
        justify-content: space-between;
    }
    .policy-row .value {
        color: var(--risu-theme-textcolor2);
    }
    .empty {
        margin: 0;
        padding: 0.25rem 0;
        font-size: 0.8125rem;
        color: var(--risu-theme-textcolor2);
    }
    .status {
        display: inline-flex;
        align-items: center;
        gap: 0.5rem;
        border-radius: 99px;
        padding: 0.35rem 0.75rem;
        font-size: 0.75rem;
        white-space: nowrap;
    }
    .status[data-tone="attention"] {
        color: var(--risu-theme-danger-400);
        border-color: color-mix(in srgb, var(--risu-theme-danger-400) 50%, transparent);
    }
    .status-dot {
        width: 0.45rem;
        height: 0.45rem;
        border-radius: 50%;
        background: currentColor;
        opacity: 0.35;
    }
    .status[data-tone="connected"] .status-dot {
        background: var(--risu-theme-success-500);
        opacity: 1;
    }
    .status[data-tone="attention"] .status-dot {
        opacity: 1;
    }
    .status[data-tone="paused"] .status-dot {
        background: transparent;
        box-shadow: inset 0 0 0 1.5px currentColor;
        opacity: 0.7;
    }
    .status[data-tone="working"] .status-dot {
        background: var(--risu-theme-primary-500);
        opacity: 1;
        animation: pulse 1.5s ease-in-out infinite;
    }
    .usage {
        display: grid;
        gap: 0.4rem;
    }
    .amount {
        display: flex;
        align-items: center;
        gap: 0.35rem;
    }
    .kv {
        display: grid;
        grid-template-columns: auto minmax(0, 1fr);
        gap: 0.25rem 1rem;
        margin: 0;
        font-size: 0.8125rem;
    }
    .kv dt {
        color: var(--risu-theme-textcolor2);
    }
    .kv dd {
        margin: 0;
        min-width: 0;
        overflow-wrap: anywhere;
        font-variant-numeric: tabular-nums;
    }
    .actions,
    .foot,
    .item-actions {
        display: flex;
        flex-wrap: wrap;
        gap: 0.5rem;
    }
    .foot {
        padding-top: 0.75rem;
        border-top: 1px solid color-mix(in srgb, var(--risu-theme-darkborderc) 55%, transparent);
    }
    .tabs {
        display: flex;
        gap: 0.25rem;
        border-bottom: 1px solid color-mix(in srgb, var(--risu-theme-darkborderc) 55%, transparent);
    }
    .tab {
        padding: 0.4rem 0.75rem;
        margin-bottom: -1px;
        border: 0;
        border-bottom: 2px solid transparent;
        background: transparent;
        font-size: 0.8125rem;
        color: var(--risu-theme-textcolor2);
        cursor: pointer;
    }
    .tab:hover {
        color: var(--risu-theme-textcolor);
    }
    .tab[aria-selected="true"] {
        color: var(--risu-theme-textcolor);
        border-bottom-color: var(--risu-theme-textcolor);
    }
    .tab:focus-visible {
        outline: 2px solid var(--risu-theme-selected);
        outline-offset: -2px;
    }
    .list {
        display: grid;
        gap: 0.5rem;
    }
    .item {
        display: grid;
        gap: 0.35rem;
        padding: 0.6rem 0.75rem;
        border-radius: 0.5rem;
        background: var(--risu-theme-bgcolor);
        font-size: 0.8125rem;
    }
    .line {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        justify-content: space-between;
        gap: 0.5rem;
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
    @keyframes pulse {
        50% {
            opacity: 0.3;
        }
    }
    @media (prefers-reduced-motion: reduce) {
        .status-dot {
            animation: none;
        }
    }
</style>
