<script lang="ts">
    import { ArrowLeft, MenuIcon, Settings } from "@lucide/svelte";
    import { language } from "src/lang";
    
    import { DBState } from 'src/ts/stores.svelte';
    import { MobileGUIStack, MobileSearch, selectedCharID, SettingsMenuIndex, MobileSideBar, settingsOpen } from "src/ts/stores.svelte";
    import { clearCharacterSelection } from "../workingSetNavigation";
    import SyncStatusButton from "../SideBars/SyncStatusButton.svelte";

</script>
{#snippet status()}<div class="-mr-2.5 ml-auto flex shrink-0"><SyncStatusButton variant="header" /></div>{/snippet}
<div class="w-full px-4 h-16 border-b border-b-darkborderc bg-darkbg flex justify-start items-center gap-2" data-mobile-header>
    {#if $settingsOpen}
        <button aria-label={language.goback} onclick={() => {
            if ($SettingsMenuIndex > -1) SettingsMenuIndex.set(-1)
            else settingsOpen.set(false)
        }}>
            <ArrowLeft />
        </button>
        <span class="font-bold text-lg truncate">{language.settings}</span>
        {@render status()}
    {:else if $selectedCharID !== -1 && $MobileSideBar > 0}
        <button aria-label={language.goback} onclick={() => {
            MobileSideBar.set(0)
        }}>
            <ArrowLeft />
        </button>
        <span class="font-bold text-lg w-2/3 truncate">{language.menu}</span>
        <div class="flex-1 flex justify-end items-center gap-2">
            <SyncStatusButton variant="header" />
            <button aria-label={language.settings} onclick={() => settingsOpen.set(true)}>
                <Settings />
            </button>
        </div>
    {:else if $selectedCharID !== -1}
        <button aria-label={language.goback} onclick={async () => {
            await clearCharacterSelection()
        }}>
            <ArrowLeft />
        </button>
        <span class="font-bold text-lg w-2/3 truncate">{DBState.db.characters[$selectedCharID].name}</span>
        <div class="flex-1 flex justify-end items-center gap-2">
            <SyncStatusButton variant="header" />
            <button aria-label={language.menu} onclick={() => {
                MobileSideBar.set(1)
            }}>
                <MenuIcon />
            </button>
        </div>
    {:else if $MobileGUIStack === 2 && $SettingsMenuIndex > -1}
        <button aria-label={language.goback} onclick={() => {
            SettingsMenuIndex.set(-1)
        }}>
            <ArrowLeft />
        </button>
        <span class="font-bold text-lg truncate">RisuNest</span>
        {@render status()}
    {:else if $MobileGUIStack === 1}
        <div class="flex items-stretch w-2xl max-w-full min-w-0">
            <input placeholder={language.search + '...'} bind:value={$MobileSearch} class="peer focus:border-textcolor transition-colors outline-hidden text-textcolor p-2 min-w-0 border bg-transparent rounded-md input-text text-xl grow mx-4 border-darkborderc resize-none overflow-y-hidden overflow-x-hidden max-w-full">
        </div>
        {@render status()}
    {:else}
        <span class="font-bold text-lg truncate">RisuNest</span>
        {@render status()}

    {/if}
</div>
