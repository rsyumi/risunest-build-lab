import { describe, expect, it, vi } from 'vitest'

import type { WindowedConversationMutationController } from '../storage/activeWorkingSet.svelte'
import type { Chat, Message } from '../storage/database.svelte'
import { diffHistoryWindow, openHistoryWindowCopy, writeHistoryWindow } from './historyWindowWrite'
import { captureGenerationConversationOperation } from './generationConversationOperation'
import { createHistoryWindowController } from './historyWindowController'

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
    it('refuses an older draft after an accepted metadata-only write', () => {
        const base = controllerFor(list('a', 'b'))
        const controller = createHistoryWindowController({
            captureWindowed: () => base.controller,
            captureSession: () => null,
            getCurrentSession: () => null,
            readLiveMetadata: () => base.chat,
        }, base.chat, 40, base.controller)
        const copy = openHistoryWindowCopy(controller)
        copy.chat.scriptstate = { $value: 'stale' }
        controller.chat.scriptstate = { $value: 'newer' }
        expect(controller.applyRange(0, 0, [], 'update-metadata')).toBe(true)
        expect(copy.commit()).toBe(false)
        expect(controller.chat.scriptstate).toEqual({ $value: 'newer' })
        controller.release()
    })

    it.each(['deleted', 'duplicate', 'replaced', 'missing-id'] as const)(
        'rejects an owned receipt with a %s output target', (change) => {
            const base = controllerFor(list('a', 'b'))
            if (change === 'missing-id') delete base.chat.message[1].chatId
            const controller = { ...base.controller, reconcileMetadata: () => true }
            const output = captureGenerationConversationOperation({
                session: null, getCurrentSession: () => null,
                chat: base.chat, getCurrentChat: () => base.chat,
                windowedController: controller, continueLast: true,
            })
            const accepted: boolean[] = []
            const copy = openHistoryWindowCopy(controller, (receipt) => accepted.push(output.acceptCommit(receipt)))
            if (change === 'deleted') copy.chat.message.pop()
            if (change === 'duplicate') copy.chat.message.push({ ...copy.chat.message[1] })
            if (change === 'replaced') copy.chat.message[1] = { ...copy.chat.message[1] }
            expect(copy.commit()).toBe(true)
            expect(accepted).toEqual([false])
            expect(output.commitData('wrong target')).toBe(false)
        },
    )

    it('refuses a draft after an unrelated controller write', () => {
        const base = controllerFor(list('a', 'b'))
        const controller = { ...base.controller, reconcileMetadata: () => true }
        const observer = vi.fn()
        const copy = openHistoryWindowCopy(controller, observer)
        controller.applyRange(0, 1, list('newer'), 'edit')
        copy.chat.message[0].data = 'stale'
        expect(copy.commit()).toBe(false)
        expect(base.chat.message[0].data).toBe('newer')
        expect(observer).not.toHaveBeenCalled()
    })

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
