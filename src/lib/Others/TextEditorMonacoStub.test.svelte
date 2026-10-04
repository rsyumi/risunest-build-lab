<script lang="ts">
    import { onMount } from 'svelte'

    interface Props {
        value: string
        language: string
        onchange: (value: string) => void
        onsave: () => void
    }

    let { value = $bindable(), language, onchange, onsave }: Props = $props()
    let host: HTMLDivElement
    let input: HTMLTextAreaElement

    export function focus() {
        input.focus()
    }

    // Like Monaco, a native listener on the editor container stops the keys it handles. An open
    // widget, such as the find box, takes Escape before the editor gives it up.
    onMount(() => {
        const keydown = (event: KeyboardEvent) => {
            const handled = event.key === 'Escape'
                ? host.dataset.widget === 'open'
                : event.key === 'Enter' && event.ctrlKey
            if (!handled) return
            event.preventDefault()
            event.stopPropagation()
            if (event.key === 'Escape') host.dataset.widget = 'closed'
            else onsave()
        }
        host.addEventListener('keydown', keydown)
        return () => host.removeEventListener('keydown', keydown)
    })
</script>

<div bind:this={host} data-monaco-stub data-language={language} data-widget="closed">
    <textarea
        bind:this={input}
        {value}
        oninput={(event) => {
            value = event.currentTarget.value
            onchange(value)
        }}
    ></textarea>
</div>
