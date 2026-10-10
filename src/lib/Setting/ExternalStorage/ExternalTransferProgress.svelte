<script lang="ts">
    import { onMount, type Snippet } from 'svelte'
    import TransferProgress from '../RisuNest/TransferProgress.svelte'
    import { subscribeExternalProgress, externalProgressFor, externalJobFraction, externalJobTotalGrows, validTransferRateSample, type ExternalOperationProgress } from 'src/ts/storage/sync/external/progress'
    import { externalJobIsActive, externalJobIsPaused } from 'src/ts/storage/sync/external/connection'
    import type { ExternalJobSummary } from 'src/ts/storage/sync/external/types'
    import { formatRisuNestStorageBytes as bytes } from 'src/ts/storage/risuNestStorageDashboard'
    import { formatRemaining } from 'src/ts/storage/sync/remainingTime'
    import type { ExternalStorageStrings } from './strings'

    let { connectionId, strings, job, remaining, onboarding = false, syncing = false, exportActions }: {
        connectionId: string
        strings: ExternalStorageStrings
        job?: ExternalJobSummary
        remaining?: number
        onboarding?: boolean
        syncing?: boolean
        exportActions?: Snippet
    } = $props()
    let observations = $state<ReadonlyMap<string, ExternalOperationProgress>>(new Map())
    onMount(() => subscribeExternalProgress(value => observations = value))
    const operations = $derived([
        externalProgressFor(observations, connectionId), externalProgressFor(observations, connectionId, 'download'),
        externalProgressFor(observations, connectionId, 'export'),
    ].filter((value): value is ExternalOperationProgress => !!value))
    const activeJob = $derived(job && externalJobIsActive(job) && !externalJobIsPaused(job) ? job : undefined)
    const copy = $derived(strings.transfer)
    const localApply = $derived(activeJob?.kind === 'restore' && ['preparing-local', 'applying-local', 'awaiting-adoption'].includes(activeJob.phase))
    const jobCounters = $derived.by(() => {
        const rows: { key: string; label: string; value: string }[] = []
        const transfer = activeJob?.transfer
        if (transfer && Number(transfer.uploadedObjects) > 0) rows.push({ key: 'upload', label: copy.uploaded, value: `${bytes(Number(transfer.uploadedBytes))} · ${copy.objects.replace('{0}', transfer.uploadedObjects)}` })
        if (remaining !== undefined && activeJob && externalJobFraction(activeJob) !== null) rows.push({ key: 'remaining', label: strings.remaining, value: formatRemaining(remaining) })
        return rows
    })
    function counters(operation: ExternalOperationProgress) {
        const rows: { key: string; label: string; value: string }[] = []
        const amount = operation.amounts
        if (Number(amount.preparedBytes) > 0) rows.push({ key: 'prepared', label: copy.prepared, value: bytes(Number(amount.preparedBytes)) })
        for (const direction of ['uploaded', 'downloaded'] as const) {
            if (Number(amount[`${direction}Objects`]) > 0) rows.push({
                key: direction, label: copy[direction],
                value: `${bytes(Number(amount[`${direction}Bytes`]))} · ${copy.objects.replace('{0}', amount[`${direction}Objects`])}`,
            })
        }
        if (operation.totalItems !== undefined) rows.push({ key: 'files', label: copy.files, value: `${operation.completedItems ?? 0} / ${operation.totalItems}` })
        return rows
    }
</script>

<div class="external-progress">
    {#each operations as operation (operation.kind)}
        {@const done = operation.state === 'complete'}
        {@const stopped = operation.state === 'failed' || operation.state === 'cancelled' || operation.stage === 'waiting'}
        {@const title = operation.kind === 'export' ? strings.download : operation.kind === 'download' ? copy.filesDownloading : operation.kind === 'binding' ? copy.connecting : copy.syncing}
        {@const label = done ? operation.kind === 'export' ? strings.completed : operation.kind === 'download' ? copy.downloadComplete : operation.kind === 'binding' ? copy.connectionComplete : copy.syncComplete : operation.state === 'cancelled' ? strings.cancelled : operation.state === 'failed' ? strings.failed : operation.kind === 'export' ? strings.download : operation.kind === 'download' ? copy.filesDownloading : copy[operation.stage]}
        {@const exported = operation.exported}
        {@const detail = exported ? `${bytes(Number(exported.completedBytes))}${exported.totalBytes ? ` / ${bytes(Number(exported.totalBytes))}` : ''}` : stopped || done || title === label ? '' : title}
        <div data-external-progress={operation.kind}>
            <TransferProgress {label} {detail} {done} {stopped}
                fraction={done ? 1 : exported && Number(exported.totalBytes) > 0 ? Number(exported.completedBytes) / Number(exported.totalBytes) : operation.kind === 'download' && operation.totalItems ? (operation.completedItems ?? 0) / operation.totalItems : null}
                counters={counters(operation)} detailsLabel={copy.details} collapsible={!onboarding}
                actions={operation.kind === 'export' && operation.state === 'running' ? exportActions : undefined}
                speed={operation.state === 'running' && validTransferRateSample(operation.network) ? {
                    sample: operation.network, active: operation.stage !== 'waiting' && !['applying', 'finalizing'].includes(operation.stage),
                    uploadLabel: copy.uploadSpeed, downloadLabel: copy.downloadSpeed,
                } : undefined} />
        </div>
    {/each}
    {#if activeJob}
        {@const fraction = externalJobFraction(activeJob)}
        {@const size = `${bytes(Number(activeJob.completedBytes))}${activeJob.totalBytes && !externalJobTotalGrows(activeJob) ? ` / ${bytes(Number(activeJob.totalBytes))}` : ''}`}
        <TransferProgress
            label={(activeJob.kind === 'restore' && strings.restorePhases[activeJob.phase]) || strings.jobActive[activeJob.kind]}
            detail={localApply ? '' : activeJob.counters ? `${strings.jobCounters[activeJob.counters]} ${size}` : size}
            {fraction} detailsLabel={copy.details} collapsible={!onboarding}
            counters={jobCounters}
            speed={validTransferRateSample(activeJob.transfer?.network) ? {
                sample: activeJob.transfer.network, active: !localApply, uploadLabel: copy.uploadSpeed, downloadLabel: copy.downloadSpeed,
            } : undefined} />
    {:else if onboarding && operations.length === 0}
        <TransferProgress label={syncing ? copy.connecting : strings.jobActive.restore} detailsLabel={copy.details} />
    {/if}
</div>

<style>
    .external-progress { display: grid; gap: 0.75rem; min-width: 0; }
    .external-progress:empty { display: none; }
</style>
