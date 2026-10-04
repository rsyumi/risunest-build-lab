import type { Message } from './database.svelte'
import type { ConversationMutation, WorkingSetCommit } from './persistentDataStore'
import { jsonByteLength } from './nativePersistenceValue'

/** Message bytes of inserted conversations in one commit of a split save. */
export const CONVERSATION_INSERT_PAGE_BYTES = 16 * 1024 * 1024

type ReplaceRange = Extract<ConversationMutation, { type: 'replace-range' }>
export type ConversationInsertStep = Omit<WorkingSetCommit, 'expectedRevision'> & { conversations: ConversationMutation[] }

export interface ConversationInsertPlan {
    steps: ConversationInsertStep[]
}

// An explicit position marks a conversation the commit creates.
export const createsConversation = (mutation: ConversationMutation): mutation is ReplaceRange & { configuredIndex: number } =>
    mutation.type === 'replace-range' && mutation.configuredIndex !== undefined

function messagePages(messages: Message[], pageBytes: number): { messages: Message[]; bytes: number }[] {
    const pages = [{ messages: [] as Message[], bytes: 0 }]
    for (const message of messages) {
        const bytes = jsonByteLength(message) + 1
        let page = pages[pages.length - 1]
        if (page.messages.length > 0 && page.bytes + bytes > pageBytes) {
            page = { messages: [], bytes: 0 }
            pages.push(page)
        }
        page.messages.push(message)
        page.bytes += bytes
    }
    return pages
}

/**
 * Splits a save whose created conversations make it too large into commits
 * applied in order. The first carries everything except the reorders, the
 * selected character's detail and the created conversations; each created
 * conversation then starts with its first message page and gains one page per
 * commit, and the last commit also carries the reorders and the detail.
 * Returns null when the save creates no conversation.
 */
export function planConversationInsertPages(
    commit: WorkingSetCommit,
    pageBytes = CONVERSATION_INSERT_PAGE_BYTES,
): ConversationInsertPlan | null {
    const conversations = commit.conversations ?? []
    if (!conversations.some(createsConversation)) return null
    const { expectedRevision: _revision, conversations: _conversations, character, ...rest } = commit
    let current: ConversationInsertStep = {
        ...rest,
        conversations: conversations.filter((mutation) => mutation.type !== 'reorder' && !createsConversation(mutation)),
    }
    const steps = [current]
    let used = 0
    for (const insert of conversations.filter(createsConversation)) {
        const [first, ...rest] = messagePages(insert.messages, pageBytes)
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
    if (character) current.character = character
    return { steps }
}
