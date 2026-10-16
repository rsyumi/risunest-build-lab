<script lang="ts">
    import CloudIcon from '@lucide/svelte/icons/cloud'
    import CloudAlertIcon from '@lucide/svelte/icons/cloud-alert'
    import CloudCheckIcon from '@lucide/svelte/icons/cloud-check'
    import CloudOffIcon from '@lucide/svelte/icons/cloud-off'
    import CloudSyncIcon from '@lucide/svelte/icons/cloud-sync'
    import { language } from 'src/lang'
    import { externalStorageStrings } from 'src/lib/Setting/ExternalStorage/strings'
    import { syncStatusPanel } from 'src/ts/gui/syncStatusPanel'
    import { persistentLocalSaveFailure } from 'src/ts/storage/persistentDataRuntime.svelte'
    import { syncStatus } from 'src/ts/storage/sync/syncStatus'
    import { syncStatusDisplay } from 'src/ts/storage/sync/syncStatusView'
    import { DBState } from 'src/ts/stores.svelte'
    import SyncStatusDot from './SyncStatusDot.svelte'

    /** `slot` is the narrow sidebar's footer, `menu` the labelled sidebar column, `header` the mobile header. */
    let { variant }: { variant: 'slot' | 'menu' | 'header' } = $props()

    const attention = 'text-[color:color-mix(in_srgb,var(--risu-theme-danger-400)_80%,var(--risu-theme-textcolor))]'
    const icons = { connected: CloudCheckIcon, working: CloudSyncIcon, idle: CloudOffIcon, paused: CloudIcon, failed: CloudAlertIcon, attention: CloudAlertIcon }
    let button = $state<HTMLButtonElement>()
    const display = $derived((DBState.db.risunestSyncStatusUi ?? true) && $syncStatus && syncStatusDisplay($syncStatus, externalStorageStrings(DBState.db.language), $persistentLocalSaveFailure !== null))
    const open = $derived(!!button && $syncStatusPanel?.anchor === button)

    function toggle() {
        if (button) syncStatusPanel.set(open ? null : { anchor: button, placement: variant === 'header' ? 'below' : 'side' })
    }
</script>

{#if display}
    {@const Icon = icons[display.tone]}
    {@const name = `${language.risuNest.tabs.sync}: ${display.label}`}
    {#if variant === 'menu'}
        <button bind:this={button} type="button" class="flex w-full flex-col items-center justify-center gap-1 py-2 {display.tone === 'attention' ? attention : open ? '' : 'text-textcolor2'}"
            aria-label={name} title={name} aria-haspopup="dialog" aria-expanded={open} onclick={toggle} data-sync-status={display.tone}>
            <span class="relative"><Icon /><SyncStatusDot tone={display.tone} class="-right-1 -top-0.5 ring-bgcolor" /></span>
            <span class="text-xs">{language.risuNest.syncStatus.menuLabel}</span>
        </button>
    {:else}
        {@const control = `group relative flex h-11 shrink-0 cursor-pointer items-center justify-center rounded-md transition-colors hover:bg-selected ${variant === 'slot' ? 'w-14' : 'w-11'} ${open ? 'bg-selected' : ''} ${display.tone === 'attention' ? attention : 'text-textcolor'}`}
        {@const ring = open ? 'ring-selected' : `${variant === 'slot' ? 'ring-bgcolor' : 'ring-darkbg'} group-hover:ring-selected`}
        {#if variant === 'slot'}
            <div class="flex w-full justify-center border-t border-t-selected py-2">
                <button bind:this={button} type="button" class={control} aria-label={name} title={name} aria-haspopup="dialog" aria-expanded={open} onclick={toggle} data-sync-status={display.tone}>
                    <Icon /><SyncStatusDot tone={display.tone} class="right-[11px] top-[7px] {ring}" />
                </button>
            </div>
        {:else}
            <button bind:this={button} type="button" class={control} aria-label={name} title={name} aria-haspopup="dialog" aria-expanded={open} onclick={toggle} data-sync-status={display.tone}>
                <Icon /><SyncStatusDot tone={display.tone} class="right-1.5 top-[7px] {ring}" />
            </button>
        {/if}
    {/if}
{/if}
