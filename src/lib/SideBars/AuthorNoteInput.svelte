<script lang="ts">
    import { ChevronDownIcon, FolderHeartIcon } from '@lucide/svelte'
    import { DBState, selectedCharID } from 'src/ts/stores.svelte'
    import { language } from 'src/lang'
    import { effectiveAuthorNote, linkedAuthorNotePreset, writeAuthorNote } from 'src/ts/authorNotePresets'
    import { getAuthorNoteDefaultText } from 'src/ts/util'
    import TextAreaInput from '../UI/GUI/TextAreaInput.svelte'
    import AuthorNotePresetPopup from './AuthorNotePresetPopup.svelte'

    let presetsOpen = $state(false)
    let chat = $derived(
        DBState.db.characters[$selectedCharID]?.chats[DBState.db.characters[$selectedCharID]?.chatPage],
    )
    let preset = $derived(chat ? linkedAuthorNotePreset(DBState.db.authorNotePresets, chat) : null)
    const write = (value: string) => {
        if (chat) writeAuthorNote(DBState.db.authorNotePresets, chat, value)
    }
</script>

<button
    class="mt-2 inline-flex items-center gap-1.5 min-h-10 px-3 rounded-md border border-darkborderc bg-darkbutton text-sm text-textcolor transition-colors hover:bg-selected"
    title={language.authorNotePresets}
    aria-haspopup="dialog"
    onclick={() => {
        presetsOpen = true
    }}
>
    <FolderHeartIcon size={16} class="shrink-0" />
    <span class="min-w-0 flex-1 truncate text-left">{preset?.name ?? language.authorNoteStoredInChat}</span>
    <ChevronDownIcon size={16} class="shrink-0 text-textcolor2" />
</button>
<TextAreaInput
    margin="both"
    autocomplete="off"
    bind:value={() => (chat ? effectiveAuthorNote(DBState.db.authorNotePresets, chat) : ''), write}
    highlight
    placeholder={getAuthorNoteDefaultText()}
/>
{#if presetsOpen}
    <AuthorNotePresetPopup
        close={() => {
            presetsOpen = false
        }}
    />
{/if}
