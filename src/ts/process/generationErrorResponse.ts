import type { ActiveConversationSession } from '../storage/activeConversationSession'
import type { WindowedConversationMutationController } from '../storage/activeWorkingSet.svelte'
import type { Chat, Message } from '../storage/database.svelte'

export interface GenerationErrorResponseOptions {
    windowedController?: WindowedConversationMutationController | null
    session: ActiveConversationSession | null
    getCurrentSession(): ActiveConversationSession | null
    characterId: string
    chat: Chat
    getCurrentChat(): Chat | null | undefined
    isOwnerCurrent?(): boolean
    suffix: string
    appendMessage: Message
}

export function applyGenerationErrorResponse(
    options: GenerationErrorResponseOptions,
): boolean {
    if (!isOwnerCurrent(options)) return false

    const controller = options.windowedController
    if (controller) {
        if (!controller.isCurrent() || controller.chat !== options.chat) return false
        const messages = options.chat.message
        const last = messages.at(-1)
        return last?.role === 'char'
            ? controller.applyRange(messages.length - 1, 1, [{ ...last, data: last.data + options.suffix }], 'edit')
            : controller.applyRange(messages.length, 0, [options.appendMessage], 'append')
    }

    const session = options.session
    if (session === null) return applyFullArrayFallback(options)
    if (
        options.getCurrentSession() !== session ||
        !session.matchesConversation(options.characterId, options.chat)
    ) return false

    const pin = session.acquirePin('transaction')
    try {
        const expectedVersion = session.version
        const latest = session.readLatest(1)
        if (
            !isOwnerCurrent(options) ||
            options.getCurrentSession() !== session ||
            session.version !== expectedVersion
        ) return false

        const lastMessage = latest.messages[0]
        const lastLocator = latest.locators[0]
        if (lastMessage?.role === 'char') {
            if (!lastLocator || !session.ownsMessageLocator(lastLocator)) return false
            session.edit(lastLocator, {
                ...lastMessage,
                data: lastMessage.data + options.suffix,
            })
        } else {
            session.append(options.appendMessage)
        }
        return true
    } finally {
        pin.release()
    }
}

function applyFullArrayFallback(options: GenerationErrorResponseOptions): boolean {
    if (options.getCurrentSession() !== null || !isOwnerCurrent(options)) return false
    const messages = options.chat.message
    const lastMessage = messages[messages.length - 1]
    if (lastMessage?.role === 'char') {
        lastMessage.data += options.suffix
    } else {
        messages.push(options.appendMessage)
    }
    return true
}

function isOwnerCurrent(options: GenerationErrorResponseOptions): boolean {
    return (options.isOwnerCurrent?.() ?? true) && options.getCurrentChat() === options.chat
}
