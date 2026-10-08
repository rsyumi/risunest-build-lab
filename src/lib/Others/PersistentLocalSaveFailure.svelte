<script lang="ts">
    import { language } from 'src/lang'
    import SettingButton from 'src/lib/Setting/RisuNest/SettingButton.svelte'
    import { persistentLocalSaveFailure, getPersistentDataRuntime } from 'src/ts/storage/persistentDataRuntime.svelte'
    import { localSaveFailureMessage } from 'src/ts/storage/localSaveFailureMessage'

    let saving = $state(false)
    const message = $derived(localSaveFailureMessage($persistentLocalSaveFailure))

    async function retry() {
        if (saving) return
        saving = true
        try { await getPersistentDataRuntime().flushPendingDataLocally('save-failure-retry') }
        catch { /* The failure remains visible until persistence confirms a successful save. */ }
        finally { saving = false }
    }
</script>

{#if $persistentLocalSaveFailure !== null}
    <aside role="alert" aria-live="assertive" class="fixed left-2 right-2 top-2 z-[1000] rounded-lg border border-danger-400 bg-darkbg p-3 text-textcolor shadow-lg sm:left-auto sm:max-w-md" data-local-save-failure>
        <p>{message}</p>
        <SettingButton class="mt-2" busy={saving} onclick={retry}>{language.retry}</SettingButton>
    </aside>
{/if}
