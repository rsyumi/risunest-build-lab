<script lang="ts">
    import { language } from 'src/lang'
    import { isTauri, isTauriAndroid, isTauriIOS } from 'src/ts/platform'
    import { cleanupNeedsWebViewUpdate, requestAppCleanup } from 'src/ts/storage/appCleanup'
    import SettingGroup from './SettingGroup.svelte'
    import SettingRow from './SettingRow.svelte'
    import SettingToggle from './SettingToggle.svelte'
    import SettingButton from './SettingButton.svelte'

    let prepareRemoval = $state(false)
    let busy = $state(false)
    let error = $state('')
    let confirming = $state(false)
    const text = $derived(language.risuNest.cleanup)
    async function reset() {
        if (busy) return
        busy = true
        error = ''
        try {
            const requested = await requestAppCleanup({
                native: isTauri,
                desktop: !isTauriAndroid && !isTauriIOS,
                prepareRemoval,
                confirm: async () => confirming,
            })
            if (!requested) busy = false
        } catch (cause) {
            error = cleanupNeedsWebViewUpdate(cause) ? text.webViewUpdateRequired : text.failed
            busy = false
        }
    }
</script>

{#if isTauri}
    <SettingGroup title={text.title} id="risunest-reset" description={text.scope}>
        {#if !isTauriAndroid && !isTauriIOS}
            <SettingRow label={text.prepareRemoval} help={text.removalHelp} inline>
                <SettingToggle label={text.prepareRemoval} bind:checked={prepareRemoval} disabled={busy || confirming} />
            </SettingRow>
        {/if}
        <SettingRow help={text.resetHelp}>
            {#snippet below()}
                {#if confirming}
                    <p class="mt-1.5 text-sm">{prepareRemoval ? text.confirmRemoval : text.confirmReset}</p>
                {/if}
                {#if error}
                    <p role="alert" class="mt-1.5 text-sm text-danger-400">{error}</p>
                {/if}
            {/snippet}
            {#if confirming}
                <SettingButton variant="danger" busy={busy} onclick={() => void reset()}>{prepareRemoval ? text.deleteAndExit : text.title}</SettingButton>
                <SettingButton variant="secondary" disabled={busy} onclick={() => { confirming = false; error = '' }}>{text.cancel}</SettingButton>
            {:else}
                <SettingButton variant="danger" onclick={() => { confirming = true }}>{prepareRemoval ? text.deleteAndExit : text.title}</SettingButton>
            {/if}
        </SettingRow>
    </SettingGroup>
{/if}
