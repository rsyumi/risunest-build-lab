<script lang="ts">
    import { untrack } from 'svelte'
    import { DBState, selectedCharID } from 'src/ts/stores.svelte'
    import { getModuleToggles } from 'src/ts/process/modules'
    import { parseToggleSyntax } from 'src/ts/util'
    import { isConversationSummaryStub } from 'src/ts/storage/conversationResidency'
    import { createToggleBindingRestorer } from 'src/ts/toggleBindings'
    import { activeRerollConversations, recoverInterruptedReroll } from 'src/ts/durableReroll'
    import {
        acquireCompleteConversation,
        captureSelectedConversationTarget,
        flushPendingData,
    } from 'src/ts/storage/persistentDataRuntime.svelte'
    import { alertError } from 'src/ts/alert'
    let recovering = new Set<string>()
    $effect(() => {
        const character = DBState.db?.characters?.[$selectedCharID]
        const chat = character?.chats?.[character.chatPage]
        const active = $activeRerollConversations
        if (!chat?.rerollRecovery || active.includes(chat.id) || recovering.has(chat.id)) return
        const target = captureSelectedConversationTarget()
        if (!target) return
        recovering.add(chat.id)
        void (async () => {
            let lease: Awaited<ReturnType<typeof acquireCompleteConversation>> | undefined
            try {
                lease = await acquireCompleteConversation('recover-reroll', target)
                const selected = DBState.db.characters[$selectedCharID]
                const current = selected?.chats[selected.chatPage]
                if (selected?.chaId !== character.chaId || current?.id !== chat.id) return
                recoverInterruptedReroll(current, lease.session)
                await flushPendingData('recover-reroll')
            } catch (error) {
                alertError(error)
            } finally {
                lease?.release()
                recovering.delete(chat.id)
            }
        })()
    })
    const toggleBinding = createToggleBindingRestorer()
    $effect(() => {
        const db = DBState.db
        const character = db?.characters?.[$selectedCharID]
        const chat = character?.chats?.[character.chatPage]
        if (!chat || isConversationSummaryStub(chat)) {
            toggleBinding.reset()
            return
        }
        const definitions = `${db.customPromptTemplateToggle ?? ''}\n${getModuleToggles()}\n${character.type === 'character' ? (character.customModuleToggle ?? '') : ''}`
        const disabled = db.disableToggleBinding
        untrack(() => {
            const keys = parseToggleSyntax(definitions).map((toggle) => `toggle_${toggle.key}`)
            toggleBinding.restore(
                [db, character.chaId, chat.id, disabled],
                db.globalChatVariables,
                disabled ? undefined : chat.savedToggleValues,
                keys,
            )
        })
    })
</script>
