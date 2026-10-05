import { describe, expect, it, vi } from 'vitest'

import type { WindowedConversationMutationController } from '../storage/activeWorkingSet.svelte'
import type { Chat, Message } from '../storage/database.svelte'
import { diffHistoryWindow, writeHistoryWindow } from './historyWindowWrite'

const message = (data: string): Message => ({ role: 'user', data, chatId: `id-${data}` })
const list = (...values: string[]) => values.map(message)

function controllerFor(messages: Message[]) {
    const chat: Chat = { id: 'conversation-a', name: '', note: '', localLore: [], message: messages }
    const applyRange = vi.fn((start: number, deleteCount: number, replacement: readonly Message[]) => {
        chat.message.splice(start, deleteCount, ...structuredClone([...replacement]))
        return true
    })
    const controller: WindowedConversationMutationController = {
        chat,
        absoluteStartIndex: 40,
        isCurrent: () => true,
        applyRange,
        release() {},
    }
    return { controller, applyRange, chat }
}

describe('diffHistoryWindow', () => {
    it('finds no change between equal windows', () => {
        expect(diffHistoryWindow(list('a', 'b'), list('a', 'b'))).toBeNull()
        expect(diffHistoryWindow([], [])).toBeNull()
    })

    it('reduces an edit, insert, removal and append to one range that ends the window when the count changes', () => {
        expect(diffHistoryWindow(list('a', 'b', 'c'), list('a', 'x', 'c'))).toEqual({
            start: 1, deleteCount: 1, messages: list('x'),
        })
        expect(diffHistoryWindow(list('a', 'c'), list('a', 'b', 'c'))).toEqual({
            start: 1, deleteCount: 1, messages: list('b', 'c'),
        })
        expect(diffHistoryWindow(list('a', 'b', 'c'), list('a', 'c'))).toEqual({
            start: 1, deleteCount: 2, messages: list('c'),
        })
        expect(diffHistoryWindow(list('a'), list('a', 'b'))).toEqual({
            start: 1, deleteCount: 0, messages: list('b'),
        })
        expect(diffHistoryWindow(list('a', 'b', 'c', 'd'), list('x', 'b', 'c', 'y'))).toEqual({
            start: 0, deleteCount: 4, messages: list('x', 'b', 'c', 'y'),
        })
    })

    it('compares message contents, not identity', () => {
        expect(diffHistoryWindow(list('a'), structuredClone(list('a')))).toBeNull()
    })
})

describe('writeHistoryWindow', () => {
    it('writes nothing when the window is unchanged', () => {
        const { controller, applyRange } = controllerFor(list('a', 'b'))

        expect(writeHistoryWindow(controller, list('a', 'b'))).toBe(true)
        expect(applyRange).not.toHaveBeenCalled()
    })

    it('writes the changed range with a matching command', () => {
        const edit = controllerFor(list('a', 'b'))
        expect(writeHistoryWindow(edit.controller, list('a', 'x'))).toBe(true)
        expect(edit.applyRange).toHaveBeenCalledWith(1, 1, list('x'), 'edit')

        const append = controllerFor(list('a'))
        expect(writeHistoryWindow(append.controller, list('a', 'b'))).toBe(true)
        expect(append.applyRange).toHaveBeenCalledWith(1, 0, list('b'), 'append')

        const removal = controllerFor(list('a', 'b', 'c'))
        expect(writeHistoryWindow(removal.controller, list('c'))).toBe(true)
        expect(removal.applyRange).toHaveBeenCalledWith(0, 3, list('c'), 'replace-range')
        expect(removal.chat.message).toEqual(list('c'))
    })

    it('reports a refused write', () => {
        const { controller, applyRange } = controllerFor(list('a'))
        applyRange.mockReturnValueOnce(false)

        expect(writeHistoryWindow(controller, list('b'))).toBe(false)
    })
})
