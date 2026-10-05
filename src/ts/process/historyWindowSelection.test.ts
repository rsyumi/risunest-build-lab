import { describe, expect, it } from 'vitest'

import type { Chat, Message } from '../storage/database.svelte'
import {
    planHistoryWindowHypaV3,
    readHistoryWindowTail,
    selectHistoryWindow,
    type HistoryWindowReader,
} from './historyWindowSelection'

function messages(count: number, overrides: Record<number, Partial<Message>> = {}): Message[] {
    return Array.from({ length: count }, (_, index): Message => ({
        role: index % 2 ? 'char' : 'user',
        data: `m${index}`,
        chatId: `id-${index}`,
        ...overrides[index],
    }))
}

function readerOf(all: Message[]) {
    const reads: Array<[number, number]> = []
    const reader: HistoryWindowReader = {
        totalMessages: all.length,
        async read(startIndex, limit) {
            reads.push([startIndex, limit])
            return all.slice(startIndex, startIndex + limit)
        },
    }
    return { reader, reads }
}

// Every message costs ten tokens, so a budget of N * 10 reaches N enabled messages.
const tenTokens = async () => 10

describe('selectHistoryWindow', () => {
    it('stops once the token budget is reached and reads only the pages it needs', async () => {
        const { reader, reads } = readerOf(messages(1000))

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 50,
            countTokens: tenTokens,
            minimumMessages: 1,
            pageSize: 8,
        })

        expect(window.start).toBe(995)
        expect(window.messages.map((message) => message.data)).toEqual(['m995', 'm996', 'm997', 'm998', 'm999'])
        expect(window.totalMessages).toBe(1000)
        expect(window.endedAtAllBefore).toBe(false)
        expect(reads).toEqual([[992, 8]])
        expect(window.bodyRows).toBe(8)
        expect(window.bodyPages).toBe(1)
    })

    it('counts the raw message text', async () => {
        const all = messages(4)
        all[3].data = 'x'.repeat(30)
        const { reader } = readerOf(all)

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 25,
            countTokens: async (text) => text.length,
            minimumMessages: 1,
        })

        expect(window.start).toBe(3)
    })

    it('skips disabled messages without counting them', async () => {
        const all = messages(10, { 9: { disabled: true }, 8: { disabled: true } })
        const { reader } = readerOf(all)

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 20,
            countTokens: tenTokens,
            minimumMessages: 1,
        })

        expect(window.start).toBe(6)
        expect(window.messages).toHaveLength(4)
    })

    it('includes an allBefore marker and stops there', async () => {
        const all = messages(10, { 7: { disabled: 'allBefore' } })
        const { reader } = readerOf(all)

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 1000,
            countTokens: tenTokens,
            minimumMessages: 1,
        })

        expect(window.start).toBe(7)
        expect(window.endedAtAllBefore).toBe(true)
        expect(window.messages[0].disabled).toBe('allBefore')
    })

    it('keeps the minimum number of enabled messages beyond the budget', async () => {
        const { reader } = readerOf(messages(20, { 16: { disabled: true } }))

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 10,
            countTokens: tenTokens,
            minimumMessages: 5,
        })

        expect(window.start).toBe(14)
        expect(window.messages.filter((message) => message.disabled !== true)).toHaveLength(5)
    })

    it('loads the whole conversation when the budget is never reached', async () => {
        const { reader } = readerOf(messages(30))

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 10_000,
            countTokens: tenTokens,
            minimumMessages: 1,
            pageSize: 7,
        })

        expect(window.start).toBe(0)
        expect(window.messages).toHaveLength(30)
        expect(window.bodyRows).toBe(30)
    })

    it('extends the window down to the requested index', async () => {
        const { reader, reads } = readerOf(messages(200))

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 20,
            countTokens: tenTokens,
            minimumMessages: 1,
            extendTo: 150,
            pageSize: 16,
        })

        expect(window.start).toBe(150)
        expect(window.messages[0].data).toBe('m150')
        expect(window.messages).toHaveLength(50)
        expect(Math.min(...reads.map(([start]) => start))).toBe(150)
    })

    it('does not shrink a window that already starts before the extension index', async () => {
        const { reader } = readerOf(messages(100))

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 500,
            countTokens: tenTokens,
            minimumMessages: 1,
            extendTo: 98,
        })

        expect(window.start).toBe(50)
    })

    it('stops the extension at an allBefore marker', async () => {
        const { reader } = readerOf(messages(100, { 80: { disabled: 'allBefore' } }))

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 20,
            countTokens: tenTokens,
            minimumMessages: 1,
            extendTo: 40,
        })

        expect(window.start).toBe(80)
        expect(window.endedAtAllBefore).toBe(true)
    })

    it('returns an empty window for an empty conversation', async () => {
        const { reader, reads } = readerOf([])

        const window = await selectHistoryWindow({
            reader,
            tokenBudget: 20,
            countTokens: tenTokens,
            minimumMessages: 1,
            extendTo: 0,
        })

        expect(window).toMatchObject({ start: 0, messages: [], totalMessages: 0, bodyRows: 0 })
        expect(reads).toEqual([])
    })

    it('rejects a short page and checks currency after each read', async () => {
        const all = messages(10)
        await expect(selectHistoryWindow({
            reader: { totalMessages: 10, read: async (start, limit) => all.slice(start, start + limit - 1) },
            tokenBudget: 20,
            countTokens: tenTokens,
            minimumMessages: 1,
        })).rejects.toThrow('incomplete')

        let checks = 0
        await expect(selectHistoryWindow({
            reader: readerOf(all).reader,
            tokenBudget: 20,
            countTokens: tenTokens,
            minimumMessages: 1,
            assertCurrent: () => {
                checks += 1
                if (checks > 1) throw new Error('stale')
            },
        })).rejects.toThrow('stale')
    })
})

describe('readHistoryWindowTail', () => {
    it('reads from the start index through the newest message in pages', async () => {
        const { reader, reads } = readerOf(messages(10))
        const tail = await readHistoryWindowTail(reader, 3, undefined, 4)
        expect(tail.start).toBe(3)
        expect(tail.messages.map((message) => message.data)).toEqual(['m3', 'm4', 'm5', 'm6', 'm7', 'm8', 'm9'])
        expect(reads).toEqual([[3, 4], [7, 3]])
    })

    it('clamps the start index to the conversation', async () => {
        const all = messages(3)
        expect((await readHistoryWindowTail(readerOf(all).reader, -2)).start).toBe(0)
        const past = readerOf(all)
        const empty = await readHistoryWindowTail(past.reader, 5)
        expect(empty).toMatchObject({ start: 3, messages: [] })
        expect(past.reads).toEqual([])
    })

    it('rejects a short page and checks currency after each read', async () => {
        const all = messages(4)
        await expect(readHistoryWindowTail(
            { totalMessages: 4, read: async (start, limit) => all.slice(start, start + limit - 1) },
            0,
        )).rejects.toThrow('incomplete')
        await expect(readHistoryWindowTail(readerOf(all).reader, 0, () => {
            throw new Error('stale')
        })).rejects.toThrow('stale')
    })
})

function conversation(summaries: unknown): Omit<Chat, 'message'> {
    return {
        id: 'conversation-a',
        name: '',
        note: '',
        localLore: [],
        hypaV3Data: summaries === undefined ? undefined : { summaries } as Chat['hypaV3Data'],
    }
}

const summary = (...chatMemos: string[]) => ({ text: 'summary', chatMemos, isImportant: false })

describe('planHistoryWindowHypaV3', () => {
    const metadata = (count: number, overrides: Record<number, { chatId?: string, disabled?: Message['disabled'] }> = {}) =>
        Array.from({ length: count }, (_, index) => ({
            chatId: `id-${index}`,
            disabled: false as Message['disabled'],
            ...overrides[index],
        }))

    it('extends to the newest summary\'s last message', () => {
        const plan = planHistoryWindowHypaV3(
            conversation([summary('id-0', 'id-1'), summary('id-2', 'id-3', 'id-4')]),
            metadata(10),
            false,
        )

        expect(plan.extendTo).toBe(4)
        expect(plan.effectiveMessageMemos).toEqual(Array.from({ length: 10 }, (_, index) => `id-${index}`))
    })

    it('accepts chat memos stored as a Set', () => {
        const plan = planHistoryWindowHypaV3(
            conversation([{ text: 'summary', chatMemos: new Set(['id-5', 'id-6']), isImportant: false }]),
            metadata(10),
            false,
        )

        expect(plan.extendTo).toBe(6)
    })

    it('loads everything from the last allBefore marker when nothing is summarized', () => {
        expect(planHistoryWindowHypaV3(conversation(undefined), metadata(10), false).extendTo).toBe(0)
        expect(planHistoryWindowHypaV3(conversation([]), metadata(10), false).extendTo).toBe(0)

        const marked = planHistoryWindowHypaV3(
            conversation([]),
            metadata(10, { 3: { disabled: 'allBefore' }, 6: { disabled: 'allBefore' } }),
            false,
        )
        expect(marked.extendTo).toBe(6)
        expect(marked.effectiveMessageMemos).toEqual(['id-7', 'id-8', 'id-9'])
    })

    it('treats malformed summaries as unsummarized', () => {
        expect(planHistoryWindowHypaV3(conversation([{ text: 'x' }]), metadata(5), false).extendTo).toBe(0)
        expect(planHistoryWindowHypaV3(conversation([summary('id-1'), { chatMemos: [1] }]), metadata(5), false).extendTo).toBe(0)
    })

    it('leaves disabled messages and messages without ids out of the memo set', () => {
        const plan = planHistoryWindowHypaV3(
            conversation([]),
            metadata(5, { 1: { disabled: true }, 3: { chatId: undefined } }),
            false,
        )

        expect(plan.effectiveMessageMemos).toEqual(['id-0', 'id-2', 'id-4'])
    })

    it('drops orphaned summaries the way HypaV3 does before finding the boundary', () => {
        // The newest summary names a message that no longer exists.
        const summaries = [summary('id-1', 'id-2'), summary('id-3', 'gone')]

        const cleaned = planHistoryWindowHypaV3(conversation(summaries), metadata(10), false)
        expect(cleaned.extendTo).toBe(2)

        const allOrphaned = planHistoryWindowHypaV3(conversation([summary('gone')]), metadata(10), false)
        expect(allOrphaned.extendTo).toBe(0)
    })

    it('reports an unresolved boundary when orphaned summaries are preserved', () => {
        const plan = planHistoryWindowHypaV3(
            conversation([summary('id-1', 'id-2'), summary('id-3', 'gone')]),
            metadata(10),
            true,
        )

        expect(plan.extendTo).toBeNull()
    })

    it('treats a summary of a disabled message as orphaned', () => {
        const summaries = [summary('id-1'), summary('id-4')]
        const rows = metadata(10, { 4: { disabled: true } })

        expect(planHistoryWindowHypaV3(conversation(summaries), rows, false).extendTo).toBe(1)
        expect(planHistoryWindowHypaV3(conversation(summaries), rows, true).extendTo).toBeNull()
    })
})
