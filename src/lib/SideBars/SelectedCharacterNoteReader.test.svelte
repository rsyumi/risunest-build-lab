<script lang="ts" module>
    export const noteReads: { id: string | undefined; stub: boolean }[] = []
</script>

<script lang="ts">
    import { untrack } from 'svelte'
    import { DBState, selectedCharID } from 'src/ts/stores.svelte'
    import { isWorkingSetCharacterStub } from 'src/ts/storage/workingSetCatalog'

    // CharConfig's token counter effect reads the selected conversation note.
    $effect.pre(() => {
        const chara = DBState.db.characters[$selectedCharID]
        untrack(() => noteReads.push({ id: chara?.chaId, stub: !!chara && isWorkingSetCharacterStub(chara) }))
        const localNote = chara.chats[chara.chatPage].note
        void localNote
    })
</script>

<span>{DBState.db.characters[$selectedCharID].name}</span>
