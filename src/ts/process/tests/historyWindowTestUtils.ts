import type { Chat, Message } from '../../storage/database.svelte'
import type { HistoryWindowController } from '../historyWindowController'
import { attachHistoryWindow } from '../historyWindowIndex'

export function historyMessages(count: number, from = 0): Message[] {
    return Array.from({ length: count }, (_, offset) => {
        const index = from + offset
        return {
            role: index % 2 === 0 ? 'user' : 'char',
            data: `m${index}`,
            chatId: `id-${index}`,
            time: 1_000_000 + index * 1000,
        } as Message
    })
}

/** A window over the end of `store` whose writes land in `store` by absolute index. */
export function createStoreHistoryWindow(
    store: Message[],
    start: number,
    metadata: Partial<Omit<Chat, 'message'>> = {},
) {
    const chat = {
        id: 'conversation-1',
        fmIndex: -1,
        ...metadata,
        message: structuredClone(store.slice(start)),
    } as Chat
    attachHistoryWindow(chat, start)
    const controller = {
        chat,
        absoluteStartIndex: start,
        isCurrent: () => true,
        applyRange(localStart: number, deleteCount: number, messages: Message[]) {
            store.splice(start + localStart, deleteCount, ...structuredClone(messages))
            chat.message.splice(localStart, deleteCount, ...messages)
            return true
        },
        reconcileMetadata: () => true,
        release: () => undefined,
    } as unknown as HistoryWindowController
    return { chat, controller }
}
