import { get } from 'svelte/store'
import { activeRerollConversations } from '../durableReroll'
import { isGenerationRequestPhaseOpen, trackConversationPatch } from '../process/generationRequestPhase'
import { doingChat } from '../process/index.svelte'
import {
    acquireCompleteConversation,
    captureSelectedConversationTarget,
    commitPreparedUnitIntent,
    flushPendingDataLocally,
    getActiveConversationSession,
    getPersistentRevision,
} from '../storage/persistentDataRuntime.svelte'
import { DBState, selectedCharID } from '../stores.svelte'
import { createConversationPatchAccess } from './conversationPatchAccess'

/** Conversation patches against the running app, shared by every plugin. */
export const conversationPatchAccess = createConversationPatchAccess({
    flushPendingData: (reason) => flushPendingDataLocally(reason),
    getPersistentRevision: () => getPersistentRevision(),
    captureSelectedConversationTarget: () => captureSelectedConversationTarget(),
    acquireCompleteConversation: (reason, target) => acquireCompleteConversation(reason, target),
    getActiveConversationSession: () => getActiveConversationSession(),
    getSelectedConversation: () => {
        const character = DBState.db.characters[get(selectedCharID)]
        return character?.chats[character.chatPage ?? 0] ?? null
    },
    isConversationGenerating: (target) => {
        if (get(activeRerollConversations).includes(target.conversationId)) return true
        if (!get(doingChat)) return false
        const selected = captureSelectedConversationTarget()
        return selected?.characterId === target.characterId && selected.conversationId === target.conversationId
    },
    isGenerationRequestPhaseOpen: (target) => isGenerationRequestPhaseOpen(target),
    trackConversationPatch: (target) => trackConversationPatch(target),
    commitPreparedUnitIntent: (reason, prepare) => commitPreparedUnitIntent(reason, prepare),
})
