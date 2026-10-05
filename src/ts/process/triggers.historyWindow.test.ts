import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Chat, character } from '../storage/database.svelte'
import { DBState, selectedCharID } from '../stores.svelte'
import { historyLength } from './historyWindowIndex'
import { writeHistoryWindowChat } from './historyWindowWrite'
import { createStoreHistoryWindow, historyMessages } from './tests/historyWindowTestUtils'

vi.mock('../storage/persistentDataRuntime.svelte', () => ({
    acquireDestructiveReplacementFence: () => {
        throw new Error('Unexpected native runtime access in this test')
    },
    capturePersistentMutationToken: () => {
        throw new Error('Unexpected native runtime access in this test')
    },
    getPersistentDataRuntime: () => {
        throw new Error('Unexpected native runtime access in this test')
    },
    peekActiveConversationSession: () => null,
}))
vi.mock('./modules', async (importOriginal) => ({
    ...await importOriginal<typeof import('./modules')>(),
    getModuleTriggers: () => [],
}))
vi.mock('../tokenizer', () => ({ tokenize: vi.fn(async () => 0) }))
vi.mock('../parser/parser.svelte', () => ({
    risuChatParser: (value: string) => value,
}))
vi.mock('./command', () => ({ processMultiCommand: vi.fn() }))
vi.mock('./request/request', () => ({ requestChatData: vi.fn() }))
vi.mock('./stableDiff', () => ({ generateAIImage: vi.fn() }))
vi.mock('./files/inlays', () => ({ writeInlayImage: vi.fn() }))

const { runTrigger } = await import('./triggers')

function characterWith(chat: Chat, effect: unknown[], conditions: unknown[] = []) {
    const char = {
        type: 'character',
        chaId: 'character-1',
        name: 'Character',
        chatPage: 0,
        chats: [chat],
        triggerscript: [{ comment: 'window', type: 'start', conditions, effect }],
        customscript: [],
        defaultVariables: '',
        firstMessage: 'first',
        alternateGreetings: [],
        lowLevelAccess: false,
    } as unknown as character
    DBState.db = { characters: [char], templateDefaultVariables: '' } as never
    return char
}

describe('block triggers over a history window', () => {
    beforeEach(() => {
        selectedCharID.set(0)
    })

    it('counts and reads messages by their index in the whole conversation', async () => {
        const { chat } = createStoreHistoryWindow(historyMessages(1000), 900)
        const char = characterWith(chat, [
            { type: 'v2GetMessageCount', outputVar: 'count', indent: 0 },
            { type: 'v2GetMessageAtIndex', index: '950', indexType: 'value', outputVar: 'inside', indent: 0 },
            { type: 'v2GetMessageAtIndex', index: '5', indexType: 'value', outputVar: 'before', indent: 0 },
            { type: 'v2GetLastMessage', outputVar: 'last', indent: 0 },
        ], [{ type: 'chatindex', value: '1000', operator: '=' }])

        const result = await runTrigger(char, 'start', { chat })

        expect(result?.chat.scriptstate).toEqual({
            $count: '1000',
            $inside: 'm950',
            $before: 'null',
            $last: 'm999',
        })
    })

    it('edits window messages by absolute index and leaves earlier messages alone', async () => {
        const store = historyMessages(1000)
        const { chat, controller } = createStoreHistoryWindow(store, 900)
        const char = characterWith(chat, [
            { type: 'modifychat', index: '950', value: 'edited' },
            { type: 'modifychat', index: '5', value: 'ignored' },
            { type: 'v2ModifyChat', index: '999', indexType: 'value', value: 'tail', valueType: 'value', indent: 0 },
            { type: 'v2ModifyChat', index: '899', indexType: 'value', value: 'ignored', valueType: 'value', indent: 0 },
        ])

        const result = await runTrigger(char, 'start', { chat })
        expect(writeHistoryWindowChat(controller, result!.chat)).toBe(true)

        expect(store).toHaveLength(1000)
        expect(store[950].data).toBe('edited')
        expect(store[999].data).toBe('tail')
        expect(store.slice(0, 900)).toEqual(historyMessages(900))
    })

    it.each([
        ['cutchat', { type: 'cutchat', start: '-10', end: '1000' }],
        ['v2CutChat without an end', {
            type: 'v2CutChat',
            start: '-10',
            startType: 'value',
            end: 'missing',
            endType: 'var',
            indent: 0,
        }],
        ['cutchat from an absolute start', { type: 'cutchat', start: '990', end: '1000' }],
    ])('keeps the last messages with %s and keeps the messages before the window', async (_name, effect) => {
        const store = historyMessages(1000)
        const { chat, controller } = createStoreHistoryWindow(store, 900)
        const char = characterWith(chat, [effect])

        const result = await runTrigger(char, 'start', { chat })
        expect(historyLength(result!.chat)).toBe(910)
        expect(writeHistoryWindowChat(controller, result!.chat)).toBe(true)

        expect(store.map((message) => message.data)).toEqual([
            ...historyMessages(900).map((message) => message.data),
            ...historyMessages(10, 990).map((message) => message.data),
        ])
    })

    it('runs a cut that starts before the window as a cut from the window start', async () => {
        const store = historyMessages(1000)
        const { chat, controller } = createStoreHistoryWindow(store, 900)
        const char = characterWith(chat, [{ type: 'cutchat', start: '0', end: '950' }])

        const result = await runTrigger(char, 'start', { chat })
        expect(writeHistoryWindowChat(controller, result!.chat)).toBe(true)

        expect(store.map((message) => message.data)).toEqual(
            historyMessages(950).map((message) => message.data),
        )
    })
})
