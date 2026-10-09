import type { simpleCharacterArgument } from './parser/parser.svelte'
import type { Message } from './storage/database.svelte'
import type { CapturedChatMessageTarget } from './chatMessageUi'
import type { ConversationViewportRow } from './conversationViewportSource'
import type { BoundedLiveChatParserProjection } from './selectedConversationLiveParserProjection'

/** Published only after the current row's parser context is ready. */
export interface ChatDisplayRefresh {
    message: string
    index?: number
    character?: simpleCharacterArgument | string | null
    totalMessages: number
    parserProjection?: BoundedLiveChatParserProjection
    parserAbortSignal?: AbortSignal
    onDisplaySettled?: () => void
    viewportBinding?: {
        viewportRow: ConversationViewportRow
        viewportSourceToken: string
        captureViewportTarget: () => CapturedChatMessageTarget | null
    }
}

/** Presentation fields do not replace the row or its active editor. */
export interface ChatPresentationRefresh {
    img: string
    name: string
    largePortrait: boolean
    bookmarked: boolean
    role: Message['role']
    messageGenerationInfo: Message['generationInfo']
}
