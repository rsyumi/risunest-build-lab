<script lang="ts">
    import { markTextEditorPopupContent } from 'src/ts/gui/textEditorPopup.svelte'
    import TextAreaInput from './TextAreaInput.svelte'

    let { value = $bindable('Field text') }: { value?: string } = $props()

    markTextEditorPopupContent()
</script>

<TextAreaInput bind:value />
