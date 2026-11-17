<script lang="ts">
    import DeferredMarkdown from './DeferredMarkdown.svelte'

    let data = $state('first')
    let controller = $state(new AbortController())
    let root = $state<HTMLElement>()
    export function refresh(value = data) {
        controller.abort()
        controller = new AbortController()
        data = value
    }

</script>

<div bind:this={root}>
    <DeferredMarkdown {data} {root} signal={controller.signal} preservePendingContent />
</div>
