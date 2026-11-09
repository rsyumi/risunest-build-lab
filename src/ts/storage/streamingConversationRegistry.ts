import type { Chat } from './database.svelte'

const streaming = new Map<string, Chat['activeStreamingDisplayOptimizationMode']>()

export function setStreamingConversation(id: string, mode: Chat['activeStreamingDisplayOptimizationMode']): void {
    streaming.set(id, mode)
}

export function clearStreamingConversation(id: string): void {
    streaming.delete(id)
}

export function isConversationStreaming(id?: string): boolean {
    return id !== undefined && streaming.has(id)
}

export function restoreStreamingConversationState(chat: Chat): void {
    if (chat.id && streaming.has(chat.id)) {
        chat.isStreaming = true
        chat.activeStreamingDisplayOptimizationMode = streaming.get(chat.id)
    }
}
