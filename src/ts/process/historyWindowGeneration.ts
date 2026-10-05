import type { ActiveConversationSession } from '../storage/activeConversationSession'
import type { Chat } from '../storage/database.svelte'
import {
    CONVERSATION_RANGE_MAX_LIMIT,
    type ConversationMessageMetadata,
    type PersistentRevisionLease,
} from '../storage/persistentDataStore'
import type { WindowedConversationPersistenceAuthority } from '../storage/saveCoordinator'
import {
    planHistoryWindowHypaV3,
    selectHistoryWindow,
    type HistoryWindowHypaPlan,
    type HistoryWindowReader,
    type HistoryWindowSelection,
} from './historyWindowSelection'

export type HistoryWindowMemoryMode = 'none' | 'hypaV3'

type MessageMetadata = Pick<ConversationMessageMetadata, 'chatId' | 'disabled'>

export interface HistoryWindowAdmissionInput {
    requested: boolean
    enabled: boolean
    maxContext: number
    supaMemory: boolean
    supaModelType: string
    hanuraiEnable: boolean
    hypav2: boolean
    hypaV3: boolean
}

/**
 * The long-term memory mode a windowed send runs with, or null when the send
 * keeps the whole conversation. The memory branches mirror the generation
 * memory dispatch.
 */
export function admitHistoryWindow(input: HistoryWindowAdmissionInput): HistoryWindowMemoryMode | null {
    if (!input.requested || !input.enabled || !(input.maxContext > 0)) return null
    const memoryActive = input.supaMemory
        && (input.supaModelType !== 'none' || input.hanuraiEnable || input.hypav2 || input.hypaV3)
    if (!memoryActive) return 'none'
    if (input.hanuraiEnable || input.hypav2 || !input.hypaV3) return null
    return 'hypaV3'
}

function conversationQuery(authority: WindowedConversationPersistenceAuthority, startIndex: number, limit: number) {
    return {
        characterId: authority.characterId,
        conversationId: authority.conversationId,
        startIndex,
        limit,
    }
}

export function createStoreHistoryWindowReader(
    lease: PersistentRevisionLease,
    authority: WindowedConversationPersistenceAuthority,
): HistoryWindowReader {
    return {
        totalMessages: authority.totalMessages,
        async read(startIndex, limit) {
            const page = await lease.readConversationWindow(conversationQuery(authority, startIndex, limit))
            if (
                !page
                || page.revision !== authority.storeRevision
                || page.value.startIndex !== startIndex
                || page.value.endIndex !== startIndex + limit
                || page.value.totalMessages !== authority.totalMessages
                || page.value.messages.length !== limit
            ) throw new Error('History window page is incomplete')
            return page.value.messages
        },
    }
}

/** Message ids and disabled flags of the whole conversation, without keeping bodies. */
export async function readStoreMessageMetadata(
    lease: PersistentRevisionLease,
    authority: WindowedConversationPersistenceAuthority,
    assertCurrent: () => void,
): Promise<MessageMetadata[] | null> {
    if (!lease.readConversationMessageMetadataWindow) return null
    const rows: MessageMetadata[] = []
    for (let startIndex = 0; startIndex < authority.totalMessages;) {
        const limit = Math.min(CONVERSATION_RANGE_MAX_LIMIT, authority.totalMessages - startIndex)
        const page = await lease.readConversationMessageMetadataWindow({
            ...conversationQuery(authority, startIndex, limit),
            skipParserWork: true,
        })
        assertCurrent()
        if (
            !page
            || page.revision !== authority.storeRevision
            || page.value.startIndex !== startIndex
            || page.value.totalMessages !== authority.totalMessages
            || page.value.messages.length === 0
        ) throw new Error('History window metadata page is incomplete')
        for (const message of page.value.messages) {
            rows.push({ chatId: message.chatId, disabled: message.disabled })
        }
        startIndex = page.value.endIndex
    }
    return rows
}

export function createSessionHistoryWindowReader(session: ActiveConversationSession): HistoryWindowReader {
    return {
        totalMessages: session.totalMessages,
        read: async (startIndex, limit) => session.readRange(startIndex, limit).messages,
    }
}

export interface HistoryWindowPreparationInput {
    reader: HistoryWindowReader
    memory: HistoryWindowMemoryMode
    conversation: Omit<Chat, 'message'>
    /** Whole-conversation message metadata, or null when the store cannot read it. */
    readMetadata(): Promise<readonly MessageMetadata[] | null>
    preserveOrphanedMemory: boolean
    queryChatCount: number
    tokenBudget: number
    countTokens(text: string): Promise<number>
    assertCurrent(): void
}

export interface HistoryWindowPreparation {
    selection: HistoryWindowSelection
    hypaPlan: HistoryWindowHypaPlan | null
}

export async function prepareHistoryWindow(
    input: HistoryWindowPreparationInput,
): Promise<HistoryWindowPreparation> {
    let hypaPlan: HistoryWindowHypaPlan | null = null
    let extendTo: number | null = null
    if (input.memory === 'hypaV3') {
        const metadata = await input.readMetadata()
        input.assertCurrent()
        if (metadata) {
            hypaPlan = planHistoryWindowHypaV3(input.conversation, metadata, input.preserveOrphanedMemory)
            extendTo = hypaPlan.extendTo
        } else {
            extendTo = 0
        }
    }
    const selection = await selectHistoryWindow({
        reader: input.reader,
        tokenBudget: input.tokenBudget,
        countTokens: input.countTokens,
        minimumMessages: input.memory === 'hypaV3' ? Math.max(1, input.queryChatCount) : 1,
        extendTo,
        assertCurrent: input.assertCurrent,
    })
    if (input.memory === 'hypaV3' && !hypaPlan) {
        // Without a metadata reader the window holds the whole conversation.
        hypaPlan = planHistoryWindowHypaV3(
            input.conversation,
            selection.messages,
            input.preserveOrphanedMemory,
        )
    }
    return { selection, hypaPlan }
}
