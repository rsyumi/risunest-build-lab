import { describe, expect, it } from 'vitest'

import type { Message } from '../storage/database.svelte'
import { createCompatibilityConversationHistorySnapshot } from '../storage/conversationHistoryOperation'
import { WindowedConversationHistoryOperation } from './historyWindowHistory'
import { iteratePromptHistory, selectPromptHistory } from './promptHistory'

function windowOf(count: number, start: number, overrides: Record<number, Partial<Message>> = {}) {
    const messages = Array.from({ length: count }, (_, index): Message => ({
        role: 'user',
        data: `m${start + index}`,
        chatId: `id-${start + index}`,
        ...overrides[start + index],
    }))
    const inner = createCompatibilityConversationHistorySnapshot({
        characterId: 'character-a',
        conversationId: 'conversation-a',
        messages,
        storeRevision: 1 as never,
    })
    return new WindowedConversationHistoryOperation(inner, start)
}

const data = (messages: readonly Message[]) => messages.map((message) => message.data)

describe('WindowedConversationHistoryOperation', () => {
    it('reports the whole conversation length and absolute positions', () => {
        const history = windowOf(4, 6)

        expect(history.totalMessages).toBe(10)
        const latest = history.readLatest(2)
        expect(latest).toMatchObject({ startIndex: 8, endIndex: 10, totalMessages: 10 })
        expect(data(latest.messages)).toEqual(['m8', 'm9'])
        const range = history.readRange(7, 2)
        expect(range).toMatchObject({ startIndex: 7, endIndex: 9 })
        expect(data(range.messages)).toEqual(['m7', 'm8'])
    })

    it('returns only loaded messages for reads that reach before the window', () => {
        const history = windowOf(4, 6)

        expect(data(history.readLatest(8).messages)).toEqual(['m6', 'm7', 'm8', 'm9'])
        const overlapping = history.readRange(4, 4)
        expect(overlapping).toMatchObject({ startIndex: 6, endIndex: 8 })
        expect(data(overlapping.messages)).toEqual(['m6', 'm7'])
    })

    it('continues a range wholly before the window at the window start, so endIndex paging advances', () => {
        const history = windowOf(4, 100)

        const page = history.readRange(0, 3)
        expect(page).toMatchObject({ startIndex: 100, endIndex: 103 })
        const collected: string[] = []
        for (let start = 0; start < history.totalMessages;) {
            const next = history.readRange(start, Math.min(3, history.totalMessages - start))
            if (next.messages.length === 0) break
            collected.push(...data(next.messages))
            start = next.endIndex
        }
        expect(collected).toEqual(['m100', 'm101', 'm102', 'm103'])
    })

    it('returns empty windows instead of reading an empty inner history', () => {
        const history = windowOf(0, 5)

        expect(history.totalMessages).toBe(5)
        expect(history.readLatest(3)).toMatchObject({ messages: [], startIndex: 5, endIndex: 5 })
        expect(history.readRange(1, 2)).toMatchObject({ messages: [], startIndex: 5, endIndex: 5 })
        expect(history.scanBackward(5, 2).entries).toEqual([])
    })

    it('scans backward with absolute indices and stops at the window start', () => {
        const history = windowOf(4, 6)

        const scan = history.scanBackward(9, 2)
        expect(scan.entries.map((entry) => entry.absoluteIndex)).toEqual([8, 7])
        expect(scan.startIndexExclusive).toBe(9)
        expect(history.scanBackward(6, 2).entries).toEqual([])
        expect(history.scanBackward().entries.map((entry) => entry.absoluteIndex)).toEqual([9, 8, 7, 6])
    })

    it('feeds prompt history selection and iteration over the window only', () => {
        const history = windowOf(4, 6, { 7: { disabled: true } })

        const selection = selectPromptHistory(history, 2)
        expect(selection).toMatchObject({ startIndex: 0, endIndex: 10, messageCount: 3, resetByAllBefore: false })
        const entries = [...iteratePromptHistory(history, selection, 2)]
        expect(entries.map((entry) => [entry.absoluteIndex, entry.relativeIndex, entry.message.data])).toEqual([
            [6, 0, 'm6'],
            [8, 1, 'm8'],
            [9, 2, 'm9'],
        ])
    })

    it('reports an allBefore marker inside the window', () => {
        const history = windowOf(4, 6, { 6: { disabled: 'allBefore' } })

        const selection = selectPromptHistory(history)
        expect(selection).toMatchObject({ startIndex: 7, resetByAllBefore: true, messageCount: 3 })
        expect([...iteratePromptHistory(history, selection)].map((entry) => entry.absoluteIndex)).toEqual([7, 8, 9])
    })
})
