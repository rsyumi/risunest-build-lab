<script lang="ts">
    import { language } from 'src/lang'
    import SettingButton from './SettingButton.svelte'

    let busy = $state(false)

    async function refresh(): Promise<void> {
        if (busy) return
        busy = true
        try {
            const { refreshWebView } = await import('src/ts/storage/webViewRefresh')
            await refreshWebView()
        } catch (error) {
            const { alertError } = await import('src/ts/alert')
            busy = false
            const blocked = error instanceof Error && ['webview-refresh-busy', 'library-file-operation-busy'].includes(error.message)
            alertError(blocked ? language.risuNest.platform.refreshBusy : language.risuNest.platform.refreshFailed)
        }
    }
</script>

<div class="flex justify-end px-4 py-3">
    <SettingButton variant="secondary" size="sm" {busy} onclick={refresh}>{language.risuNest.platform.refreshScreen}</SettingButton>
</div>
