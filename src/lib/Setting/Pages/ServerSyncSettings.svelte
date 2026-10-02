<script lang="ts">
    import { onMount, onDestroy } from 'svelte'
    import { language } from 'src/lang'
    import { alertConfirm } from 'src/ts/alert'
    import { isTauri } from 'src/ts/platform'
    import { connectServerSync, disconnectServerSync, retryServerSync, getServerSyncController, getServerSyncCacheUsage, cleanupServerSyncCache, type ServerSyncCacheUsage } from 'src/ts/storage/sync/serverSyncProduction'
    import { parseServerRegistration } from 'src/ts/storage/sync/serverSyncRegistration'
    import { serverRegistrationInbox } from 'src/ts/storage/sync/serverSyncRegistrationInbox'
    import { canScanServerRegistration, createServerQrScanner } from 'src/ts/storage/sync/serverSyncQr'
    import { getAssetResidencyStatus, setAssetResidencyPolicy, evictLocalAssets, cancelAssetResidencyOperation, type AssetResidencyStatus } from 'src/ts/storage/sync/serverAssetResidency'
    import { describeBlockedReason } from 'src/ts/storage/sync/blockedReasonText'
    import type { ServerConfig } from 'src/ts/storage/sync/serverSync'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingButton from '../RisuNest/SettingButton.svelte'

    const copy = language.risuNest.serverSync
    const controller = getServerSyncController()
    const scanner = createServerQrScanner()
    let view = $state(controller.snapshot())
    let code = $state('')
    let candidate = $state<ServerConfig | undefined>()
    let busy = $state(false)
    let scanning = $state(false)
    let failure = $state('')
    let residency = $state<AssetResidencyStatus | undefined>()
    let cache = $state<ServerSyncCacheUsage | undefined>()
    let dispose = () => {}
    let inboxDispose = () => {}
    const message = (error: unknown) => {
        const token = typeof error === 'object' && error && 'code' in error ? String(error.code) : ''
        if (['clock-skew', 'incoming-clock-skew', 'accepted-clock-correction-required'].includes(token)) return language.lwwSync.clockBlocked
        if (['writer-collision', 'equal-stamp-integrity'].includes(token)) return language.lwwSync.writerCollision
        if (token.startsWith('credential-')) return copy.credentialUnavailable
        if (token.startsWith('qr-')) return token.includes('permission') ? copy.cameraDenied : copy.cameraUnavailable
        return copy.errorHelp
    }
    async function refresh() { await controller.ensureStatus(); residency = await getAssetResidencyStatus(); cache = await getServerSyncCacheUsage() }
    async function run(operation: () => Promise<unknown>) {
        if (busy) return
        busy = true; failure = ''
        try { await operation(); await refresh() } catch (error) { failure = message(error) } finally { busy = false }
    }
    function readCode() { try { candidate = parseServerRegistration(code); code = ''; failure = '' } catch { failure = copy.registrationInvalid } }
    async function scan() { scanning = true; try { candidate = await scanner.scan(() => {}); failure = '' } catch (error) { failure = message(error) } finally { scanning = false } }
    async function connect(newDevice = false) { if (!candidate) return; await connectServerSync(candidate, newDevice); candidate = undefined }
    async function cleanCache() { if (await alertConfirm(copy.management.cleanConfirm)) await cleanupServerSyncCache() }
    const bytes = (value: number) => `${(value / 1024 / 1024).toFixed(1)} MiB`
    onMount(() => {
        if (!isTauri) return
        dispose = controller.subscribe(value => { view = value })
        inboxDispose = serverRegistrationInbox.changed.subscribe(() => { const pending = serverRegistrationInbox.take(); if (pending) candidate = pending })
        void refresh().catch(error => { failure = message(error) })
    })
    onDestroy(() => { dispose(); inboxDispose(); scanner.cancel(); serverRegistrationInbox.releaseConsumed() })
</script>

{#if isTauri}
    <SettingGroup title={copy.title} description={copy.description}>
        <p class="mb-3 text-sm text-textcolor">{language.lwwSync.concurrentEditNotice}</p>
        <SettingRow label={copy.connectRow} help={copy.connectRowHelpScan}>
            <span>{view.status.configured && !view.paused ? copy.ready : copy.disconnected}</span>
        </SettingRow>
        <label class="block text-sm" for="server-registration">{copy.registrationCode}</label>
        <textarea id="server-registration" class="my-2 w-full rounded border border-darkborderc bg-darkbg p-2" bind:value={code} disabled={busy || scanning}></textarea>
        <div class="flex flex-wrap gap-2">
            <SettingButton onclick={readCode} disabled={busy || !code}>{copy.readRegistration}</SettingButton>
            {#if canScanServerRegistration}
                <SettingButton onclick={() => void scan()} disabled={busy || scanning}>{copy.scanRegistration}</SettingButton>
            {/if}
            {#if scanning}<SettingButton onclick={() => scanner.cancel()}>{copy.cancelScan}</SettingButton>{/if}
        </div>
        {#if candidate}
            <SettingRow label={copy.endpoint}><span class="break-all">{candidate.endpoint}</span></SettingRow>
            <SettingRow label={copy.libraryId}><span class="break-all">{candidate.libraryId}</span></SettingRow>
            <div class="flex flex-wrap gap-2">
                <SettingButton onclick={() => void run(() => connect())} disabled={busy}>{copy.connect}</SettingButton>
                <SettingButton onclick={() => void run(() => connect(true))} disabled={busy}>{language.lwwSync.newDeviceAction}</SettingButton>
                <SettingButton onclick={() => { candidate = undefined }} disabled={busy}>{copy.discardRegistration}</SettingButton>
            </div>
        {/if}
        {#if view.status.bound}
            <div class="mt-2 flex flex-wrap gap-2">
                <SettingButton onclick={() => void run(retryServerSync)} disabled={busy}>{copy.syncNow}</SettingButton>
                <SettingButton onclick={() => void run(disconnectServerSync)} disabled={busy}>{copy.disconnect}</SettingButton>
            </div>
        {/if}
        {#if view.error || failure}<p role="alert" class="mt-2 text-sm">{failure || message({ code: view.error })}</p>{/if}
    </SettingGroup>
    {#if residency && view.status.configured}
        <SettingGroup title={copy.residency.title} description={copy.residency.description}>
            <SettingRow label={copy.residency.local}><span>{bytes(residency.localBytes)}</span></SettingRow>
            <SettingRow label={copy.residency.remoteOnly}><span>{bytes(residency.remoteBytes)}</span></SettingRow>
            <SettingRow label={copy.residency.unavailable}><span>{residency.unavailableObjects}</span></SettingRow>
            <div class="flex flex-wrap gap-2">
                <SettingButton onclick={() => void run(() => setAssetResidencyPolicy('full'))} disabled={busy || residency.policy === 'full'}>{copy.residency.full}</SettingButton>
                <SettingButton onclick={() => void run(() => setAssetResidencyPolicy('remote'))} disabled={busy || residency.policy === 'remote'}>{copy.residency.remote}</SettingButton>
                <SettingButton onclick={() => void run(evictLocalAssets)} disabled={busy}>{copy.residency.clean}</SettingButton>
                {#if busy}<SettingButton onclick={() => void cancelAssetResidencyOperation()}>{copy.residency.cancel}</SettingButton>{/if}
            </div>
            <p class="mt-2 text-sm">{copy.residency.cleanupNote}</p>
        </SettingGroup>
    {/if}
    {#if cache}
        <SettingGroup title={copy.management.title}>
            <SettingRow label={copy.management.cache}><span>{bytes(cache.cacheBytes)}</span></SettingRow>
            <SettingRow label={copy.management.protected}><span>{bytes(cache.protectedBytes)}</span></SettingRow>
            <SettingRow label={copy.management.ledger}><span>{bytes(cache.ledgerBytes)}</span></SettingRow>
            <SettingButton onclick={() => void run(cleanCache)} disabled={busy || !!cache.blockedReason}>{copy.management.clean}</SettingButton>
            {#if cache.blockedReason}<p class="text-sm">{describeBlockedReason(cache.blockedReason)}</p>{/if}
        </SettingGroup>
    {/if}
{/if}
