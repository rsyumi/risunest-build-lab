import type { Chat, Message, character } from './database.svelte'
import type { ConversationMutation, PersistentUnitMutation, WorkingSetCommit } from './persistentDataStore'
import { jsonByteLength, MAX_NATIVE_REQUEST_BYTES, PayloadTooLargeError, utf8ByteLength } from './nativePersistenceValue'
import { canonicalClone, canonicalJson, requiresWholeObjectCapture } from './saveCoordinatorHelpers'

/** Message bytes of inserted conversations in one commit of a split save. */
export const CONVERSATION_INSERT_PAGE_BYTES = 16 * 1024 * 1024

type ReplaceRange = Extract<ConversationMutation, { type: 'replace-range' }>
export type ConversationInsertStep = Omit<WorkingSetCommit, 'expectedRevision'> & { conversations: ConversationMutation[] }

export interface ConversationInsertPlan {
    steps: ConversationInsertStep[]
    addedCharacterId?: string
}

// An explicit position marks a conversation the commit creates.
export const createsConversation = (mutation: ConversationMutation): mutation is ReplaceRange & { configuredIndex: number } =>
    mutation.type === 'replace-range' && mutation.configuredIndex !== undefined

export function captureMessagePages(messages: readonly Message[], pageBytes = CONVERSATION_INSERT_PAGE_BYTES, detach = false): { messages: Message[]; bytes: number }[] {
    const pages = [{ messages: [] as Message[], bytes: 0 }]
    for (const [index, message] of messages.entries()) {
        const json = detach ? canonicalJson({ [index]: message }) : undefined
        const bytes = json === undefined ? jsonByteLength(message) + 1
            : utf8ByteLength(json) - JSON.stringify(String(index)).length - 2
        if (bytes > MAX_NATIVE_REQUEST_BYTES - 1024) throw new PayloadTooLargeError('message', bytes)
        const captured = json === undefined ? message : JSON.parse(json)[index] as Message
        let page = pages[pages.length - 1]
        if (page.messages.length > 0 && page.bytes + bytes > pageBytes) {
            page = { messages: [], bytes: 0 }
            pages.push(page)
        }
        page.messages.push(captured)
        page.bytes += bytes
    }
    return pages
}

/**
 * Splits a save whose created conversations make it too large into commits
 * applied in order. The first carries everything except the reorders, the
 * selected character's detail and the created conversations; each created
 * conversation then starts with its first message page and gains one page per
 * commit, and the last commit also carries the reorders, the conversation order
 * of every character that gains a conversation, and the detail.
 * Returns null when the save creates no conversation.
 */
export function planConversationInsertPages(
    commit: WorkingSetCommit,
    pageBytes = CONVERSATION_INSERT_PAGE_BYTES,
): ConversationInsertPlan | null {
    const conversations = [...(commit.conversations ?? [])]
    if (commit.addCharacter) {
        for (const [configuredIndex, chat] of commit.addCharacter.chats.entries()) {
            const { message, ...conversation } = chat
            conversations.push({ type: 'replace-range', characterId: commit.addCharacter.chaId,
                conversationId: chat.id!, start: 0, deleteCount: 0, messages: message,
                conversation, configuredIndex })
        }
    }
    if (!conversations.some(createsConversation)) return null
    const { expectedRevision: _revision, conversations: _conversations, character, ...rest } = commit
    // An order unit names conversations that later steps create, so it applies once they all exist.
    const gaining = new Set(conversations.filter(createsConversation).map((mutation) => mutation.characterId))
    const isGainingOrder = (mutation: PersistentUnitMutation) => {
        const [kind, scope, characterId] = JSON.parse(mutation.key) as string[]
        return kind === 'order' && scope === 'conversations' && gaining.has(characterId)
    }
    const orders = (rest.unitMutations ?? []).filter(isGainingOrder)
    let current: ConversationInsertStep = {
        ...rest,
        ...(commit.addCharacter ? { addCharacter: { ...commit.addCharacter, chats: [] } } : {}),
        ...(orders.length ? { unitMutations: rest.unitMutations!.filter((mutation) => !isGainingOrder(mutation)) } : {}),
        conversations: conversations.filter((mutation) => mutation.type !== 'reorder' && !createsConversation(mutation)),
    }
    const steps = [current]
    let used = 0
    for (const insert of conversations.filter(createsConversation)) {
        const [first, ...rest] = captureMessagePages(insert.messages, pageBytes)
        if (used > 0 && used + first.bytes > pageBytes) {
            current = { conversations: [] }
            steps.push(current)
            used = 0
        }
        current.conversations.push({ ...insert, messages: first.messages })
        used += first.bytes
        let start = insert.start + first.messages.length
        for (const page of rest) {
            current = {
                conversations: [{
                    type: 'replace-range',
                    characterId: insert.characterId,
                    conversationId: insert.conversationId,
                    start,
                    deleteCount: 0,
                    messages: page.messages,
                }],
            }
            steps.push(current)
            used = page.bytes
            start += page.messages.length
        }
    }
    current.conversations.push(...conversations.filter((mutation) => mutation.type === 'reorder'))
    if (orders.length) current.unitMutations = [...(current.unitMutations ?? []), ...orders]
    if (character) current.character = character
    return { steps, addedCharacterId: commit.addCharacter?.chaId }
}

export function cloneConversationByMessage(conversation: Chat): Chat {
    if (requiresWholeObjectCapture(conversation)) return canonicalClone(conversation)
    const { message, ...metadata } = conversation
    return { ...canonicalClone(metadata), message: message.map((value, index) => JSON.parse(canonicalJson({ [index]: value }))[index]) }
}

export function estimateCharacterBytes(character: character): number {
    let bytes = 2
    for (const [key, value] of Object.entries(character)) {
        if (key === 'chats') continue
        bytes += jsonByteLength(key) + jsonByteLength(value) + 2
    }
    for (const chat of character.chats) {
        for (const [key, value] of Object.entries(chat)) {
            bytes += jsonByteLength(key) + 2
            if (key === 'message') {
                for (const message of chat.message) bytes += jsonByteLength(message) + 1
            } else bytes += jsonByteLength(value)
        }
    }
    return bytes
}
