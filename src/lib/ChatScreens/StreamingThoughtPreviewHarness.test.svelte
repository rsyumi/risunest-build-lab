<script lang="ts">
    import { untrack } from 'svelte'
    import StreamingThoughtPreview from './StreamingThoughtPreview.svelte'
    import { getStreamingThoughtPreview } from '../../ts/parser/streamingThoughtPreview'

    let { initialSource }: { initialSource: string } = $props()
    let source = $state(untrack(() => initialSource))
    const preview = $derived(getStreamingThoughtPreview(source))

    export function setSource(value: string) {
        source = value
    }
</script>

{#if preview}
    <StreamingThoughtPreview {preview} mode="collapsed" {source} />
{/if}
