<script lang="ts">
    import { CheckIcon, PlusIcon } from '@lucide/svelte'
    import { DBState } from 'src/ts/stores.svelte'
    import { language } from 'src/lang'
    import { alertActionConfirm, alertConfirm, alertError, alertInput, alertToast } from 'src/ts/alert'
    import {
        authorNotePresetLinkPatch,
        createAuthorNotePreset,
        effectiveAuthorNote,
        resolveAuthorNoteSource,
        type AuthorNotePreset,
    } from 'src/ts/authorNotePresets'
    import {
        captureChatBindingTarget,
        chatBindingBlockedByGeneration,
        saveChatBinding,
        updateChatBinding,
    } from 'src/ts/chatBindings.svelte'
    import type { ConversationBindingPatch } from 'src/ts/storage/conversationBinding'
    import { flushPendingData, getPersistentDataRuntime } from 'src/ts/storage/persistentDataRuntime.svelte'
    import PresetListPopup from '../UI/PresetListPopup.svelte'

    interface Props {
        close: () => void
    }
    let { close }: Props = $props()

    const usageListLimit = 10
    let target = captureChatBindingTarget()
    let source = $derived(target ? resolveAuthorNoteSource(DBState.db.authorNotePresets, target.conversation) : null)
    // A chat whose preset was deleted reads an empty note it now owns.
    let storedInChat = $derived(source !== null && source.kind !== 'preset')

    // Read back after the assignment: `??=` would hand out the raw array instead of the reactive proxy.
    const list = () => {
        DBState.db.authorNotePresets ??= []
        return DBState.db.authorNotePresets
    }

    async function write(patch: ConversationBindingPatch, current: NonNullable<typeof target>) {
        try {
            await updateChatBinding(current.conversation, patch)
            await saveChatBinding()
        } catch (error) {
            alertError(error)
            return
        }
        close()
    }
    async function select(preset: AuthorNotePreset) {
        if (chatBindingBlockedByGeneration() || !target?.isCurrent()) return
        const current = target
        const from = resolveAuthorNoteSource(DBState.db.authorNotePresets, current.conversation)
        if (from.kind === 'preset' && from.preset.id === preset.id) return close()
        const note = effectiveAuthorNote(DBState.db.authorNotePresets, current.conversation)
        if (from.kind === 'chat' && note.trim() && note !== preset.text) {
            if (!(await alertConfirm(language.selectAuthorNotePresetConfirm))) return
            if (!current.isCurrent() || !list().includes(preset)) return
        }
        await write(authorNotePresetLinkPatch(preset), current)
    }
    async function storeInChat() {
        if (chatBindingBlockedByGeneration() || !target?.isCurrent()) return
        if (!target.conversation.authorNotePreset) return close()
        const note = effectiveAuthorNote(DBState.db.authorNotePresets, target.conversation)
        await write({ note, authorNotePreset: undefined }, target)
    }
    async function saveNew() {
        if (chatBindingBlockedByGeneration() || !target?.isCurrent()) return
        const current = target
        const name = (await alertInput(language.authorNotePresetNamePrompt))?.trim()
        if (!name || !current.isCurrent()) return
        const preset = createAuthorNotePreset(name, effectiveAuthorNote(DBState.db.authorNotePresets, current.conversation))
        list().push(preset)
        alertToast(language.presetSaved(name))
        await write(authorNotePresetLinkPatch(preset), current)
    }
    async function confirmDelete(preset: AuthorNotePreset) {
        let usage
        try {
            await flushPendingData('author-note-preset-delete')
            usage = (await getPersistentDataRuntime().store.queryAuthorNotePresetUsage?.(preset.id)) ?? []
        } catch (error) {
            alertError(error)
            return false
        }
        const title = language.deleteAuthorNotePresetConfirm(preset.name)
        if (!usage.length) return alertConfirm(title)
        const lines = usage.slice(0, usageListLimit).map((item) => `${item.characterName} · ${item.conversationName}`)
        if (usage.length > usageListLimit) lines.push(language.authorNotePresetUsageMore(usage.length - usageListLimit))
        return alertActionConfirm({
            title,
            description: `${language.deleteUsedAuthorNotePresetDescription}\n\n${lines.join('\n')}`,
            actionLabel: language.deletePreset,
            cancelLabel: language.cancel,
        })
    }
</script>

<PresetListPopup
    title={language.authorNotePresets}
    {close}
    {list}
    presets={DBState.db.authorNotePresets}
    empty={language.authorNotePresetsEmpty}
    rowTitle={language.apply}
    selected={(preset) => source?.kind === 'preset' && source.preset.id === preset.id}
    apply={select}
    copy={(preset, name) => createAuthorNotePreset(name, preset.text)}
    {confirmDelete}
>
    {#snippet leading(guarded)}
        <button
            class="w-full border border-darkborderc rounded-md px-3 py-2 text-left transition-colors hover:bg-selected flex items-center gap-2"
            aria-current={storedInChat ? 'true' : undefined}
            onclick={() => guarded(storeInChat)}
        >
            <span class="shrink-0 w-4 text-textcolor">
                {#if storedInChat}<CheckIcon size={16} />{/if}
            </span>
            <span class="min-w-0 flex-1 truncate {storedInChat ? 'font-medium' : ''}">{language.authorNoteStoredInChat}</span>
        </button>
    {/snippet}
    {#snippet footer(guarded, action)}
        <button class="{action} w-full" onclick={() => guarded(saveNew)}>
            <PlusIcon size={16} class="shrink-0" /><span class="truncate">{language.saveNewPreset}</span>
        </button>
    {/snippet}
</PresetListPopup>
