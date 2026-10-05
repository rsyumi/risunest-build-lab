<script lang="ts">
    import { language } from 'src/lang'
    import SettingButton from 'src/lib/Setting/RisuNest/SettingButton.svelte'
    import { persistentLocalSaveFailure, getPersistentDataRuntime } from 'src/ts/storage/persistentDataRuntime.svelte'

    let saving = $state(false)
    const copy = $derived(language.risuNest.localSaveFailure)
    const error = $derived($persistentLocalSaveFailure && typeof $persistentLocalSaveFailure === 'object'
        ? $persistentLocalSaveFailure as { code?: unknown; area?: unknown; name?: unknown } : null)
    const area = $derived(typeof error?.area === 'string' ? ({
        root: copy.settings, character: language.character, conversation: copy.conversation,
        presets: copy.presets, 'plugin storage': copy.plugins, 'asset aliases': copy.assets,
    } as Record<string, string>)[error.area] ?? copy.data : copy.data)
    const message = $derived(error?.code === 'unsaveable-value'
        ? copy.invalid.replace('{0}', area)
        : error?.code === 'payload-too-large' ? copy.tooLarge
        : error?.name === 'QuotaExceededError' ? copy.storage
        : error?.name === 'WindowedConversationSaveError' ? copy.currentChat : copy.failed)

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
