import { get_encoding } from '@dqbd/tiktoken'
import type { Chat, Database, Message } from '../../src/ts/storage/database.svelte'

export const tokenizerName = '@dqbd/tiktoken cl100k_base'
const encoder = get_encoding('cl100k_base')
export const countTokens = (text: string) => encoder.encode(text).length

export function syntheticText(tokens: number, prefix = 'synthetic-history'): string {
    let value = prefix + ' alpha'.repeat(Math.max(0, tokens - countTokens(prefix)))
    while (countTokens(value) > tokens) value = value.slice(0, value.lastIndexOf(' alpha'))
    while (countTokens(value) < tokens) value += ' alpha'
    if (countTokens(value) !== tokens) throw new Error('Synthetic token construction did not converge')
    return value
}

export function historyFixture(targetTokens: number, metadataBytes = 0) {
    const messages: Message[] = []
    let remaining = targetTokens
    const full = syntheticText(Math.min(1000, targetTokens))
    while (remaining > 0) {
        const tokens = Math.min(1000, remaining)
        messages.push({ role: messages.length % 2 ? 'char' : 'user',
            data: tokens === 1000 ? full : syntheticText(tokens), chatId: `history-${messages.length}` })
        remaining -= tokens
    }
    const chat = { id: 'conversation', name: 'Synthetic history', note: '', localLore: [], message: messages,
        ...(metadataBytes ? { syntheticMetadata: 'm'.repeat(metadataBytes) } : {}) } as Chat
    const database = { username: 'Synthetic', botPresets: [], characters: [{type: 'character',
        chaId: 'character', name: 'Synthetic', chats: [chat], chatPage: 0}] } as unknown as Database
    return { database, chat, metrics: { tokenizer: tokenizerName, historyTokens: messages.reduce((n, m) => n + countTokens(m.data), 0),
        historyCharacters: messages.reduce((n, m) => n + m.data.length, 0),
        historyUtf8Bytes: messages.reduce((n, m) => n + Buffer.byteLength(m.data), 0),
        messages: messages.length, metadataUtf8Bytes: metadataBytes } }
}

export const bufferedResponse = () => syntheticText(20_000, 'synthetic-response')
export const streamingResponse = () => ('synthetic-response ' + 'alpha beta gamma '.repeat(4000)).slice(0, 51_210)
