// @vitest-environment node
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { beforeAll, expect, it, vi } from 'vitest'
import { ActiveConversationSession } from '../../src/ts/storage/activeConversationSession'
import { getDatabase } from '../../src/ts/storage/database.svelte'
import { DBState } from '../../src/ts/stores.svelte'
import { getModuleTriggers } from '../../src/ts/process/modules'
import { luaCodeWrapper } from '../../src/ts/process/luaCodeWrapper'
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
    tsSources: ['src/ts/process/scriptings.ts', 'src/ts/process/luaCodeWrapper.ts', 'src/ts/process/luaRuntime.ts', 'public/lua/json.lua'],
}

type Text = string | { u: number[] }
type Tagged = ['undefined'] | ['null'] | ['bool', boolean] | ['number', string] | ['string', Text] | ['array', Tagged[]] | ['object', [Text, Tagged][]]
type Row = { role: string; data: string; chatId: string; [field: string]: unknown }
type EditCase = OracleCase & {
    mode: string; data: Text | object[]; index: number | null; triggers: string[]; modules: string[]
    rows: Row[]; scriptstate: Record<string, string>; calls: number; decline?: string
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
            const chat = { id: `lua-edit-chat-${entry.id}`, message: entry.rows.map(row => ({ ...row })), scriptstate: { ...entry.scriptstate }, localLore: [] }
            const trigger = (code: string) => ({ comment: '', type: 'start', conditions: [], effect: [{ type: 'triggerlua', code }], lowLevelAccess: false })
            const char = {
                type: 'character', chaId: `lua-edit-${entry.id}`, name: 'Beta', chatPage: 0, chats: [chat],
                triggerscript: entry.triggers.map(trigger), defaultVariables: '', globalLore: [], alternateGreetings: [],
            }
            const database = { characters: [char], templateDefaultVariables: '' }
            DBState.db = database as never
            vi.mocked(getDatabase).mockReturnValue(database as never)
            vi.mocked(getModuleTriggers).mockReturnValue(entry.modules.map(trigger) as never)
            runtime.chat = chat
            runtime.session = new ActiveConversationSession({ characterId: char.chaId, conversationId: chat.id, conversation: chat as never, storeRevision: 1 })
            const calls = []
            const ids = new Set<unknown>(entry.rows.map(row => row.chatId))
            try {
                for (let call = 0; call < entry.calls; call++) {
                    const data = entry.mode === 'editRequest' ? structuredClone(entry.data) : decodeText(entry.data as Text)
                    const meta = entry.index === null ? undefined : { index: entry.index }
                    const result = await scriptings.runLuaEditTrigger(char as never, entry.mode, data as never, meta)
                    calls.push({
                        data: entry.mode === 'editRequest' ? canonicalRequest(result) : tag(result),
                        scriptstate: Object.keys(chat.scriptstate).sort(byUnits).map(key => [units(key), tag((chat.scriptstate as Record<string, unknown>)[key])]),
                        messages: chat.message.map(row => storedMessage(row as never, ids)),
                    })
                }
            } finally {
                runtime.session = null
                runtime.chat = null
            }
            results.push({ calls })
        }
    } finally {
        errors.mockRestore()
        logs.mockRestore()
    }
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
