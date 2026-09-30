<script lang="ts">
    import TextInput from './TextInput.svelte'

    let {
        initial = '',
        hideText = false,
        oninput,
        onchange,
    }: {
        initial?: string
        hideText?: boolean
        oninput?: () => void
        onchange?: () => void
    } = $props()
    // svelte-ignore state_referenced_locally
    let bound = $state(initial)

    export function read(): string {
        return bound
    }

    export function write(next: string): void {
        bound = next
    }
</script>

<TextInput id="bound-input" {hideText} bind:value={bound} {oninput} {onchange} />
<output data-bound>{bound}</output>
