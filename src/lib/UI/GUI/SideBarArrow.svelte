<script lang="ts">
    import { ArrowLeft, ArrowRight } from "@lucide/svelte";
    import { language } from "src/lang";
    import { externalStorageStrings } from "src/lib/Setting/ExternalStorage/strings";
    import SyncStatusDot from "src/lib/SideBars/SyncStatusDot.svelte";
    import { persistentLocalSaveFailure } from "src/ts/storage/persistentDataRuntime.svelte";
    import { syncStatus } from "src/ts/storage/sync/syncStatus";
    import { syncStatusDisplay } from "src/ts/storage/sync/syncStatusView";
    import { DBState, DynamicGUI, MobileGUI, sideBarClosing, sideBarStore } from "src/ts/stores.svelte";

    // The closed sidebar hides the status button, so the tab carries every state but connected.
    const display = $derived((DBState.db.risunestSyncStatusUi ?? true) && $syncStatus && syncStatusDisplay($syncStatus, externalStorageStrings(DBState.db.language), $persistentLocalSaveFailure !== null))
    const dot = $derived(display && display.tone !== 'connected' ? display : null)
</script>

{#if !$MobileGUI}
    {#if $sideBarStore && !$DynamicGUI}
        <button onclick={() => {sideBarClosing.set(true)}} class="absolute top-3 left-0 h-12 w-12 border-r border-b border-t border-transparent  rounded-r-md bg-darkbg hover:border-neutral-200 transition-colors flex items-center justify-center text-textcolor z-20">
            <ArrowLeft />
        </button>
    {:else}
        <button onclick={() => {
            sideBarClosing.set(false);
            sideBarStore.set(true)}} class="absolute top-3 left-0 h-12 w-12 border-r border-b border-t border-borderc rounded-r-md bg-darkbg hover:border-neutral-200 transition-colors flex items-center justify-center text-textcolor z-20 {dot ? '' : 'opacity-50 hover:opacity-90'}"
            aria-label={dot ? `${language.menu}, ${language.risuNest.tabs.sync}: ${dot.label}` : language.menu}>
            <ArrowRight />
            {#if dot}<SyncStatusDot tone={dot.tone} class="right-1.5 top-1.5 ring-darkbg" />{/if}
        </button>
    {/if}
{/if}