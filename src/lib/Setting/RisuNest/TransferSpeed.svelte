<script lang="ts">
    import { onMount } from 'svelte'
    import { createRateMeter, type TransferRateSample } from 'src/ts/storage/sync/transferRate'
    import { formatRisuNestStorageBytes as bytes } from 'src/ts/storage/risuNestStorageDashboard'

    let { sample, active = true, uploadLabel, downloadLabel }: { sample: TransferRateSample; active?: boolean; uploadLabel: string; downloadLabel: string } = $props()
    const upload = createRateMeter(), download = createRateMeter()
    let id: string | undefined, at = -1, receivedAt = 0
    let rates = $state<{ upload?: number; download?: number }>({})
    function refresh() {
        const now = at + performance.now() - receivedAt
        rates = { upload: upload.rate(now), download: download.rate(now) }
    }
    $effect(() => {
        if (sample.id !== id) { id = sample.id; at = -1; upload.reset(); download.reset() }
        if (sample.atMs > at) {
            at = sample.atMs; receivedAt = performance.now()
            upload.add(at, Number(sample.sentBytes)); download.add(at, Number(sample.receivedBytes))
        }
        refresh()
    })
    onMount(() => { const timer = setInterval(refresh, 500); return () => clearInterval(timer) })
</script>

{#if active && ((sample.sending && rates.upload !== undefined) || (sample.receiving && rates.download !== undefined))}
    <p class="speed" data-transfer-speed>
        {#if sample.sending && rates.upload !== undefined}<span aria-label={uploadLabel}>↑ {bytes(rates.upload)}/s</span>{/if}
        {#if sample.receiving && rates.download !== undefined}<span aria-label={downloadLabel}>↓ {bytes(rates.download)}/s</span>{/if}
    </p>
{/if}

<style>
    .speed { display: flex; flex-wrap: wrap; gap: 0 1rem; margin: 0; color: var(--risu-theme-textcolor2); font-size: 12px; line-height: 18px; font-variant-numeric: tabular-nums; }
</style>
