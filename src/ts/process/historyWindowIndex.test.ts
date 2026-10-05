import { describe, expect, it } from 'vitest'

import type { Chat, Message } from '../storage/database.svelte'
import {
    attachHistoryWindow,
    detachHistoryWindow,
    getHistoryMessage,
    getHistoryMessageAt,
    getHistoryWindowStart,
    historyLength,
    inheritHistoryWindow,
    sliceHistory,
    toAbsoluteIndex,
    toWindowIndex,
} from './historyWindowIndex'

function chatOf(count: number, first = 0): Chat {
    return {
        id: 'conversation-a',
        name: '',
        note: '',
        localLore: [],
        message: Array.from({ length: count }, (_, index): Message => ({
            role: 'user',
            data: `m${first + index}`,
            chatId: `id-${first + index}`,
        })),
    }
}

const data = (messages: readonly (Message | undefined)[]) => messages.map((message) => message?.data)

describe('history window index helpers', () => {
    it('keeps array semantics for a chat without a window', () => {
        const chat = chatOf(4)
        const full = chat.message

        expect(getHistoryWindowStart(chat)).toBeNull()
        expect(historyLength(chat)).toBe(4)
        expect(toWindowIndex(chat, 2)).toBe(2)
        expect(getHistoryMessage(chat, 1)?.data).toBe('m1')
        for (const index of [-5, -1, 0, 3, 4, 1.7, -1.2, Number.NaN]) {
            expect(getHistoryMessageAt(chat, index)).toBe(full.at(index))
        }
        for (const [start, end] of [[undefined, undefined], [1, 3], [-2, undefined], [-10, 2], [3, 1]]) {
            expect(sliceHistory(chat, start, end)).toEqual(full.slice(start, end))
        }
    })

    it('maps absolute indices onto a window that starts later in the conversation', () => {
        // Conversation of 10 messages, window holds 6..9.
        const chat = chatOf(4, 6)
        attachHistoryWindow(chat, 6)

        expect(getHistoryWindowStart(chat)).toBe(6)
        expect(historyLength(chat)).toBe(10)
        expect(toWindowIndex(chat, 7)).toBe(1)
        expect(toWindowIndex(chat, 2)).toBe(-4)
        expect(toAbsoluteIndex(chat, 0)).toBe(6)
        expect(getHistoryMessage(chat, 9)?.data).toBe('m9')
        expect(getHistoryMessage(chat, 5)).toBeUndefined()
        expect(getHistoryMessage(chat, 10)).toBeUndefined()
    })

    it('reads messages with Array.prototype.at semantics over the whole conversation', () => {
        const chat = chatOf(4, 6)
        attachHistoryWindow(chat, 6)

        expect(getHistoryMessageAt(chat, -1)?.data).toBe('m9')
        expect(getHistoryMessageAt(chat, -4)?.data).toBe('m6')
        expect(getHistoryMessageAt(chat, -5)).toBeUndefined()
        expect(getHistoryMessageAt(chat, 6)?.data).toBe('m6')
        expect(getHistoryMessageAt(chat, 6.9)?.data).toBe('m6')
        expect(getHistoryMessageAt(chat, 0)).toBeUndefined()
        expect(getHistoryMessageAt(chat, Number.NaN)).toBeUndefined()
        expect(getHistoryMessageAt(chat, 10)).toBeUndefined()
    })

    it('slices the whole conversation and returns only the loaded part', () => {
        const chat = chatOf(4, 6)
        attachHistoryWindow(chat, 6)

        expect(data(sliceHistory(chat))).toEqual(['m6', 'm7', 'm8', 'm9'])
        expect(data(sliceHistory(chat, 0, 5))).toEqual([])
        expect(data(sliceHistory(chat, 0, 7))).toEqual(['m6'])
        expect(data(sliceHistory(chat, 7, 9))).toEqual(['m7', 'm8'])
        expect(data(sliceHistory(chat, -2))).toEqual(['m8', 'm9'])
        expect(data(sliceHistory(chat, -6, -3))).toEqual(['m6'])
        expect(data(sliceHistory(chat, 2, -1))).toEqual(['m6', 'm7', 'm8'])
        expect(data(sliceHistory(chat, 9, 7))).toEqual([])
    })

    it('carries the window to a clone and drops it on detach', () => {
        const chat = chatOf(2, 3)
        attachHistoryWindow(chat, 3)
        const clone = structuredClone(chat)

        expect(getHistoryWindowStart(clone)).toBeNull()
        inheritHistoryWindow(chat, clone)
        expect(getHistoryWindowStart(clone)).toBe(3)
        inheritHistoryWindow(chatOf(1), clone)
        expect(getHistoryWindowStart(clone)).toBe(3)

        detachHistoryWindow(clone)
        expect(getHistoryWindowStart(clone)).toBeNull()
        expect(getHistoryWindowStart(chat)).toBe(3)
    })

    it('rejects a negative or fractional start', () => {
        expect(() => attachHistoryWindow(chatOf(1), -1)).toThrow(RangeError)
        expect(() => attachHistoryWindow(chatOf(1), 1.5)).toThrow(RangeError)
    })
})
