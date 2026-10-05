import { DBState } from './stores.svelte'
import { alertError } from './alert'
import type { character } from './storage/database.svelte'
import { mutatePersistentCharacterDetail } from './storage/persistentDataRuntime.svelte'
import {
    captureChatBindingTarget,
    conversationMutationBlockedByGeneration,
    updateChatBinding,
} from './chatBindings.svelte'

// Sidebar toggle edits of the selected chat and character. A windowed chat
// cannot be saved after a direct write to its metadata or character detail,
// so these go through the explicit binding and detail mutations instead.

function reportFailure(error: unknown) {
    alertError(error instanceof Error ? error : String(error))
}

export async function setToggleValue(key: string, value: string): Promise<void> {
    try {
        const conversation = captureChatBindingTarget()?.conversation
        const local = !!conversation?.useLocallySetGlobalVariables
        if (conversation && (local || conversation.GLGlobalVariables?.[key] !== undefined)) {
            if (conversationMutationBlockedByGeneration(conversation.id)) return
            const values = { ...conversation.GLGlobalVariables }
            if (local) values[key] = value
            else delete values[key]
            await updateChatBinding(conversation, { GLGlobalVariables: values })
            if (local) return
        }
        DBState.db.globalChatVariables[key] = value
    } catch (error) {
        reportFailure(error)
    }
}

export async function removeLocalToggleValue(key: string): Promise<void> {
    try {
        const conversation = captureChatBindingTarget()?.conversation
        if (conversation?.GLGlobalVariables?.[key] === undefined) return
        if (conversationMutationBlockedByGeneration(conversation.id)) return
        const values = { ...conversation.GLGlobalVariables }
        delete values[key]
        await updateChatBinding(conversation, { GLGlobalVariables: values })
    } catch (error) {
        reportFailure(error)
    }
}

export async function setLocalToggleMode(enabled: boolean): Promise<void> {
    try {
        const conversation = captureChatBindingTarget()?.conversation
        if (!conversation || conversationMutationBlockedByGeneration(conversation.id)) return
        await updateChatBinding(conversation, { useLocallySetGlobalVariables: enabled })
    } catch (error) {
        reportFailure(error)
    }
}

export async function setCharacterMemory(owner: character, enabled: boolean): Promise<void> {
    try {
        const saved = await mutatePersistentCharacterDetail(owner.chaId, 'toggle-character-memory', ({ character }) => {
            character.supaMemory = enabled
        })
        // A character outside the persistent store keeps the in-memory write.
        if (!saved) owner.supaMemory = enabled
    } catch (error) {
        reportFailure(error)
    }
}
