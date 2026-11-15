// @vitest-environment node
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { beforeAll, expect, it, vi } from 'vitest'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { IndexedDbPersistentDataStore } from '../../src/ts/storage/indexedDbPersistentDataStore'
import { canonicalClone } from '../../src/ts/storage/saveCoordinatorHelpers'
import { ActiveConversationSession } from '../../src/ts/storage/activeConversationSession'
import { getDatabase } from '../../src/ts/storage/database.svelte'
import { DBState } from '../../src/ts/stores.svelte'
import { getModuleTriggers } from '../../src/ts/process/modules'
import { luaCodeWrapper } from '../../src/ts/process/luaCodeWrapper'
import { ScriptConversationAccess } from '../../src/ts/process/scriptConversationAccess'
import { createConversationOperationContext } from '../../src/ts/process/conversationOperationContext'
import { getPersonaPrompt, parseKeyValue } from '../../src/ts/util'
import { risuChatParser } from '../../src/ts/parser/parser.svelte'
import { prepareNativePersistenceValue } from '../../src/ts/storage/nativePersistenceValue'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// Edit triggers run in the node environment, where Worker is undefined, so scriptings.ts uses its main-thread
// Wasmoon engine. The conversation is held by an active session, so Lua reaches it through the script access scope.
const runtime = vi.hoisted(() => ({ session: null as unknown, chat: null as unknown }))

vi.mock('../../src/ts/storage/persistentDataRuntime.svelte', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).persistentRuntimeModule(() => runtime.session as never))
vi.mock('../../src/ts/parser/parser.svelte', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).parserModule())
vi.mock('../../src/ts/alert', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).alertModule())
vi.mock('../../src/ts/globalApi.svelte', () => ({ fetchNative: vi.fn(), readImage: vi.fn() }))
vi.mock('../../src/ts/platform', async () => (await import('../../src/ts/platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))
vi.mock('../../src/ts/tokenizer', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).tokenizerModule())
vi.mock('../../src/ts/util', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).utilModule())
vi.mock('../../src/ts/storage/database.svelte', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).databaseModule({
    getCurrentChat: vi.fn(() => runtime.chat as never),
}))
vi.mock('../../src/ts/stores.svelte', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).storesModule())
vi.mock('../../src/ts/process/modules', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).modulesModule())
vi.mock('../../src/ts/process/files/inlays', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).inlaysModule())
vi.mock('../../src/ts/process/lorebook.svelte', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).lorebookModule())
vi.mock('../../src/ts/process/memory/hypamemory', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).hypamemoryModule())
vi.mock('../../src/ts/process/request/request', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).requestModule())
vi.mock('../../src/ts/process/stableDiff', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).stableDiffModule())
vi.mock('../../src/ts/process/command', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).commandModule())

const settle = {
    generator: import.meta.url,
    tsSources: ['src/ts/process/scriptings.ts', 'src/ts/process/luaCodeWrapper.ts', 'src/ts/process/luaRuntime.ts', 'public/lua/json.lua',
        'src/ts/process/scriptConversationAccess.ts', 'src/ts/storage/activeConversationSession.ts',
        'src/ts/storage/saveCoordinatorHelpers.ts', 'src/ts/storage/indexedDbPersistentDataStore.ts',
        'src/ts/process/conversationOperationContext.ts', 'src/ts/process/triggers.ts',
        'src/ts/parser/parser.svelte.ts', 'src/ts/parser/chatVar.svelte.ts', 'src/ts/util.ts', 'src/ts/storage/nativePersistenceValue.ts'],
}

type Text = string | { u: number[] }
type Tagged = ['undefined'] | ['null'] | ['bool', boolean] | ['number', string] | ['string', Text] | ['array', Tagged[]] | ['object', [Text, Tagged][]]
type Row = { role: string; data: unknown; chatId: string; [field: string]: unknown }
type ObservationChat = { id: string; message: Pick<Row, 'role' | 'data' | 'chatId'>[]; scriptstate: Record<string, unknown>; localLore: unknown[]; note?: unknown; authorNotePreset?: unknown }
type EditCase = OracleCase & {
    mode: string; data: Text | object[]; index: number | null; triggers: string[]; modules: string[]
    rows: Row[]; scriptstate: Record<string, string>; calls: number; decline?: string; persistence?: 'json-roundtrip' | 'native-json'; globals?: Record<string, unknown>; expectData?: string | string[]
    authorNote?: { note?: unknown; link?: unknown; presets?: unknown }
    owner?: 'access' | 'direct-operation'; finish?: 'normal' | 'without-messages'; bodies?: string[]; flushAfter?: number[]
    defaultVariables?: string; templateDefaultVariables?: string; locals?: Record<string, string>; personaPrompt?: string
    expectState?: [Text, Tagged][][]; expectStoredState?: [Text, Tagged][][]; expectMessages?: unknown[][]
}

function decodeText(text: Text) {
    return typeof text === 'string' ? text : String.fromCharCode(...text.u)
}
function wellFormed(text: string) {
    for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index)
        if (unit >= 0xdc00 && unit <= 0xdfff) return false
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = text.charCodeAt(index + 1)
            if (!(next >= 0xdc00 && next <= 0xdfff)) return false
            index++
        }
    }
    return true
}
function units(text: string): Text {
    if (wellFormed(text)) return text
    const result: number[] = []
    for (let index = 0; index < text.length; index++) result.push(text.charCodeAt(index))
    return { u: result }
}
const byUnits = (left: string, right: string) => left < right ? -1 : left > right ? 1 : 0
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

// A stored message as `[role, data, id, other fields]`: an id the case's rows held is kept, a version 4 UUID they did not
// hold is new (`insertChat`, `setFullChatMain`), and the other fields compare with sorted keys.
function storedMessage(row: Record<string, unknown>, ids: Set<unknown>) {
    const id = row.chatId
    const idTag = ids.has(id) ? ['kept', id] : typeof id === 'string' && uuid.test(id) ? ['new'] : ['other', tag(id)]
    const extra = Object.keys(row).filter(key => !['role', 'data', 'chatId'].includes(key)).sort(byUnits)
    return [tag(row.role), tag(row.data), idTag, extra.map(key => [units(key), tag(row[key])])]
}

// Lua `pairs` order decides key order after a listener, so objects compare with sorted keys.
function tag(value: unknown): Tagged {
    if (value === undefined) return ['undefined']
    if (value === null) return ['null']
    if (typeof value === 'boolean') return ['bool', value]
    if (typeof value === 'number') return ['number', Object.is(value, -0) ? '-0' : String(value)]
    if (typeof value === 'string') return ['string', units(value)]
    if (Array.isArray(value)) return ['array', value.map(tag)]
    if (typeof value === 'object') {
        return ['object', Object.keys(value).sort(byUnits).map(key => [units(key), tag((value as Record<string, unknown>)[key])])]
    }
    throw new Error(`untaggable ${typeof value}`)
}

// The canonical PromptMessage order (crates/core/ir/src/chat.rs `PromptMessage::to_value`), other keys sorted.
const promptFields = ['role', 'content', 'name', 'memo', 'attr', 'multimodals', 'thoughts', 'removable', 'cachePoint']
function canonicalRequest(value: unknown): Tagged {
    if (!Array.isArray(value)) return tag(value)
    return ['array', value.map(message => {
        if (typeof message !== 'object' || message === null || Array.isArray(message)) return tag(message)
        const record = message as Record<string, unknown>
        const known = promptFields.filter(key => key in record && record[key] !== undefined
            && !(key === 'multimodals' && Array.isArray(record[key]) && (record[key] as unknown[]).length === 0))
        const extra = Object.keys(record).filter(key => !promptFields.includes(key)).sort(byUnits)
        return ['object', [...known, ...extra].map(key => [units(key), tag(record[key])])] as Tagged
    })]
}

let scriptings: typeof import('../../src/ts/process/scriptings')
let luaRuntime: typeof import('../../src/ts/process/luaRuntime')

function variableTags(state: Record<string, unknown>) {
    return Object.keys(state).sort(byUnits).map(key => [units(key), tag(state[key])])
}

async function enableProductVariableHelpers(persona: boolean) {
    vi.doMock('@tauri-apps/api/webviewWindow', () => ({ getCurrentWebviewWindow: () => null }))
    const util = await vi.importActual<typeof import('../../src/ts/util')>('../../src/ts/util')
    vi.mocked(parseKeyValue).mockImplementation(util.parseKeyValue)
    vi.mocked(getPersonaPrompt).mockImplementation(util.getPersonaPrompt)
    if (persona) {
        vi.doMock('dompurify', async () => {
            const { Window } = await import('happy-dom')
            const { default: createDOMPurify } = await vi.importActual<typeof import('dompurify')>('dompurify')
            return { default: createDOMPurify(new Window() as unknown as Parameters<typeof createDOMPurify>[0]) }
        })
        const parser = await vi.importActual<typeof import('../../src/ts/parser/parser.svelte')>('../../src/ts/parser/parser.svelte')
        vi.mocked(risuChatParser).mockImplementation(parser.risuChatParser)
    }
}

beforeAll(async () => {
    const jsonLuaSource = await readFile(resolve(process.cwd(), 'public/lua/json.lua'), 'utf8')
    vi.stubGlobal('fetch', vi.fn(async () => new Response(jsonLuaSource, { status: 200 })))
    scriptings = await import('../../src/ts/process/scriptings')
    luaRuntime = await import('../../src/ts/process/luaRuntime')
})

it('main-thread Wasmoon pushes JS strings into Lua with the pinned encoder', async () => {
    expect(typeof Worker).toBe('undefined')
    const set = readOracleCases<OracleCase & { text: Text }>('lua', 'encode')
    const engine = await (await luaRuntime.createLuaFactory()).createEngine({ injectObjects: true, enableProxy: false })
    try {
        await engine.doString('function bytes(value) local out = {} for i = 1, #value do out[i] = string.byte(value, i) end return table.concat(out, ",") end')
        const bytes = engine.global.get('bytes') as (value: string) => string
        const results = set.cases.map(entry => {
            const text = bytes(decodeText(entry.text))
            return text === '' ? [] : text.split(',').map(Number)
        })
        expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
    } finally {
        engine.global.close()
    }
})

it('luaCodeWrapper text is what the native host reads from its source', () => {
    const set = readOracleCases<OracleCase>('lua', 'wrapper')
    const results = set.cases.map(() => createHash('sha256').update(luaCodeWrapper(''), 'utf8').digest('hex'))
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})

it('runLuaEditTrigger batches across modes', async () => {
    const set = readOracleCases<EditCase>('lua', 'edit')
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    const logs = vi.spyOn(console, 'log').mockImplementation(() => undefined)
    const results: unknown[] = []
    try {
        for (const entry of set.cases) {
            if (Array.isArray(entry.expectData)) expect(entry.expectData).toHaveLength(entry.calls)
            if (entry.owner) {
                expect(entry.bodies).toHaveLength(entry.calls)
                expect(entry.expectState).toHaveLength(entry.calls)
                expect(entry.expectMessages).toHaveLength(entry.calls)
                expect(entry.flushAfter ?? []).not.toContain(entry.calls - 1)
            }
            let chat: ObservationChat = { id: `lua-edit-chat-${entry.id}`, message: entry.rows.map(row => ({ ...row })), scriptstate: { ...entry.scriptstate }, localLore: [] }
            if (entry.authorNote) {
                if ('note' in entry.authorNote) chat.note = entry.authorNote.note
                if ('link' in entry.authorNote) chat.authorNotePreset = entry.authorNote.link
            }
            const trigger = (code: string) => ({ comment: '', type: 'start', conditions: [], effect: [{ type: 'triggerlua', code }], lowLevelAccess: false })
            const char = {
                type: 'character', chaId: `lua-edit-${entry.id}`, name: 'Beta', chatPage: 0, chats: [chat],
                triggerscript: entry.triggers.map(trigger), defaultVariables: entry.defaultVariables ?? '', globalLore: [], alternateGreetings: [],
            }
            const database = { characters: [char], templateDefaultVariables: entry.templateDefaultVariables ?? '', ...(entry.personaPrompt === undefined ? {} : { personaPrompt: entry.personaPrompt }), ...(entry.globals === undefined ? {} : { globalChatVariables: entry.globals }),
                ...(entry.authorNote && 'presets' in entry.authorNote ? { authorNotePresets: entry.authorNote.presets } : {}) }
            const store = entry.persistence !== undefined
                ? new IndexedDbPersistentDataStore(`lua-oracle-${entry.id}`, new IDBFactory(), IDBKeyRange) : null
            let revision = 1
            let persistedMessageCount = chat.message.length
            if (store) {
                await store.open()
                revision = (await store.replaceFromDatabase(canonicalClone(database) as never)).revision
            }
            DBState.db = database as never
            vi.mocked(getDatabase).mockReturnValue(database as never)
            vi.mocked(getModuleTriggers).mockReturnValue(entry.modules.map(trigger) as never)
            runtime.chat = chat
            runtime.session = new ActiveConversationSession({ characterId: char.chaId, conversationId: chat.id, conversation: chat as never, storeRevision: 1 })
            const calls = []
            const ids = new Set<unknown>(entry.rows.map(row => row.chatId))
            const session = runtime.session as ActiveConversationSession
            const access = entry.owner === 'access' ? new ScriptConversationAccess({ session, chat: chat as never,
                database: database as never, characterId: char.chaId }) : null
            const operation = entry.owner === 'direct-operation' ? createConversationOperationContext(session, chat as never) : null
            if (entry.owner) await enableProductVariableHelpers(entry.locals !== undefined || entry.personaPrompt !== undefined)
            try {
                for (let call = 0; call < entry.calls; call++) {
                    const data = entry.mode === 'editRequest' ? structuredClone(entry.data) : decodeText(entry.data as Text)
                    const meta = entry.index === null ? undefined : { index: entry.index }
                    let result: unknown
                    let beforeFinish: { scriptstate: unknown; messages: unknown } | undefined
                    if (entry.owner) {
                        const code = `function onStart(id) ${entry.bodies![call]} end`
                        if (entry.locals) {
                            expect(access).not.toBeNull()
                            const { runTrigger } = await import('../../src/ts/process/triggers')
                            const localCharacter = { ...char, triggerscript: [{ comment: 'raw-key-locals', type: 'start', conditions: [],
                                lowLevelAccess: false, effect: [...Object.entries(entry.locals).map(([name, value]) => ({
                                    type: 'v2DeclareLocalVar', var: name, value, valueType: 'value', indent: 0,
                                })), { type: 'triggerlua', code }] }] }
                            await runTrigger(localCharacter as never, 'start', { chat: chat as never, access: access! })
                            result = access!.getVariable('completed')
                        } else {
                            result = (await scriptings.runScripted(code, { char: char as never, chat: chat as never, mode: 'start',
                                ...(access ? { access } : { operationContext: operation! }) })).res
                        }
                        beforeFinish = { scriptstate: variableTags(chat.scriptstate), messages: chat.message.map(row => storedMessage(row as never, ids)) }
                        if (entry.flushAfter?.includes(call)) {
                            expect(access).not.toBeNull()
                            access!.flushVariables()
                        }
                        if (call === entry.calls - 1) {
                            if (access) {
                                if (entry.finish === 'without-messages') access.commitWithoutMessages()
                                else access.commit()
                            } else if (entry.finish === 'without-messages') operation!.commitMetadata(session)
                            else operation!.commit(session)
                        }
                    } else result = await scriptings.runLuaEditTrigger(char as never, entry.mode, data as never, meta)
                    if (entry.expectData !== undefined) expect(tag(result)).toEqual(['string',
                        Array.isArray(entry.expectData) ? entry.expectData[call] : entry.expectData])
                    const observation: Record<string, unknown> = {
                        data: entry.mode === 'editRequest' ? canonicalRequest(result) : tag(result),
                        scriptstate: variableTags(chat.scriptstate),
                        messages: chat.message.map(row => storedMessage(row as never, ids)),
                    }
                    if (beforeFinish) observation.beforeFinish = beforeFinish
                    if (entry.expectState) {
                        expect(observation.scriptstate).toStrictEqual(entry.expectState[call])
                        for (const [key, value] of entry.expectState[call]) if (value[0] === 'undefined') {
                            const name = decodeText(key)
                            expect(Object.hasOwn(chat.scriptstate, name)).toBe(true)
                            expect(chat.scriptstate[name]).toBeUndefined()
                        }
                    }
                    if (entry.expectMessages) expect(chat.message.map(row => row.data)).toStrictEqual(entry.expectMessages[call])
                    if (entry.authorNote) {
                        observation.authorNote = [tag(chat.note), tag(chat.authorNotePreset)]
                        expect(observation.authorNote).toEqual([tag(entry.authorNote.note), tag(entry.authorNote.link)])
                    }
                    if (store) {
                        const { message, ...conversation } = chat
                        const input = { expectedRevision: revision, conversations: [{ type: 'replace-range',
                            characterId: char.chaId, conversationId: chat.id, start: 0, deleteCount: persistedMessageCount,
                            messages: message, conversation }] }
                        const persisted = canonicalClone(entry.persistence === 'native-json' ? prepareNativePersistenceValue(input) : input)
                        revision = (await store.commit(persisted as never)).revision
                        const stored = await store.readConversation(char.chaId, chat.id)
                        expect(stored).not.toBeNull()
                        const state = stored!.value.scriptstate ?? {}
                        observation.storedScriptstate = variableTags(state)
                        if (entry.expectStoredState) expect(observation.storedScriptstate).toStrictEqual(entry.expectStoredState[call])
                        if (!entry.owner) chat = stored!.value as typeof chat
                        if (entry.authorNote) expect([tag(chat.note), tag(chat.authorNotePreset)])
                            .toEqual([tag(entry.authorNote.note), tag(entry.authorNote.link)])
                        persistedMessageCount = chat.message.length
                        if (!entry.owner) {
                            char.chats[0] = chat
                            runtime.chat = chat
                            runtime.session = new ActiveConversationSession({ characterId: char.chaId, conversationId: chat.id, conversation: chat as never, storeRevision: revision })
                        }
                    }
                    calls.push(observation)
                }
            } finally {
                operation?.release()
                runtime.session = null
                runtime.chat = null
            }
            results.push({ calls })
        }
    } finally {
        errors.mockRestore()
        logs.mockRestore()
    }
    expect(settleOracle(set, results, { ...settle, tsSources: [...settle.tsSources, 'src/ts/authorNotePresets.ts'] })).toMatch(/written|matched/)
}, 60000)

function rawKeyReference(id: string, state: Record<string, unknown> = {}) {
    const chat: ObservationChat = { id: `raw-key-${id}`, message: [{ role: 'user', data: 'base', chatId: 'original' }], scriptstate: state, localLore: [] }
    const char = { type: 'character', chaId: `raw-key-${id}`, name: 'Beta', chatPage: 0, chats: [chat],
        triggerscript: [], defaultVariables: '', globalLore: [], alternateGreetings: [] }
    const database = { characters: [char], templateDefaultVariables: '' }
    DBState.db = database as never
    vi.mocked(getDatabase).mockReturnValue(database as never)
    runtime.chat = chat
    const session = new ActiveConversationSession({ characterId: char.chaId, conversationId: chat.id, conversation: chat as never, storeRevision: 1 })
    runtime.session = session
    const access = new ScriptConversationAccess({ session, chat: chat as never, database: database as never, characterId: char.chaId })
    return { chat, char, database, access }
}

it('guards the Wasmoon table-key error artifact separately from the accepted native catchable-error contract', async () => {
    await enableProductVariableHelpers(false)
    const { chat, char, access } = rawKeyReference('tostring', { $7: 'B' })
    try {
        const result = await scriptings.runScripted(`function onStart(id)
            local key=setmetatable({}, {__tostring=function()
                setChatVar(id,7,'numeric') setChatVar(id,'7','string') error('raw-keybad')
            end})
            local ok,err=pcall(setChatVar,id,'outer',{[key]=1})
            setChatVar(id,'after','kept')
            return json.encode({ok=ok,error=tostring(err),num=getChatVar(id,7),text=getChatVar(id,'7'),outer=getChatVar(id,'outer')})
        end`, { char: char as never, chat: chat as never, access, mode: 'start' })
        expect(typeof result.res).toBe('string')
        const diagnostic = JSON.parse(result.res as string)
        expect(diagnostic).toMatchObject({ ok: true, error: 'nil', num: 'numeric', text: 'string' })
        const keys = Object.keys(diagnostic.outer)
        expect(keys).toHaveLength(1)
        expect(keys[0]).toContain('raw-keybad')
        expect(diagnostic.outer[keys[0]]).toBe(1)
        access.commit()
        expect(chat.scriptstate.$7).toBe('string')
        expect(chat.scriptstate.$after).toBe('kept')
        expect(chat.scriptstate.$outer).toStrictEqual(diagnostic.outer)
    } finally {
        runtime.session = null
        runtime.chat = null
    }
})

it('checks native Unicode key collisions before undefined omission and preserves the durable baseline', async () => {
    await enableProductVariableHelpers(false)
    for (const omitted of [false, true]) {
        const key = '$' + String.fromCharCode(0xd800)
        const { chat, char, database, access } = rawKeyReference(`unicode-collision-${omitted}`, { [key]: 'old' })
        const store = new IndexedDbPersistentDataStore(`lua-native-collision-${omitted}`, new IDBFactory(), IDBKeyRange)
        await store.open()
        await store.replaceFromDatabase(canonicalClone(prepareNativePersistenceValue(database)) as never)
        const before = await store.readConversation(char.chaId, chat.id)
        try {
            const result = await scriptings.runScripted(`function onStart(id)
                local key=string.char(237,160,128)
                ${omitted ? "setChatVar(id,key)" : "setChatVar(id,key,'changed')"}
                setChatVar(id,utf8.char(65533),'present')
                return 'collision:done'
            end`, { char: char as never, chat: chat as never, access, mode: 'start' })
            expect(result.res).toBe('collision:done')
            access.commit()
            expect(Object.hasOwn(chat.scriptstate, key)).toBe(true)
            expect(chat.scriptstate[key]).toBe(omitted ? undefined : 'changed')
            expect(chat.scriptstate['$\uFFFD']).toBe('present')
            expect(() => prepareNativePersistenceValue(chat)).toThrow('Unicode replacement would merge object keys')
            expect(await store.readConversation(char.chaId, chat.id)).toStrictEqual(before)
            expect(Object.hasOwn(chat.scriptstate, key)).toBe(true)
            expect(chat.scriptstate[key]).toBe(omitted ? undefined : 'changed')
        } finally {
            runtime.session = null
            runtime.chat = null
        }
    }
})
