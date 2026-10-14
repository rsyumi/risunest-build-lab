<script lang="ts">
    let { name, onOpenUnusedImages }: { name: string, onOpenUnusedImages?: () => void } = $props()
</script>

<div data-child={name}>
    {#if onOpenUnusedImages}<button type="button" data-open-unused-images aria-label="unused images" onclick={onOpenUnusedImages}></button>{/if}
</div>
