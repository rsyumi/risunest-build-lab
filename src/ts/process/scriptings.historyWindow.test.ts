// @vitest-environment node

import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import { getCurrentChat } from '../storage/database.svelte'
import type { Chat, character } from '../storage/database.svelte'
import { registerActiveHistoryWindow } from './historyWindowIndex'
import { openHistoryWindowCopy } from './historyWindowWrite'
import { captureGenerationConversationOperation } from './generationConversationOperation'
import * as polyfill from '../polyfill'
import { alertConfirm } from '../alert'
import { createStoreHistoryWindow, historyMessages } from './tests/historyWindowTestUtils'

vi.mock('../storage/persistentDataRuntime.svelte', () => ({
    peekActiveConversationSession: () => null,
}))
vi.mock('../parser/parser.svelte', () => ({
    hasher: vi.fn(),
    risuChatParser: vi.fn(),
}))
vi.mock('../alert', () => ({
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertInput: vi.fn(),
    alertNormal: vi.fn(),
    alertSelect: vi.fn(),
}))
vi.mock('../globalApi.svelte', () => ({ fetchNative: vi.fn(), readImage: vi.fn() }))
vi.mock('../platform', () => ({ isTauriMobile: true }))
vi.mock('../tokenizer', () => ({ tokenize: vi.fn() }))
vi.mock('../util', () => ({
    asBuffer: vi.fn(),
    getPersonaPrompt: vi.fn(),
    getUserIcon: vi.fn(),
    getUserName: vi.fn(),
    parseKeyValue: vi.fn(() => []),
}))
vi.mock('../storage/database.svelte', () => ({
    getCurrentCharacter: vi.fn(() => ({})),
    getCurrentChat: vi.fn(() => ({ id: 'conversation-1', message: [] })),
    getDatabase: vi.fn(() => ({ characters: [] })),
    setDatabase: vi.fn(),
}))
vi.mock('../stores.svelte', () => ({
    DBState: { db: {} },
    ReloadChatPointer: { update: vi.fn() },
    ReloadGUIPointer: {
        subscribe: (run: (value: number) => void) => {
            run(0)
            return () => {}
        },
        set: vi.fn(),
        update: vi.fn(),
    },
    CurrentTriggerIdStore: {
        subscribe: (run: (value: null) => void) => {
            run(null)
            return () => {}
        },
        set: vi.fn(),
    },
    selectedCharID: {
        subscribe: (run: (value: number) => void) => (run(0), () => undefined),
    },
}))
vi.mock('./modules', () => ({
    getModuleLorebooks: vi.fn(() => []),
    getModuleTriggers: vi.fn(() => []),
}))
vi.mock('./files/inlays', () => ({ getInlayAsset: vi.fn(), writeInlayImage: vi.fn() }))
vi.mock('./lorebook.svelte', () => ({ loadLoreBookV3PromptFromCompatibilitySnapshot: vi.fn() }))
vi.mock('./memory/hypamemory', () => ({ HypaProcesser: vi.fn() }))
vi.mock('./request/request', () => ({ requestChatData: vi.fn() }))
vi.mock('./stableDiff', () => ({ generateAIImage: vi.fn() }))
vi.mock('./command', () => ({ processMultiCommand: vi.fn() }))

let scriptings: typeof import('./scriptings')

beforeAll(async () => {
    const jsonLuaSource = await readFile(resolve(process.cwd(), 'public/lua/json.lua'), 'utf8')
    vi.stubGlobal('fetch', vi.fn(async () => new Response(jsonLuaSource, { status: 200 })))
    scriptings = await import('./scriptings')
})

async function runOnWindow(chat: Chat, body: string, owner: string) {
    const result = await scriptings.runScripted(`
        listenEdit('editInput', function(id, value, meta)
            ${body}
        end)
    `, {
        char: { chaId: owner } as never,
        chat,
        data: 'input',
        mode: 'editInput',
    })
    return result as { chat: Chat, res: unknown }
}

const data = (messages: { data: string }[]) => messages.map((message) => message.data)

describe('Lua chat APIs over a history window', () => {
    let unregister: (() => void) | null = null

    afterEach(() => {
        unregister?.()
        unregister = null
        vi.restoreAllMocks()
    })

    it('reads by index in the whole conversation', async () => {
        const { chat } = createStoreHistoryWindow(historyMessages(1000), 900)

        const result = await runOnWindow(chat, `
            local inside = getChat(id, 950)
            local last = getChat(id, -1)
            return table.concat({
                tostring(getChatLength(id)),
                inside.data,
                last.data,
                getChatData(id, 920),
                getChatRole(id, 921),
                tostring(getChat(id, 5) == nil),
                tostring(getChat(id, -101) == nil),
                '[' .. getChatData(id, 899) .. ']',
                tostring(#getFullChat(id)),
            }, '|')
        `, 'lua-window-reads')

        expect(result.res).toBe('1000|m950|m999|m920|char|true|true|[]|100')
    })

    it('writes inside the window by absolute index and leaves earlier messages alone', async () => {
        const store = historyMessages(1000)
        const copy = openHistoryWindowCopy(createStoreHistoryWindow(store, 900).controller)

        await runOnWindow(copy.chat, `
            setChat(id, 950, 'edited')
            setChat(id, 5, 'ignored')
            setChatRole(id, 6, 'char')
            removeChat(id, 10)
            insertChat(id, 20, 'char', 'ignored')
            removeChat(id, -1)
            insertChat(id, 900, 'char', 'front')
            addChat(id, 'user', 'tail')
            return value
        `, 'lua-window-writes')
        expect(copy.commit()).toBe(true)

        expect(store.slice(0, 900)).toEqual(historyMessages(900))
        expect(data(store.slice(900))).toEqual([
            'front',
            ...data(historyMessages(50, 900)),
            'edited',
            ...data(historyMessages(48, 951)),
            'tail',
        ])
    })

    it('cuts and replaces only the window range', async () => {
        const cutStore = historyMessages(1000)
        const cut = openHistoryWindowCopy(createStoreHistoryWindow(cutStore, 900).controller)
        await runOnWindow(cut.chat, `
            cutChat(id, -10, getChatLength(id))
            return value
        `, 'lua-window-cut')
        expect(cut.commit()).toBe(true)
        expect(data(cutStore)).toEqual([
            ...data(historyMessages(900)),
            ...data(historyMessages(10, 990)),
        ])

        const fullStore = historyMessages(1000)
        const full = openHistoryWindowCopy(createStoreHistoryWindow(fullStore, 900).controller)
        await runOnWindow(full.chat, `
            setFullChat(id, {{role = 'user', data = 'only'}})
            return value
        `, 'lua-window-full')
        expect(full.commit()).toBe(true)
        expect(data(fullStore)).toEqual([...data(historyMessages(900)), 'only'])
    })

    it('hands edit listeners the active window of a send and writes their edits back', async () => {
        const store = historyMessages(1000)
        const { controller } = createStoreHistoryWindow(store, 900)
        unregister = registerActiveHistoryWindow({
            characterId: 'lua-window-listener',
            conversationId: 'conversation-1',
            shell: null,
            controller,
        })
        vi.mocked(getCurrentChat).mockReturnValue({ id: 'conversation-1', message: [] } as never)
        const char = {
            type: 'character',
            chaId: 'lua-window-listener',
            triggerscript: [{
                comment: '',
                type: 'start',
                conditions: [],
                effect: [{
                    type: 'triggerlua',
                    code: `
                        listenEdit('editInput', function(id, value, meta)
                            setChat(id, 999, 'seen ' .. getChatLength(id))
                            return value .. ' ' .. getChatLength(id)
                        end)
                        listenEdit('editDisplay', function(id, value, meta)
                            return value .. ' ' .. getChatLength(id)
                        end)
                    `,
                }],
            }],
        } as unknown as character

        await expect(scriptings.runLuaEditTrigger(char, 'editinput', 'input')).resolves.toBe('input 1000')
        expect(store[999].data).toBe('seen 1000')
        expect(store.slice(0, 900)).toEqual(historyMessages(900))
        // Display listeners keep reading the selected conversation.
        await expect(scriptings.runLuaEditTrigger(char, 'editdisplay', 'shown')).resolves.toBe('shown 0')
    })

    it.each(['insert', 'remove'] as const)('follows an owned Lua %s before the output target', async (action) => {
        const store = historyMessages(1000)
        const { controller } = createStoreHistoryWindow(store, 900)
        unregister = registerActiveHistoryWindow({
            characterId: `lua-output-${action}`,
            conversationId: 'conversation-1',
            shell: null,
            controller,
        })
        const output = captureGenerationConversationOperation({
            session: null,
            getCurrentSession: () => null,
            chat: controller.chat,
            getCurrentChat: () => controller.chat,
            windowedController: controller,
            continueLast: true,
        })
        const char = {
            chaId: `lua-output-${action}`,
            triggerscript: [{ effect: [{ type: 'triggerlua', code: `
                listenEdit('editOutput', function(id, value, meta)
                    ${action === 'insert' ? "insertChat(id, 950, 'user', 'inserted')" : 'removeChat(id, 950)'}
                    return value .. ' edited'
                end)
            ` }] }],
        } as unknown as character
        const accepted: boolean[] = []
        const result = await scriptings.runLuaEditTrigger(
            char, 'editoutput', 'reply', undefined, undefined,
            (commit) => accepted.push(output.acceptCommit(commit)),
        )
        expect(accepted).toEqual([true])
        expect(output.absoluteIndex).toBe(action === 'insert' ? 1000 : 998)
        expect(output.commitData(result)).toBe(true)
        expect(store.at(-1)?.data).toBe('reply edited')
        expect(store.slice(0, 900)).toEqual(historyMessages(900))
        output.release()
    })

    it('does not clone the history or summaries for text-only output listeners', async () => {
        const { controller } = createStoreHistoryWindow(historyMessages(1000), 900)
        controller.chat.hypaV3Data = { summaries: Array.from({ length: 3000 }, (_, i) => ({
            text: `summary ${i}`, chatMemos: [], isImportant: false,
        })) } as never
        unregister = registerActiveHistoryWindow({
            characterId: 'lua-text-only', conversationId: 'conversation-1', shell: null, controller,
        })
        const clone = vi.spyOn(polyfill, 'safeStructuredClone')
        const applyRange = vi.spyOn(controller, 'applyRange')
        const char = {
            chaId: 'lua-text-only',
            triggerscript: [{ effect: [{ type: 'triggerlua', code: `
                listenEdit('editOutput', function(id, value, meta)
                    return value .. ' edited'
                end)
            ` }] }],
        } as unknown as character
        for (let i = 0; i < 5; i += 1) {
            await expect(scriptings.runLuaEditTrigger(char, 'editoutput', 'reply')).resolves.toBe('reply edited')
        }
        expect(clone).not.toHaveBeenCalled()
        expect(applyRange).not.toHaveBeenCalled()
    })

    it.each([false, true])('isolates a lazy history draft across an await (concurrent write %s)', async (concurrent) => {
        const store = historyMessages(1000)
        const { controller } = createStoreHistoryWindow(store, 900)
        unregister = registerActiveHistoryWindow({
            characterId: `lua-await-${concurrent}`, conversationId: 'conversation-1', shell: null, controller,
        })
        let resume!: (value: boolean) => void
        vi.mocked(alertConfirm).mockReset()
        vi.mocked(alertConfirm).mockReturnValueOnce(new Promise((resolve) => { resume = resolve }))
        const char = {
            chaId: `lua-await-${concurrent}`,
            triggerscript: [{ effect: [{ type: 'triggerlua', code: `
                listenEdit('editOutput', function(id, value, meta)
                    setChat(id, 999, 'owned')
                    alertConfirm(id, 'synthetic wait'):await()
                    return getChatData(id, 999)
                end)
            ` }] }],
        } as unknown as character
        const pending = scriptings.runLuaEditTrigger(char, 'editoutput', 'reply')
        const outcome = pending.then(value => ({ value }), error => ({ error }))
        await vi.waitFor(() => expect(alertConfirm).toHaveBeenCalledOnce())
        expect(store[999].data).toBe('m999')
        if (concurrent) controller.applyRange(99, 1, [{ ...controller.chat.message[99], data: 'newer' }], 'edit')
        resume(true)
        if (concurrent) {
            expect(await outcome).toHaveProperty('error')
            expect(store[999].data).toBe('newer')
        } else {
            expect(await outcome).toEqual({ value: 'owned' })
            expect(store[999].data).toBe('owned')
        }
    })
})
