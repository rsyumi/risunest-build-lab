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

vi.mock('../storage/persistentDataRuntime.svelte', async () => (await import('./tests/scriptingsTestHarness')).persistentRuntimeModule())
vi.mock('../parser/parser.svelte', async () => (await import('./tests/scriptingsTestHarness')).parserModule())
vi.mock('../alert', async () => (await import('./tests/scriptingsTestHarness')).alertModule())
vi.mock('../globalApi.svelte', () => ({ fetchNative: vi.fn(), readImage: vi.fn() }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'android' }))
vi.mock('../tokenizer', async () => (await import('./tests/scriptingsTestHarness')).tokenizerModule())
vi.mock('../util', async () => (await import('./tests/scriptingsTestHarness')).utilModule())
vi.mock('../storage/database.svelte', async () => (await import('./tests/scriptingsTestHarness')).databaseModule({
    getCurrentChat: vi.fn(() => ({ id: 'conversation-1', message: [] })),
}))
vi.mock('../stores.svelte', async () => (await import('./tests/scriptingsTestHarness')).storesModule())
vi.mock('./modules', async () => (await import('./tests/scriptingsTestHarness')).modulesModule())
vi.mock('./files/inlays', async () => (await import('./tests/scriptingsTestHarness')).inlaysModule())
vi.mock('./lorebook.svelte', async () => (await import('./tests/scriptingsTestHarness')).lorebookModule())
vi.mock('./memory/hypamemory', async () => (await import('./tests/scriptingsTestHarness')).hypamemoryModule())
vi.mock('./request/request', async () => (await import('./tests/scriptingsTestHarness')).requestModule())
vi.mock('./stableDiff', async () => (await import('./tests/scriptingsTestHarness')).stableDiffModule())
vi.mock('./command', async () => (await import('./tests/scriptingsTestHarness')).commandModule())

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
