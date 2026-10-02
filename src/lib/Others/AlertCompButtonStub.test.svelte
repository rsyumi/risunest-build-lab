<script lang="ts">
    import type { Snippet } from 'svelte'

    let {
        children,
        onclick,
        disabled = false,
    }: {
        disabled?: boolean
        children?: Snippet
        onclick?: (event: MouseEvent) => void
    } = $props()
</script>

<button {onclick} {disabled}>
    {@render children?.()}
</button>
