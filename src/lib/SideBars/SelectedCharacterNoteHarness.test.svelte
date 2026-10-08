<script lang="ts">
    import { DBState, selectedCharID } from 'src/ts/stores.svelte'
    import SelectedConversationEditor from './SelectedConversationEditor.svelte'
    import SelectedCharacterNoteReader from './SelectedCharacterNoteReader.test.svelte'
</script>

<!-- Mirrors the sidebar character tab that mounts CharConfig. -->
{#if $selectedCharID >= 0 && DBState.db.characters[$selectedCharID]}
    <SelectedConversationEditor>
        <SelectedCharacterNoteReader />
    </SelectedConversationEditor>
{/if}
