import { get } from 'svelte/store'
import { getDatabase } from '../storage/database.svelte'
import { peekActiveConversationSession } from '../storage/persistentDataRuntime.svelte'
import { selectedCharID } from '../stores.svelte'
import { requestScriptDisplayRefresh } from './scriptDisplayRefresh'

/**
 * Sets a chat variable of the selected conversation from outside a script run. When the
 * active session owns the conversation the write commits through it, so it is saved and
 * the rows reading the key refresh; otherwise it lands on the conversation directly.
 */
export function setSelectedChatVariable(key: string, value: string): void {
    const character = getDatabase().characters[get(selectedCharID)]
    const chat = character.chats[character.chatPage]
    const session = peekActiveConversationSession()
    if (
        session?.isActive &&
        !session.isTransactionActive &&
        session.materializeCompatibilityArray() === chat.message
    ) {
        requestScriptDisplayRefresh({ keys: session.updateChatVariables(new Map([[key, value]])) })
        return
    }
    chat.scriptstate ??= {}
    chat.scriptstate['$' + key] = value
}
