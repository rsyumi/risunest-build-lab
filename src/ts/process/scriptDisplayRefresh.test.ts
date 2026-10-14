// @vitest-environment jsdom

import { get, writable } from 'svelte/store'
import { afterEach, beforeEach, expect, test, vi } from 'vitest'
import type { Chat, Database, character, customscript } from '../storage/database.svelte'
import { ActiveConversationSession } from '../storage/activeConversationSession'

vi.mock('../platform', () => ({ isTauri: false, isNodeServer: false }))

const live = vi.hoisted(() => ({
    database: {} as Database,
    session: null as unknown,
}))
const plugins = vi.hoisted(() => ({
    editinput: new Set<(data: string) => Promise<string | null>>(),
    editoutput: new Set<(data: string) => Promise<string | null>>(),
    editprocess: new Set<(data: string) => Promise<string | null>>(),
    editdisplay: new Set<(data: string) => Promise<string | null>>(),
}))
const lua = vi.hoisted(() => ({
    displayListener: async (_data: string) => {},
}))

vi.mock('../storage/database.svelte', () => ({
    appVer: '1.0.0',
    getDatabase: () => live.database,
    getCurrentCharacter: () => live.database.characters[0],
    getCurrentChat: () => live.database.characters[0]?.chats[0],
}))
vi.mock('../storage/persistentDataRuntime.svelte', async (importOriginal) => ({
    ...await importOriginal<typeof import('../storage/persistentDataRuntime.svelte')>(),
    peekActiveConversationSession: () => live.session,
}))
vi.mock('../stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return {
        DBState: { get db() { return live.database } },
        selIdState: { selId: 0 },
        selectedCharID: writable(0),
        CharEmotion: writable({}),
        CurrentTriggerIdStore: writable(null),
        ReloadChatPointer: writable({}),
        ScriptDisplayRefreshPointer: writable(0),
    }
})
vi.mock('../globalApi.svelte', () => ({
    aiWatermarkingLawApplies: () => false,
    downloadFile: vi.fn(),
    getFileSrc: async (source: string) => source,
}))
vi.mock('./modules', () => ({
    getModules: () => [],
    getModuleLorebooks: () => [],
    getModuleAssets: () => [],
    getModuleRegexScripts: () => [],
}))
vi.mock('./memory/hypamemory', () => ({ HypaProcesser: class {} }))
// The Lua host: a display listener that runs the given reads and keeps the text.
vi.mock('./scriptings', () => ({
    runLuaEditTrigger: async (_char: unknown, _mode: unknown, data: string) => {
        await lua.displayListener(data)
        return data
    },
}))
vi.mock('../plugins/plugins.svelte', () => ({ pluginV2: plugins }))
vi.mock('./triggers', () => ({ runTrigger: vi.fn() }))
vi.mock('../alert', () => ({ alertError: vi.fn(), alertNormal: vi.fn() }))

const { processScriptFull, resetScriptCache } = await import('./scripts')
const { requestScriptDisplayRefresh } = await import('./scriptDisplayRefresh')
const { DisplayReadRecord, registerDisplayRow } = await import('./scriptVariableReads')
const { getChatVar, getGlobalChatVar } = await import('../parser/chatVar.svelte')
const { ReloadChatPointer } = await import('../stores.svelte')

const statusPanel: customscript = {
    comment: 'status panel on the last message only',
    type: 'editdisplay',
    in: '\\[상태창\\]',
    out: '{{#if {{equal::{{chat_index}}::{{lastmessageid}}}}}}<b>HP {{getvar::hp}}</b>{{/if}}',
    flag: 'g',
    ableFlag: true,
}

function installConversation(scripts: customscript[]) {
    const chat = {
        id: 'status-panel',
        message: [0, 1, 2].map((index) => ({
            role: 'char',
            data: `turn ${index} [상태창]`,
            chatId: `turn-${index}`,
        })),
        note: '',
        name: '',
        localLore: [],
        fmIndex: -1,
        scriptstate: { $hp: '10', $mood: 'calm' },
    } as unknown as Chat
    const char = {
        type: 'character',
        name: 'Synthetic',
        chaId: 'synthetic',
        firstMessage: 'hello',
        alternateGreetings: [],
        chats: [chat],
        chatPage: 0,
        customscript: scripts,
        globalLore: [],
        additionalAssets: [],
        defaultVariables: '',
    } as unknown as character
    live.database = {
        characters: [char],
        presetRegex: [],
        globalChatVariables: {},
        templateDefaultVariables: '',
        dynamicAssets: false,
    } as unknown as Database
    live.session = new ActiveConversationSession({
        characterId: char.chaId,
        conversationId: chat.id!,
        conversation: chat,
        storeRevision: 1,
    })
    return { char, chat }
}

const rows: Array<{ dispose(): void }> = []

async function renderRow(char: character, chat: Chat, index: number) {
    const reads = new DisplayReadRecord()
    const { data } = await processScriptFull(
        char,
        chat.message[index].data,
        'editdisplay',
        index,
        {},
        { variableReads: reads, regexWorker: false },
    )
    const row = registerDisplayRow(() => index)
    row.track('body', reads)
    rows.push(row)
    return { data, row }
}

async function refreshedRows(request: Parameters<typeof requestScriptDisplayRefresh>[0]) {
    const before = { ...get(ReloadChatPointer) }
    requestScriptDisplayRefresh(request)
    await Promise.resolve()
    const after = get(ReloadChatPointer)
    return Object.keys(after)
        .filter((index) => after[Number(index)] !== before[Number(index)])
        .map(Number)
}

beforeEach(() => {
    resetScriptCache()
    ReloadChatPointer.set({})
})

afterEach(() => {
    for (const row of rows.splice(0)) row.dispose()
    plugins.editdisplay.clear()
    lua.displayListener = async () => {}
    live.session = null
})

test('refreshes only the rows whose display read a changed chat variable', async () => {
    const { char, chat } = installConversation([statusPanel])
    const rendered = []
    for (const index of [0, 1, 2]) rendered.push(await renderRow(char, chat, index))
    expect(rendered.map(({ data }) => data)).toEqual([
        'turn 0 ',
        'turn 1 ',
        'turn 2 <b>HP 10</b>',
    ])

    expect(await refreshedRows({ keys: ['hp'] })).toEqual([2])
    expect(await refreshedRows({ keys: ['mood'] })).toEqual([])
    expect(await refreshedRows({ all: true })).toEqual([2])

    // A row rendered again from the cached regex result keeps its reads.
    rendered[2].row.dispose()
    expect((await renderRow(char, chat, 2)).data).toBe('turn 2 <b>HP 10</b>')
    expect(await refreshedRows({ keys: ['hp'] })).toEqual([2])
})

test.each(['chat', 'global', 'local global'])('records the %s variables a Lua display listener reads', async (source) => {
    const { char, chat } = installConversation([])
    live.database.globalChatVariables = { mood: 'calm' }
    if (source === 'local global') chat.GLGlobalVariables = { mood: 'local' }
    lua.displayListener = async () => {
        await Promise.resolve()
        if (source === 'chat') getChatVar('mood')
        else getGlobalChatVar('mood')
    }
    await renderRow(char, chat, 0)

    expect(await refreshedRows({ keys: ['hp'] })).toEqual(source === 'chat' ? [] : [0])
    expect(await refreshedRows({ keys: ['mood'] })).toEqual([0])
    expect(await refreshedRows({ all: true })).toEqual([0])
})

test('refreshes a row whose display ran a plugin hook for any change', async () => {
    const { char, chat } = installConversation([])
    plugins.editdisplay.add(async (data) => data)
    await renderRow(char, chat, 0)
    plugins.editdisplay.clear()
    await renderRow(char, chat, 1)

    expect(await refreshedRows({ keys: ['hp'] })).toEqual([0])
})

test.each(['chat', 'global'])('records the reads of a row rendered from a projected %s variable snapshot', async (source) => {
    const { char, chat } = installConversation([{
        ...statusPanel,
        out: source === 'global' ? statusPanel.out.replace('getvar', 'getglobalvar') : statusPanel.out,
    }])
    const reads = new DisplayReadRecord()
    const { data } = await processScriptFull(char, chat.message[2].data, 'editdisplay', 2, {}, {
        variableReads: reads,
        regexWorker: false,
        captureContext: {
            presetRegex: [],
            moduleRegexScripts: [],
            moduleAssets: [],
            dynamicAssets: false,
            dynamicAssetsEditDisplay: false,
            parserContext: {
                database: live.database,
                character: char,
                userName: 'User',
                personaPrompt: '',
                modules: [],
                moduleLorebooks: [],
                selectedCharID: 0,
                chatVariables: { hp: '7' },
                globalChatVariables: { hp: '7' },
                currentTime: 1,
            },
        },
    })
    const row = registerDisplayRow(() => 2)
    row.track('body', reads)
    rows.push(row)

    expect(data).toBe('turn 2 <b>HP 7</b>')
    expect(await refreshedRows({ keys: ['hp'] })).toEqual([2])
    expect(await refreshedRows({ keys: ['other'] })).toEqual(source === 'global' ? [2] : [])
})

test('keeps global variable dependencies when regex output is cached', async () => {
    const { char, chat } = installConversation([{
        ...statusPanel,
        out: '<b>Global {{getglobalvar::hp}}</b>',
    }])
    live.database.globalChatVariables = { hp: '7' }
    const first = await renderRow(char, chat, 0)
    expect(first.data).toBe('turn 0 <b>Global 7</b>\n')
    first.row.dispose()
    expect((await renderRow(char, chat, 0)).data).toBe('turn 0 <b>Global 7</b>\n')
    expect(await refreshedRows({ all: true })).toEqual([0])
})
