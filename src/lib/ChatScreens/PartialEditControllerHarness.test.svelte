<script lang="ts">
    import PartialEditController from './PartialEditController.svelte'

    type SaveDetail = {
        newData: string
        target: 'original' | 'translation'
        translationKey?: string
    }

    let {
        getTranslationEditContext,
        onSave,
    }: {
        getTranslationEditContext: () => Promise<{ key: string; data: string } | null>
        onSave?: (detail: SaveDetail & { sourceData: string }) => Promise<boolean>
    } = $props()

    let translatedView = $state(true)
    let bodyRoot = $state<HTMLElement | null>(null)
    let saves: SaveDetail[] = []

    export function setTranslatedView(value: boolean) {
        translatedView = value
    }

    export function getSaves() {
        return [...saves]
    }

    function handleSave(event: CustomEvent<SaveDetail>) {
        saves.push(event.detail)
    }
</script>

<div bind:this={bodyRoot}>
    <p>Shared text</p>
</div>

{#if bodyRoot}
    <PartialEditController
        messageData="Shared text"
        chatIndex={0}
        {bodyRoot}
        blockEditEnabled={true}
        {translatedView}
        {getTranslationEditContext}
        {onSave}
        on:save={handleSave}
    />
{/if}
