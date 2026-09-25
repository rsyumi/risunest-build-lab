<script lang="ts">
    import { DBState, selectedCharID } from 'src/ts/stores.svelte'
    import type { character } from 'src/ts/storage/database.svelte'
    import SelectedConversationEditor from './SelectedConversationEditor.svelte'
    import TextInput from '../UI/GUI/TextInput.svelte'
    import TextAreaInput from '../UI/GUI/TextAreaInput.svelte'
</script>

<!-- Mirrors the sidebar wrapper and CharConfig's name/description bindings. -->
<SelectedConversationEditor>
    {#if DBState.db.characters[$selectedCharID].type !== 'group'}
        <TextInput size="xl" marginBottom placeholder="Character Name" bind:value={DBState.db.characters[$selectedCharID].name} />
        <TextAreaInput margin="both" autocomplete="off" bind:value={(DBState.db.characters[$selectedCharID] as character).desc}></TextAreaInput>
    {:else}
        <TextInput size="xl" marginBottom placeholder="Group Name" bind:value={DBState.db.characters[$selectedCharID].name} />
    {/if}
</SelectedConversationEditor>
