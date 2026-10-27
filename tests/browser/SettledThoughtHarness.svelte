<script lang="ts">
    import { untrack } from 'svelte'
    import ChatBody from '../../src/lib/ChatScreens/ChatBody.svelte'
    import { getStreamingThoughtPreview } from '../../src/ts/parser/streamingThoughtPreview'

    let { source, previewInitially, onSettled, onError }: {
        source: string
        previewInitially: boolean
        onSettled: () => void
        onError: (generation: number, error: unknown) => void
    } = $props()
    let preview = $state(untrack(() => previewInitially))
    let bodyRoot = $state<HTMLElement | null>(null)
    export function settle() { preview = false }
</script>

<span class="text chat-width chattext prose minw-0" bind:this={bodyRoot}>
    <ChatBody {bodyRoot} character={null} msgDisplay={source} role="char" translated={false}
        translating={false} retranslate={false} modelShortName=""
        rawStreamingText={source} renderRawStreaming={preview}
        thoughtPreview={preview ? getStreamingThoughtPreview(source) : null}
        streamingThoughtMode={preview ? 'collapsed' : 'off'}
        onCaptureSettled={onSettled} onCaptureError={onError} />
</span>
