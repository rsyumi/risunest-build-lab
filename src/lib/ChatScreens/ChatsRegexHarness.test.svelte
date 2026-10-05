<script lang="ts">
    import type { character, Message } from 'src/ts/storage/database.svelte'
    import ChatsHarness from './ChatsHarness.test.svelte'
    import RegexData from '../SideBars/Scripts/RegexData.svelte'

    let { initialMessages, initialCharacter }: { initialMessages: Message[]; initialCharacter: character } = $props()
    let harness = $state<{ getCurrentCharacter(): character }>()
    const currentCharacter = $derived(harness?.getCurrentCharacter())
</script>

<ChatsHarness bind:this={harness} {initialMessages} {initialCharacter} />
{#if currentCharacter}
    <div data-regex-editor><RegexData bind:value={currentCharacter.customscript[0]} idx={0} /></div>
{/if}
