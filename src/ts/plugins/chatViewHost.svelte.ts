import { untrack } from 'svelte'
import { v4 } from 'uuid'
import { DBState, selIdState } from '../stores.svelte'
import { createChatViewEvents, type ChatViewConversation } from './chatViewEvents'

function readSelectedConversation(): ChatViewConversation {
    const characterIndex = selIdState.selId
    const character = DBState.db.characters?.[characterIndex]
    if (!character) return { characterId: null, conversationId: null, characterIndex: -1, chatIndex: -1 }
    const chatIndex = character.chatPage ?? 0
    const conversation = character.chats?.[chatIndex]
    return {
        characterId: character.chaId ?? null,
        conversationId: conversation?.id ?? null,
        characterIndex,
        chatIndex: conversation ? chatIndex : -1,
    }
}

export const chatViewEvents = createChatViewEvents({
    readConversation: readSelectedConversation,
    watchConversation: (onChange) =>
        $effect.root(() => {
            $effect(() => {
                readSelectedConversation()
                untrack(onChange)
            })
        }),
    requestFrame: (callback) => {
        requestAnimationFrame(() => callback())
    },
    createId: () => v4(),
})
