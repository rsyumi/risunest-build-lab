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
        ScriptDisplayRebuildPointer: writable(0),
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
// Dynamic asset matching: every unknown asset name resolves to the one synthetic asset.
vi.mock('./memory/hypamemory', () => ({
    HypaProcesser: class {
        async addText() {}
        async similaritySearch() { return ['synthetic-portrait'] }
    },
}))
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
const { createScriptDisplayRefreshBatch, rebuildScriptDisplayRow, requestScriptDisplayRefresh } = await import('./scriptDisplayRefresh')
const { DisplayReadRecord, registerDisplayRow } = await import('./scriptVariableReads')
const { getChatVar, getGlobalChatVar } = await import('../parser/chatVar.svelte')
const { ReloadChatPointer, ScriptDisplayRebuildPointer, ScriptDisplayRefreshPointer } = await import('../stores.svelte')
const { runTrigger } = await import('./triggers')
const { installDisplayRecovery, isDisplayRecoveryMarked } = await import('../displayRecovery.svelte')
const { recoveryStore } = await import('../displayRecovery.testSupport')

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
    // An explicit reload reaches every mounted row.
    expect(await refreshedRows({ all: true })).toEqual([0, 1, 2])

    // A row rendered again from the cached regex result keeps its reads.
    rendered[2].row.dispose()
    expect((await renderRow(char, chat, 2)).data).toBe('turn 2 <b>HP 10</b>')
    expect(await refreshedRows({ keys: ['hp'] })).toEqual([2])
})

test('coalesces Worker transport tasks until a script boundary without capturing unrelated refreshes', async () => {
    const { char, chat } = installConversation([statusPanel])
    for (const index of [0, 1, 2]) await renderRow(char, chat, index)
    const batch = createScriptDisplayRefreshBatch()
    const before = { ...get(ReloadChatPointer) }
    for (let i = 0; i < 4; i++) {
        batch.run(() => requestScriptDisplayRefresh({ all: true }))
        await Promise.resolve()
    }
    expect(get(ReloadChatPointer)).toEqual(before)
    // Another invocation is free to refresh while this one waits for transport.
    expect(await refreshedRows({ keys: ['hp'] })).toEqual([2])
    const prior = { ...get(ReloadChatPointer) }
    batch.flush()
    await Promise.resolve()
    for (const index of [0, 1, 2]) expect(get(ReloadChatPointer)[index]).toBe((prior[index] ?? 0) + 1)
    const after = { ...get(ReloadChatPointer) }
    batch.flush()
    await Promise.resolve()
    expect(get(ReloadChatPointer)).toEqual(after)
    for (let i = 0; i < 4; i++) {
        batch.run(() => rebuildScriptDisplayRow(i % 2 ? '1' : 1))
        await Promise.resolve()
    }
    expect(get(ReloadChatPointer)).toEqual(after)
    batch.flush()
    expect(get(ReloadChatPointer)[1]).toBe((after[1] ?? 0) + 1)
})

test('rebuilds the rows an explicit reload reaches, and not the rows a variable change refreshes', async () => {
    const { char, chat } = installConversation([statusPanel])
    const rendered = []
    for (const index of [0, 1, 2]) rendered.push(await renderRow(char, chat, index))
    const revisions = () => rendered.map(({ row }) => row.rebuildRevision)
    const initial = revisions()
    const background = get(ScriptDisplayRebuildPointer)

    expect(await refreshedRows({ keys: ['hp'] })).toEqual([2])
    expect(revisions()).toEqual(initial)
    expect(get(ScriptDisplayRebuildPointer)).toBe(background)

    await refreshedRows({ all: true })
    const reloaded = revisions()
    expect(reloaded.map((revision, index) => revision !== initial[index])).toEqual([true, true, true])
    expect(get(ScriptDisplayRebuildPointer)).not.toBe(background)

    // A reload of one message rebuilds the rows at its index at once.
    const before = { ...get(ReloadChatPointer) }
    rebuildScriptDisplayRow(1)
    expect(get(ReloadChatPointer)[1]).not.toBe(before[1])
    expect(get(ReloadChatPointer)[0]).toBe(before[0])
    expect(revisions().map((revision, index) => revision !== reloaded[index])).toEqual([false, true, false])
})

test('keeps the reload and refresh scope of a chat opened without Modify Display, and its mark', async () => {
    const { char, chat } = installConversation([statusPanel])
    chat.message[1].data = 'turn 1 {{getvar::mood}} [상태창]'
    char.firstMessage = 'hello {{getvar::mood}} {{img::synthetic-portrai}} [상태창]'
    char.additionalAssets = [['synthetic-portrait', 'synthetic-portrait.png', 'png']]
    live.database.dynamicAssets = true
    live.database.dynamicAssetsEditDisplay = true
    const listened: string[] = []
    lua.displayListener = async (data) => { listened.push(data) }
    plugins.editdisplay.add(async (data) => `${data} plugin`)
    vi.mocked(runTrigger).mockClear()
    const target = { characterId: char.chaId, conversationId: chat.id! }
    const device = recoveryStore({ working: [target], marked: [] }, { commitAtOnce: true })
    await installDisplayRecovery(device.store)
    try {
        const rendered = []
        for (const index of [0, 1, 2]) rendered.push(await renderRow(char, chat, index))
        // The background, and a greeting rendered without a parser projection, take the same path.
        const background = await processScriptFull(char, 'background {{getvar::hp}} {{bg::synthetic-portrai}} [상태창]', 'editdisplay', -1, {}, { regexWorker: false })
        const greeting = await processScriptFull(char, char.firstMessage, 'editdisplay', -1, {}, {
            regexWorker: false,
            cache: 'bypass',
            captureContext: {
                presetRegex: [],
                moduleRegexScripts: [],
                moduleAssets: [],
                dynamicAssets: true,
                dynamicAssetsEditDisplay: true,
                parserContext: {
                    database: live.database,
                    character: char,
                    userName: 'User',
                    personaPrompt: '',
                    modules: [],
                    moduleLorebooks: [],
                    selectedCharID: 0,
                    chatVariables: { mood: 'calm' },
                    globalChatVariables: {},
                    currentTime: 1,
                },
            },
        })
        expect(rendered.map(({ data }) => data)).toEqual(['turn 0 [상태창]', 'turn 1 calm [상태창]', 'turn 2 [상태창]'])
        expect(background.data).toBe('background 10 {{bg::synthetic-portrai}} [상태창]')
        expect(greeting.data).toBe('hello calm {{img::synthetic-portrai}} [상태창]')
        const revisions = () => rendered.map(({ row }) => row.rebuildRevision)
        const initial = revisions()
        const backgroundRebuilds = get(ScriptDisplayRebuildPointer)
        const backgroundRefreshes = get(ScriptDisplayRefreshPointer)

        // A variable change refreshes the rows that read it and the background, without a rebuild.
        expect(await refreshedRows({ keys: ['mood'] })).toEqual([1])
        expect(await refreshedRows({ keys: ['hp'] })).toEqual([])
        expect(revisions()).toEqual(initial)
        expect(get(ScriptDisplayRebuildPointer)).toBe(backgroundRebuilds)
        expect(get(ScriptDisplayRefreshPointer)).toBe(backgroundRefreshes + 2)

        // A full reload rebuilds every row and the background.
        expect(await refreshedRows({ all: true })).toEqual([0, 1, 2])
        const reloaded = revisions()
        expect(reloaded.map((revision, index) => revision !== initial[index])).toEqual([true, true, true])
        expect(get(ScriptDisplayRebuildPointer)).toBe(backgroundRebuilds + 1)

        // A row reload by number or by string, as a clicked message uses, rebuilds that row.
        const before = { ...get(ReloadChatPointer) }
        rebuildScriptDisplayRow(1)
        rebuildScriptDisplayRow('2')
        expect(get(ReloadChatPointer)[1]).not.toBe(before[1])
        expect(get(ReloadChatPointer)['2' as never]).not.toBe(before['2' as never])
        expect(revisions().map((revision, index) => revision !== reloaded[index])).toEqual([false, true, true])

        // Rendering after the reloads still skips Modify Display, and the chat stays marked.
        rendered[1].row.dispose()
        expect((await renderRow(char, chat, 1)).data).toBe('turn 1 calm [상태창]')
        expect(listened).toEqual([])
        expect(runTrigger).not.toHaveBeenCalled()
        expect(isDisplayRecoveryMarked(target)).toBe(true)
        expect(device.record?.marked).toEqual([target])
    } finally {
        await installDisplayRecovery(null)
    }
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

test('compiled display reads preserve defaults, context updates and conditional dependencies', async () => {
    const { InMemoryWorker } = await import('../../../tests/support/regexWorker')
    const workerModule = await import('./regexWorkerClient')
    const worker = new InMemoryWorker()
    const client = new workerModule.RegexWorkerClient(() => worker)
    const factory = vi.spyOn(workerModule, 'getSharedRegexWorkerClient').mockReturnValue(client)
    try {
        const panel = { ...statusPanel,
            out: '{{#if {{equal::{{getvar::show}}::1}}}}<b>{{getvar::hp}}/{{getvar::fallback}}</b>{{/if}}',
        }
        const { char, chat } = installConversation([panel])
        char.defaultVariables = 'fallback=character\nshow=1'
        live.database.templateDefaultVariables = 'fallback=template'
        const firstReads = new DisplayReadRecord()
        const run = (variableReads: InstanceType<typeof DisplayReadRecord>) => processScriptFull(
            char, '[상태창]', 'editdisplay', 0, {}, { cache: 'bypass', regexWorker: true, variableReads },
        )
        expect((await run(firstReads)).data).toBe('<b>10/character</b>')
        expect([...firstReads.keys]).toEqual(['show', 'hp', 'fallback'])
        chat.scriptstate.$hp = '21'
        expect((await run(new DisplayReadRecord())).data).toBe('<b>21/character</b>')
        chat.scriptstate.$show = '0'
        const hiddenReads = new DisplayReadRecord()
        expect((await run(hiddenReads)).data).toBe('')
        expect([...hiddenReads.keys]).toEqual(['show'])
        // A revision replaces both the syntax and its declared variables.
        char.customscript[0].out = '{{getvar::mood}}'
        expect((await run(new DisplayReadRecord())).data).toBe('calm')
        expect(worker.requests.some((request) => request.type === 'execute' && request.cbsContext)).toBe(true)
    } finally { factory.mockRestore() }
})
