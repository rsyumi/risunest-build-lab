<script lang="ts">
    import { CheckIcon, CopyIcon } from '@lucide/svelte'
    import { language } from 'src/lang'
    import { diagnosticInfo, formatErrorDetails } from 'src/ts/diagnostics'

    let { message }: { message: string } = $props()
    let open = $state(false)
    let copied = $state('')
    const report = $derived(formatErrorDetails($diagnosticInfo, message))

    async function copy() {
        const text = report
        try {
            await navigator.clipboard.writeText(text)
            copied = text
        } catch {
            const textarea = document.createElement('textarea')
            textarea.value = text
            document.body.appendChild(textarea)
            textarea.select()
            try { if (document.execCommand('copy')) copied = text }
            catch { /* Keep the report selectable when the clipboard is unavailable. */ }
            finally { textarea.remove() }
        }
    }
</script>

<details bind:open class="mt-3 min-w-0 w-full text-sm">
    <summary class="cursor-pointer text-textcolor2">{open ? language.hideErrorDetails : language.showErrorDetails}</summary>
    <div class="relative mt-2">
        <button type="button" class="absolute top-1 right-1 rounded-sm bg-bgcolor p-1 text-textcolor2"
            title={copied === report ? language.risuNest.importDialog.copied : language.copy}
            aria-label={language.copy} onclick={copy}>
            {#if copied === report}<CheckIcon size={14} />{:else}<CopyIcon size={14} />{/if}
        </button>
        <pre class="max-h-48 overflow-auto rounded-md border border-darkborderc bg-bgcolor p-2 pr-8 text-xs whitespace-pre-wrap break-all text-textcolor">{report}</pre>
    </div>
</details>
