import { afterEach, describe, expect, it, vi } from 'vitest'
import {
    defaultCBSRegisterArg,
    registerCBS,
    type matcherArg,
    type RegisterCallback,
} from './cbs'
import { registerActiveHistoryWindow } from './process/historyWindowIndex'
import { createStoreHistoryWindow, historyMessages } from './process/tests/historyWindowTestUtils'
import type { Chat, Database } from './storage/database.svelte'

vi.mock('./stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return { CurrentTriggerIdStore: writable(null) }
})

const callbacks = new Map<string, RegisterCallback>()
registerCBS({
    ...defaultCBSRegisterArg,
    getDatabase: () => ({ characters: [] }) as unknown as Database,
    getSelectedCharID: () => 0,
    registerFunction: ({ name, callback, alias }) => {
        if (callback === 'doc_only') return
        for (const key of [name, ...alias]) callbacks.set(key, callback)
    },
})

function databaseWith(chat: Chat): Database {
    return {
        characters: [{
            chaId: 'character-1',
            chatPage: 0,
            chats: [chat],
            firstMessage: 'greeting',
            alternateGreetings: [],
        }],
    } as unknown as Database
}

function run(name: string, db: Database, chatID = -1, args: string[] = []) {
    const result = callbacks.get(name)!('', {
        chatID,
        db,
        chara: db.characters[0],
        rmVar: false,
        cbsConditions: {},
    } as matcherArg, args, null)
    return typeof result === 'string' ? result : result?.text
}

function windowedConversation(total: number, start: number) {
    const { chat: window, controller } = createStoreHistoryWindow(historyMessages(total), start)
    const shell = {
        id: 'conversation-1',
        fmIndex: -1,
        get message(): never {
            throw new Error('metadata-only conversation')
        },
    } as unknown as Chat
    const unregister = registerActiveHistoryWindow({
        characterId: 'character-1',
        conversationId: 'conversation-1',
        shell,
        controller,
    })
    return { db: databaseWith(shell), window, unregister }
}

describe('history CBS over a history window', () => {
    let unregister: (() => void) | null = null

    afterEach(() => {
        unregister?.()
        unregister = null
    })

    it('reads the window in place of a metadata-only conversation by absolute index', () => {
        const conversation = windowedConversation(1000, 900)
        unregister = conversation.unregister
        const { db } = conversation

        expect(run('lastmessageid', db)).toBe('999')
        expect(run('lastmessage', db)).toBe('m999')
        expect(run('previous_chat_log', db, -1, ['950'])).toBe('m950')
        expect(run('previous_chat_log', db, -1, ['5'])).toBe('Out of range')
        expect(run('role', db, 951)).toBe('char')
        expect(run('messagetime', db, 950)).toBe(new Date(1_000_000 + 950 * 1000).toLocaleTimeString())
        expect(run('previoususerchat', db, 951)).toBe('m950')
        expect(run('previouscharchat', db, 950)).toBe('m949')
        // The scan stops at the window start and falls back to the greeting.
        expect(run('previouscharchat', db, 900)).toBe('greeting')
        expect(JSON.parse(run('userhistory', db)!)).toHaveLength(50)
    })

    it('leaves the greeting out of history when the window starts after the first message', () => {
        const late = windowedConversation(1000, 900)
        const history = JSON.parse(run('history', late.db)!) as string[]
        late.unregister()
        expect(history).toHaveLength(100)
        expect(JSON.parse(history[0]).data).toBe('m900')

        const whole = windowedConversation(4, 0)
        unregister = whole.unregister
        const complete = (JSON.parse(run('history', whole.db)!) as string[]).map((entry) => JSON.parse(entry).data)
        expect(complete).toEqual(['greeting', 'm0', 'm1', 'm2', 'm3'])
    })

    it('reads a complete conversation as it is while a send builds a window over it', () => {
        const { controller } = createStoreHistoryWindow(historyMessages(1000), 900)
        const complete = { id: 'conversation-1', fmIndex: -1, message: historyMessages(1000) } as Chat
        unregister = registerActiveHistoryWindow({
            characterId: 'character-1',
            conversationId: 'conversation-1',
            shell: null,
            controller,
        })
        const db = databaseWith(complete)

        expect(run('lastmessageid', db)).toBe('999')
        expect(run('previous_chat_log', db, -1, ['5'])).toBe('m5')
        expect(JSON.parse(run('history', db)!)).toHaveLength(1001)
    })
})
