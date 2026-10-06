<script lang="ts">
    import { onMount, onDestroy } from 'svelte'
    import { CheckIcon, ChevronRightIcon, LoaderCircleIcon, TriangleAlertIcon } from '@lucide/svelte'
    import { language } from 'src/lang'
    import { alertCheckboxConfirm, alertConfirm } from 'src/ts/alert'
    import { isTauri } from 'src/ts/platform'
    import { platform as nativePlatform } from '@tauri-apps/plugin-os'
    import { completeServerSyncBinding, connectServerSync, disconnectServerSync, holdServerSync, retryServerSync, getServerSyncController, getServerSyncCacheUsage, cleanupServerSyncCache, type ServerSyncCacheUsage } from 'src/ts/storage/sync/serverSyncProduction'
    import { parseServerRegistration } from 'src/ts/storage/sync/serverSyncRegistration'
    import { serverRegistrationInbox } from 'src/ts/storage/sync/serverSyncRegistrationInbox'
    import { canScanServerRegistration, createServerQrScanner } from 'src/ts/storage/sync/serverSyncQr'
    import { getAssetResidencyStatus, setAssetResidencyPolicy, evictLocalAssets, cancelAssetResidencyOperation, type AssetResidencyPolicy, type AssetResidencyStatus } from 'src/ts/storage/sync/serverAssetResidency'
    import { describeBlockedReason } from 'src/ts/storage/sync/blockedReasonText'
    import type { ServerConfig } from 'src/ts/storage/sync/serverSync'
    import { PREVIOUS_FILES_DOWNLOAD_FAILED } from 'src/ts/storage/sync/bindingFlow'
    import { formatRisuNestStorageBytes as bytes } from 'src/ts/storage/risuNestStorageDashboard'
    import { serverSyncProgressView, serverSyncRoutineView, type ServerSyncProgressView } from 'src/ts/storage/sync/serverSyncProgress'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingButton from '../RisuNest/SettingButton.svelte'
    import SegmentedButtons from '../RisuNest/SegmentedButtons.svelte'
    import SettingProgress from '../RisuNest/SettingProgress.svelte'

    interface Props {
        connectTarget?: (config: ServerConfig, newDevice: boolean) => Promise<unknown>
        /** `onboarding` drops the section heading and panel, which the onboarding screen provides. */
        tone?: 'settings' | 'onboarding'
    }

    let { connectTarget = connectServerSync, tone = 'settings' }: Props = $props()

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
    let downloading = $state(false)
    let codeOpen = $state(false)
    let policy = $state<AssetResidencyPolicy>('full')
    let pending = $state<number | undefined>()
    let now = $state(Date.now())
    let detailsOpen = $state(false)
    let dispose = () => {}
    let inboxDispose = () => {}
    let stopWatching = () => {}
    const writerRecoveryCodes = ['writer-collision', 'equal-stamp-integrity']
    const registrationCodes = ['unauthorized', 'invalid-device-token', 'server-epoch-changed']
    let writerRecovery = $derived(writerRecoveryCodes.includes(view.error))
    let connected = $derived(!!view.status.configured && !!view.status.bound)
    // A connected device keeps its registration input folded until a code is needed again.
    let codeShown = $derived(!view.status.bound || !!view.error || !!failure || !!candidate || codeOpen)
    let status = $derived.by((): { label: string; tone: 'idle' | 'connected' | 'working' | 'paused' | 'attention' } => {
        if (view.progress || (connected && view.running)) return { label: copy.running, tone: 'working' }
        if (!connected) return { label: copy.disconnected, tone: view.bindingIncomplete ? 'attention' : 'idle' }
        if (registrationCodes.includes(view.error)) return { label: copy.registrationRequired, tone: 'attention' }
        if (view.error) return { label: view.paused ? copy.blocked : copy.ready, tone: 'attention' }
        if (view.paused) return { label: copy.paused, tone: 'paused' }
        return { label: copy.ready, tone: 'connected' }
    })
    // Automatic sync shows one bar as soon as it has anything to move, and keeps it finished for a moment.
    let shown = $derived(view.progress ?? view.finished)
    let summary = $derived(shown?.mode === 'routine' ? serverSyncRoutineView(shown, copy, !view.progress) : undefined)
    // Connecting and downloading every asset show their steps; a short check finishes before they would appear.
    let progress = $derived(!shown ? undefined
        : shown.mode === 'routine' ? summary && serverSyncProgressView(shown, copy, shown.endedAt ?? now)
            : now - shown.startedAt >= 600 ? serverSyncProgressView(shown, copy, now) : undefined)
    // One place shows the panel: the asset download that started it, the server being connected, or the connection.
    let progressPlace = $derived(!progress ? undefined : downloading ? 'assets' : candidate ? 'candidate' : view.status.bound ? 'bound' : 'incomplete')
    // Read again when the connection settles, an attempt ends or the error changes.
    let pendingKey = $derived(`${connected && !view.running && !view.progress}:${view.error}`)
    let residencyOptions = $derived([
        { value: 'full' as AssetResidencyPolicy, label: copy.residency.full },
        { value: 'remote' as AssetResidencyPolicy, label: copy.residency.remote },
    ])
    const linux = (() => { try { return isTauri && nativePlatform() === 'linux' } catch { return false } })()
    const errorCode = (error: unknown) => typeof error === 'object' && error && 'code' in error ? String(error.code) : ''
    const message = (error: unknown) => {
        const token = errorCode(error)
        if (['clock-skew', 'incoming-clock-skew', 'accepted-clock-correction-required'].includes(token)) return language.lwwSync.clockBlocked
        if (writerRecoveryCodes.includes(token)) return language.lwwSync.writerCollision
        if (token === 'unit-too-large') return language.lwwSync.unitTooLarge
        if (token === 'device-credential-unavailable') return linux ? copy.credentialUnavailableLinux : copy.credentialUnavailable
        if (registrationCodes.includes(token)) return language.lwwSync.registrationRevoked
        if (token === PREVIOUS_FILES_DOWNLOAD_FAILED) return language.lwwSync.downloadFailedNotConnected
        if (token === 'previous-storage-unavailable') return language.lwwSync.previousStorageUnavailable
        if (token.startsWith('qr-')) return token.includes('permission') ? copy.cameraDenied : copy.cameraUnavailable
        return copy.errorHelp
    }
    async function refresh() { await controller.ensureStatus(); residency = await getAssetResidencyStatus(); cache = await getServerSyncCacheUsage() }
    async function run(operation: () => Promise<unknown>) {
        if (busy) return
        busy = true; failure = ''
        // A failure the operation already explained stays over a later refresh error.
        try { await operation(); await refresh() } catch (error) { failure ||= message(error) } finally { busy = false }
    }
    function readCode() { try { candidate = parseServerRegistration(code); code = ''; failure = '' } catch { failure = copy.registrationInvalid } }
    async function scan() { scanning = true; try { candidate = await scanner.scan(() => {}); failure = '' } catch (error) { failure = message(error) } finally { scanning = false } }
    async function connect(newDevice = false) { if (!candidate) return; await connectTarget(candidate, newDevice); candidate = undefined; codeOpen = false }
    async function downloadAll() { downloading = true; try { await controller.track('assets', () => setAssetResidencyPolicy('full')) } finally { downloading = false } }
    async function downloadHeld() { const release = await holdServerSync(); try { await downloadAll() } finally { await release() } }
    async function disconnect() {
        let serverObjects = 0
        try { serverObjects = (await getAssetResidencyStatus()).serverObjects } catch {}
        if (!serverObjects) return disconnectServerSync()
        const choice = await alertCheckboxConfirm({ title: copy.disconnectTitle, description: copy.disconnectRemoteOnly, checkboxLabel: copy.downloadThenDisconnect, actionLabel: copy.disconnect, cancelLabel: language.cancel, requireChecked: false })
        if (!choice.confirmed) return
        if (!choice.checked) return disconnectServerSync()
        const release = await holdServerSync()
        try {
            try { await downloadAll() } catch (error) {
                if (errorCode(error) === 'cancelled') return
                // Files that are on neither this device nor the server fail the download but are not lost by disconnecting.
                let remaining: number | undefined
                try { remaining = (await getAssetResidencyStatus()).serverObjects } catch {}
                if (remaining !== 0) { failure = copy.downloadFailedKeptConnection; return }
            }
            await disconnectServerSync()
        } finally { await release() }
    }
    async function cleanCache() { if (await alertConfirm(copy.management.cleanConfirm)) await cleanupServerSyncCache() }
    // A refused change puts the control back on the stored policy.
    function choosePolicy(next: AssetResidencyPolicy) { void run(() => next === 'full' ? downloadAll() : setAssetResidencyPolicy(next)).finally(() => { if (residency) policy = residency.policy }) }
    $effect(() => { if (residency) policy = residency.policy })
    $effect(() => {
        if (!view.progress) return
        now = Date.now()
        const timer = setInterval(() => { now = Date.now() }, 500)
        return () => clearInterval(timer)
    })
    $effect(() => {
        if (!pendingKey.startsWith('true:')) return
        void controller.pendingChanges().then(value => { pending = value }, () => {})
    })
    onMount(() => {
        if (!isTauri) return
        dispose = controller.subscribe(value => { view = value })
        stopWatching = controller.watchProgress()
        inboxDispose = serverRegistrationInbox.changed.subscribe(() => { const pending = serverRegistrationInbox.take(); if (pending) candidate = pending })
        void refresh().catch(error => { failure ||= message(error) })
    })
    onDestroy(() => { dispose(); stopWatching(); inboxDispose(); scanner.cancel(); serverRegistrationInbox.releaseConsumed() })
</script>

{#snippet statusPill()}
    <span class="status" data-tone={status.tone} aria-live="polite"><span class="status-dot" aria-hidden="true"></span>{status.label}</span>
{/snippet}

{#snippet connection()}
    {#if !connected}
        <div class="notice">
            <TriangleAlertIcon size={16} class="mt-0.5 shrink-0" aria-hidden="true" />
            <p>{language.lwwSync.concurrentEditNotice}</p>
        </div>
    {/if}
    {#if view.bindingIncomplete}
        <div class="sync-block">
            <p role="status" class="text-sm">{language.lwwSync.bindingIncomplete}</p>
            <div class="actions">
                <SettingButton onclick={() => void run(completeServerSyncBinding)} busy={busy}>{copy.connect}</SettingButton>
            </div>
            {#if progressPlace === 'incomplete'}{@render progressPanel()}{/if}
        </div>
    {/if}
    {#if view.status.bound}
        <div class="sync-block">
            {#if connected && (view.status.libraryId || view.status.deviceId || pending !== undefined)}
                <dl class="kv">
                    {#if view.status.libraryId}<dt>{copy.libraryId}</dt><dd>{view.status.libraryId}</dd>{/if}
                    {#if view.status.deviceId}<dt>{copy.deviceId}</dt><dd>{view.status.deviceId}</dd>{/if}
                    {#if pending !== undefined}<dt>{copy.pendingChanges}</dt><dd>{copy.count.replace('{0}', pending.toLocaleString())}</dd>{/if}
                    {#if view.lastSuccessAt !== undefined && !progress}<dt>{copy.lastSuccess}</dt><dd>{new Date(view.lastSuccessAt).toLocaleString()}</dd>{/if}
                </dl>
            {/if}
            {#if progressPlace === 'bound'}{@render progressPanel()}{/if}
            <div class="actions">
                {#if !view.bindingIncomplete}<SettingButton onclick={() => void run(retryServerSync)} disabled={busy}>{copy.syncNow}</SettingButton>{/if}
                <SettingButton variant="danger" onclick={() => void run(disconnect)} disabled={busy}>{copy.disconnect}</SettingButton>
            </div>
            {@render alert()}
        </div>
    {/if}
    {#if codeShown}
        <div class="sync-block">
            {#if candidate}
                <p class="text-[15px]">{copy.reviewTitle}</p>
                <dl class="kv review">
                    <dt>{copy.endpoint}</dt><dd>{candidate.endpoint}</dd>
                    <dt>{copy.libraryId}</dt><dd>{candidate.libraryId}</dd>
                </dl>
                <div class="actions">
                    <SettingButton onclick={() => void run(() => connect())} busy={busy} disabled={busy}>{copy.connect}</SettingButton>
                    {#if writerRecovery}<SettingButton variant="secondary" onclick={() => void run(() => connect(true))} disabled={busy}>{language.lwwSync.newDeviceAction}</SettingButton>{/if}
                    <SettingButton variant="secondary" onclick={() => { candidate = undefined }} disabled={busy}>{copy.discardRegistration}</SettingButton>
                </div>
                {#if progressPlace === 'candidate'}{@render progressPanel()}{:else}<p class="help">{copy.connectHint}</p>{/if}
            {:else}
                <label class="text-[15px]" for="server-registration">{copy.registrationCode}</label>
                {#if tone === 'settings'}<p class="help">{canScanServerRegistration ? copy.connectRowHelpScan : copy.connectRowHelp}</p>{/if}
                <textarea id="server-registration" class="code" rows="3" spellcheck="false" autocapitalize="none" bind:value={code} disabled={busy || scanning}></textarea>
                <div class="actions">
                    <SettingButton onclick={readCode} disabled={busy || !code}>{copy.readRegistration}</SettingButton>
                    {#if canScanServerRegistration}
                        <SettingButton variant="secondary" onclick={() => void scan()} busy={scanning} disabled={busy}>{copy.scanRegistration}</SettingButton>
                    {/if}
                    {#if scanning}<SettingButton variant="secondary" onclick={() => scanner.cancel()}>{copy.cancelScan}</SettingButton>{/if}
                </div>
            {/if}
            {#if !view.status.bound}{@render alert()}{/if}
        </div>
    {:else}
        <SettingRow label={copy.registrationCode} help={canScanServerRegistration ? copy.connectRowHelpScan : copy.connectRowHelp}>
            <SettingButton variant="secondary" aria-expanded={false} onclick={() => { codeOpen = true }}>{copy.enterCode}</SettingButton>
        </SettingRow>
    {/if}
{/snippet}

{#snippet alert()}
    {#if view.error || failure}<p role="alert" class="text-sm text-danger-400">{failure || message({ code: view.error })}</p>{/if}
{/snippet}

{#snippet progressSteps(steps: ServerSyncProgressView)}
    {#if steps.stages.length > 1}
        <ol class="stages">
            {#each steps.stages as stage (stage.stage)}
                <li data-state={stage.state} aria-current={stage.state === 'active' ? 'step' : undefined}>
                    {#if stage.state === 'done'}<CheckIcon size={14} aria-hidden="true" />{:else}<LoaderCircleIcon size={14} class="motion-safe:animate-spin" aria-hidden="true" />{/if}
                    <span>{stage.label}</span>
                </li>
            {/each}
        </ol>
    {/if}
    <dl class="kv">
        {#each steps.counters as counter (counter.key)}<dt>{counter.label}</dt><dd>{counter.value}</dd>{/each}
    </dl>
{/snippet}

{#snippet progressPanel()}
    {#if summary && progress}
        <div class="progress" data-sync-progress data-mode="routine">
            <SettingProgress label={summary.label} fraction={summary.fraction} done={summary.complete} />
            <details class="group" bind:open={detailsOpen}>
                <summary class="flex cursor-pointer list-none items-center gap-2 text-sm text-textcolor2 select-none [&::-webkit-details-marker]:hidden">
                    <ChevronRightIcon size={16} class="shrink-0 transition-transform duration-200 group-open:rotate-90" aria-hidden="true" />
                    <span>{copy.details}</span>
                </summary>
                <div class="details">
                    {#if !summary.complete}<p class="activity">{progress.detail ? `${progress.label} · ${progress.detail}` : progress.label}</p>{/if}
                    {@render progressSteps(progress)}
                </div>
            </details>
        </div>
    {:else if progress}
        <div class="progress" data-sync-progress>
            <SettingProgress label={progress.label} detail={progress.detail} fraction={progress.fraction} />
            {@render progressSteps(progress)}
        </div>
    {/if}
{/snippet}

{#if isTauri}
    {#if tone === 'onboarding'}
        <div class="embedded">
            {#if status.tone !== 'idle'}<div class="embedded-status">{@render statusPill()}</div>{/if}
            {@render connection()}
        </div>
    {:else}
        <SettingGroup id="risunest-server-sync" title={copy.title} description={copy.description} actions={statusPill}>
            {@render connection()}
        </SettingGroup>
    {/if}
    {#if residency && connected}
        <SettingGroup title={copy.residency.title} description={copy.residency.description}>
            <div class="sync-block">
                <SegmentedButtons bind:value={policy} options={residencyOptions} label={copy.residency.title} role="radiogroup" disabled={busy} onchange={choosePolicy} />
            </div>
            <SettingRow inline label={copy.residency.local}><span class="value">{bytes(residency.localBytes)}</span></SettingRow>
            <SettingRow inline label={copy.residency.remoteOnly}><span class="value">{bytes(residency.serverBytes)}</span></SettingRow>
            {#if residency.remoteObjects > residency.serverObjects}<SettingRow inline label={copy.residency.externalOnly}><span class="value">{bytes(residency.remoteBytes - residency.serverBytes)}</span></SettingRow>{/if}
            <SettingRow inline label={copy.residency.unavailable}><span class="value">{copy.count.replace('{0}', residency.unavailableObjects.toLocaleString())}</span></SettingRow>
            <div class="sync-block">
                <div class="actions">
                    {#if residency.policy === 'full' && residency.remoteObjects > 0}<SettingButton onclick={() => void run(downloadHeld)} busy={downloading} disabled={busy}>{copy.residency.download}</SettingButton>{/if}
                    <SettingButton variant="secondary" onclick={() => void run(evictLocalAssets)} disabled={busy}>{copy.residency.clean}</SettingButton>
                    {#if busy}<SettingButton variant="secondary" onclick={() => void cancelAssetResidencyOperation()}>{copy.residency.cancel}</SettingButton>{/if}
                </div>
                {#if progressPlace === 'assets'}{@render progressPanel()}{/if}
                {#if downloading}<p role="status" class="text-sm">{copy.residency.working}</p>{/if}
                <p class="help">{copy.residency.cleanupNote}</p>
            </div>
        </SettingGroup>
    {/if}
    {#if cache && tone === 'settings'}
        <SettingGroup title={copy.management.title}>
            <SettingRow inline label={copy.management.cache}><span class="value">{bytes(cache.cacheBytes)}</span></SettingRow>
            <SettingRow inline label={copy.management.protected}><span class="value">{bytes(cache.protectedBytes)}</span></SettingRow>
            <SettingRow inline label={copy.management.ledger} help={copy.management.ledgerHelp}><span class="value">{bytes(cache.ledgerBytes)}</span></SettingRow>
            <div class="sync-block">
                <div class="actions">
                    <SettingButton variant="secondary" onclick={() => void run(cleanCache)} disabled={busy || !!cache.blockedReason}>{copy.management.clean}</SettingButton>
                </div>
                {#if cache.blockedReason}<p class="help">{describeBlockedReason(cache.blockedReason)}</p>{/if}
            </div>
        </SettingGroup>
    {/if}
{/if}

<style>
    .sync-block {
        display: grid;
        gap: 0.625rem;
        min-width: 0;
        padding: 0.75rem 1rem;
        overflow-wrap: anywhere;
    }
    .actions {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.5rem;
    }
    .help {
        max-width: 62ch;
        font-size: 13px;
        line-height: 1.5;
        color: color-mix(in srgb, var(--risu-theme-textcolor2) 62%, var(--risu-theme-textcolor) 38%);
    }
    .notice {
        display: flex;
        gap: 0.625rem;
        margin: 0.75rem 1rem;
        padding: 0.625rem 0.75rem;
        border: 1px solid color-mix(in srgb, var(--risu-theme-danger-400) 40%, transparent);
        border-radius: 0.5rem;
        background: color-mix(in srgb, var(--risu-theme-danger-400) 7%, transparent);
        color: var(--risu-theme-textcolor);
        font-size: 13px;
        line-height: 1.5;
    }
    .notice :global(svg) {
        color: var(--risu-theme-danger-400);
    }
    .code {
        width: 100%;
        min-height: 4.5rem;
        resize: vertical;
        padding: 0.5rem 0.75rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 0.375rem;
        background: var(--risu-theme-bgcolor);
        color: var(--risu-theme-textcolor);
        font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
        font-size: 12.5px;
        line-height: 1.5;
        word-break: break-all;
    }
    .code:focus-visible {
        outline: 2px solid var(--risu-theme-selected);
        outline-offset: 1px;
    }
    .code:disabled {
        opacity: 0.5;
    }
    .kv {
        display: grid;
        grid-template-columns: auto minmax(0, 1fr);
        gap: 0.25rem 1rem;
        margin: 0;
        font-size: 13px;
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
    .kv.review {
        padding: 0.75rem 0.875rem;
        border: 1px solid color-mix(in srgb, var(--risu-theme-primary-500) 35%, transparent);
        border-radius: 0.5rem;
        background: color-mix(in srgb, var(--risu-theme-primary-500) 8%, transparent);
    }
    .kv.review dd {
        font-weight: 600;
    }
    .progress {
        display: grid;
        gap: 0.625rem;
        min-width: 0;
    }
    /* An automatic sync bar fills from empty when it appears. */
    .progress[data-mode='routine'] :global([role='progressbar'] > div) {
        animation: risunest-sync-fill 0.6s ease-out;
    }
    @keyframes -global-risunest-sync-fill {
        from {
            width: 0;
        }
    }
    .details {
        display: grid;
        gap: 0.625rem;
        min-width: 0;
        margin-top: 0.5rem;
        padding-left: 1.5rem;
    }
    .activity {
        font-size: 13px;
    }
    .stages {
        display: grid;
        gap: 0.375rem;
        margin: 0;
        padding: 0;
        list-style: none;
        font-size: 13px;
    }
    .stages li {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        min-width: 0;
        color: var(--risu-theme-textcolor2);
    }
    .stages li[data-state='active'] {
        color: var(--risu-theme-textcolor);
    }
    .stages li[data-state='done'] :global(svg) {
        color: var(--risu-theme-success-500);
    }
    .value {
        font-size: 14px;
        font-variant-numeric: tabular-nums;
        white-space: nowrap;
    }
    .status {
        display: inline-flex;
        align-items: center;
        gap: 0.5rem;
        padding: 0.3rem 0.75rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 99px;
        font-size: 0.75rem;
        white-space: nowrap;
    }
    .status[data-tone='attention'] {
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
    .status[data-tone='connected'] .status-dot {
        background: var(--risu-theme-success-500);
        opacity: 1;
    }
    .status[data-tone='attention'] .status-dot {
        opacity: 1;
    }
    .status[data-tone='paused'] .status-dot {
        background: transparent;
        box-shadow: inset 0 0 0 1.5px currentColor;
        opacity: 0.7;
    }
    .status[data-tone='working'] .status-dot {
        background: var(--risu-theme-primary-500);
        opacity: 1;
        animation: status-pulse 1.5s ease-in-out infinite;
    }
    @keyframes status-pulse {
        50% {
            opacity: 0.3;
        }
    }
    @media (prefers-reduced-motion: reduce) {
        .status-dot,
        .progress[data-mode='routine'] :global([role='progressbar'] > div) {
            animation: none;
        }
    }
    .embedded {
        display: grid;
        gap: 0.25rem;
        min-width: 0;
    }
    .embedded-status {
        display: flex;
        padding: 0 0 0.25rem;
    }
    .embedded .notice {
        margin: 0 0 0.5rem;
    }
    .embedded .sync-block {
        padding: 0.5rem 0;
    }
</style>
