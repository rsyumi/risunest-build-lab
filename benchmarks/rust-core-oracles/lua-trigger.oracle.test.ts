// @vitest-environment node
import { readFile } from 'node:fs/promises'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { beforeAll, expect, it, vi } from 'vitest'
import { ActiveConversationSession } from '../../src/ts/storage/activeConversationSession'
import { getDatabase } from '../../src/ts/storage/database.svelte'
import { DBState } from '../../src/ts/stores.svelte'
import { getModuleTriggers } from '../../src/ts/process/modules'
import { loadLoreBookV3PromptFromCompatibilitySnapshot } from '../../src/ts/process/lorebook.svelte'
import { getPersonaPrompt, parseKeyValue } from '../../src/ts/util'
import { risuChatParser } from '../../src/ts/parser/parser.svelte'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// The start triggers of the Lua rows of the Ollama Send oracle (`benchmarks/generation-send/ollama-oracle.test.ts`), run
// by the TS Lua host as a send runs them: `runTrigger(character, 'start')` over the conversation an active session holds,
// in the node environment, where Worker is undefined, so scriptings.ts uses its main-thread Wasmoon engine.
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
    tsSources: ['src/ts/process/triggers.ts', 'src/ts/process/scriptings.ts', 'src/ts/process/luaCodeWrapper.ts', 'src/ts/process/luaRuntime.ts',
        'public/lua/json.lua', 'src/ts/process/scriptConversationAccess.ts', 'src/ts/storage/activeConversationSession.ts',
        'src/ts/parser/parser.svelte.ts', 'src/ts/parser/chatVar.svelte.ts', 'src/ts/util.ts'],
}

type Tagged = ['undefined'] | ['null'] | ['bool', boolean] | ['number', string] | ['string', string] | ['array', Tagged[]] | ['object', [string, Tagged][]]
type Row = { role: string; data: string; chatId: string; [field: string]: unknown }
type Trigger = { effect?: { type: string }[]; lowLevelAccess?: boolean; [field: string]: unknown }
// `modules` are the selected modules' triggers as `getModuleTriggers` returns them; `request` is the array an `editRequest`
// listener edits after the start trigger ran.
type TriggerCase = OracleCase & {
    triggers: Trigger[]; lowLevelAccess: boolean; modules: Trigger[]; rows: Row[]; scriptstate: Record<string, unknown>; note: string
    request?: { role: string; content: string }[]
}
type SendRow = { id: string; character?: { triggerscript?: Trigger[]; lowLevelAccess?: boolean }
    root?: { modules?: { id: string; trigger?: Trigger[]; lowLevelAccess?: boolean }[]; enabledModules?: string[] } }

const byUnits = (left: string, right: string) => left < right ? -1 : left > right ? 1 : 0
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

// Lua `pairs` order decides key order after a run, so objects compare with sorted keys.
function tag(value: unknown): Tagged {
    if (value === undefined) return ['undefined']
    if (value === null) return ['null']
    if (typeof value === 'boolean') return ['bool', value]
    if (typeof value === 'number') return ['number', Object.is(value, -0) ? '-0' : String(value)]
    if (typeof value === 'string') return ['string', value]
    if (Array.isArray(value)) return ['array', value.map(tag)]
    if (typeof value === 'object') return ['object', Object.keys(value).sort(byUnits).map(key => [key, tag((value as Record<string, unknown>)[key])])]
    throw new Error(`untaggable ${typeof value}`)
}

// A message as `[role, data, id, other fields]`: an id the case's rows held is kept, a version 4 UUID they did not hold is
// new (`addChat`, `insertChat`, `setFullChatMain`).
function message(row: Record<string, unknown>, ids: Set<unknown>) {
    const id = row.chatId
    const idTag = ids.has(id) ? ['kept', id] : typeof id === 'string' && uuid.test(id) ? ['new'] : ['other', tag(id)]
    const extra = Object.keys(row).filter(key => !['role', 'data', 'chatId'].includes(key)).sort(byUnits)
    return [tag(row.role), tag(row.data), idTag, extra.map(key => [key, tag(row[key])])]
}

// What the run left in the conversation the session holds: variables with sorted keys, the author note and the history.
function conversation(chat: { scriptstate: Record<string, unknown>; note?: unknown; message: Row[] }, ids: Set<unknown>) {
    return { scriptstate: Object.keys(chat.scriptstate).sort(byUnits).map(key => [key, tag(chat.scriptstate[key])]),
        note: tag(chat.note), messages: chat.message.map(row => message(row, ids)) }
}

// The canonical PromptMessage order (crates/core/ir/src/chat.rs `PromptMessage::to_value`), other keys sorted.
const promptFields = ['role', 'content', 'name', 'memo', 'attr', 'multimodals', 'thoughts', 'removable', 'cachePoint']
function canonicalRequest(value: unknown): Tagged {
    if (!Array.isArray(value)) return tag(value)
    return ['array', value.map(entry => {
        if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) return tag(entry)
        const record = entry as Record<string, unknown>
        const known = promptFields.filter(key => key in record && record[key] !== undefined
            && !(key === 'multimodals' && Array.isArray(record[key]) && (record[key] as unknown[]).length === 0))
        const extra = Object.keys(record).filter(key => !promptFields.includes(key)).sort(byUnits)
        return ['object', [...known, ...extra].map(key => [key, tag(record[key])])] as Tagged
    })]
}

let scriptings: typeof import('../../src/ts/process/scriptings')
let triggers: typeof import('../../src/ts/process/triggers')

beforeAll(async () => {
    const jsonLuaSource = await readFile(resolve(process.cwd(), 'public/lua/json.lua'), 'utf8')
    vi.stubGlobal('fetch', vi.fn(async () => new Response(jsonLuaSource, { status: 200 })))
    // Trigger effects and the Lua host parse CBS with the product parser and read key-value defaults with the product helper.
    vi.doMock('@tauri-apps/api/webviewWindow', () => ({ getCurrentWebviewWindow: () => null }))
    vi.doMock('dompurify', async () => {
        const { Window } = await import('happy-dom')
        const { default: createDOMPurify } = await vi.importActual<typeof import('dompurify')>('dompurify')
        return { default: createDOMPurify(new Window() as unknown as Parameters<typeof createDOMPurify>[0]) }
    })
    const util = await vi.importActual<typeof import('../../src/ts/util')>('../../src/ts/util')
    vi.mocked(parseKeyValue).mockImplementation(util.parseKeyValue)
    vi.mocked(getPersonaPrompt).mockImplementation(util.getPersonaPrompt)
    const parser = await vi.importActual<typeof import('../../src/ts/parser/parser.svelte')>('../../src/ts/parser/parser.svelte')
    vi.mocked(risuChatParser).mockImplementation(parser.risuChatParser)
    // The Send rows' characters, conversations and modules hold no lorebook.
    vi.mocked(loadLoreBookV3PromptFromCompatibilitySnapshot).mockResolvedValue({ actives: [] } as never)
    scriptings = await import('../../src/ts/process/scriptings')
    triggers = await import('../../src/ts/process/triggers')
})

it('runs the start triggers of the Lua Send rows through the TS Lua host', async () => {
    const set = readOracleCases<TriggerCase>('lua', 'trigger')
    const send = JSON.parse(readFileSync(resolve('tests/fixtures/native-generation/ollama-text.json'), 'utf8')) as { cases: SendRow[]; effects: SendRow[] }
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    const logs = vi.spyOn(console, 'log').mockImplementation(() => undefined)
    const results: unknown[] = []
    try {
        for (const entry of set.cases) {
            // A case runs the triggers of the Send row it is named after (`lua-trigger.cases.mjs` copies them).
            const row = [...send.cases, ...send.effects].find(candidate => candidate.id === entry.id)
            expect(row, entry.id).toBeDefined()
            expect(entry.triggers, entry.id).toEqual(row!.character?.triggerscript ?? [])
            expect(entry.lowLevelAccess, entry.id).toBe(row!.character?.lowLevelAccess ?? false)
            const selected = (row!.root?.modules ?? []).filter(module => (row!.root?.enabledModules ?? []).includes(module.id))
            expect(entry.modules, entry.id).toEqual(selected.flatMap(module => (module.trigger ?? []).map(trigger =>
                JSON.parse(JSON.stringify({ ...trigger, lowLevelAccess: module.lowLevelAccess })))))
            expect([...entry.triggers, ...entry.modules].some(trigger => trigger.effect?.some(effect => effect.type === 'triggerlua')), entry.id).toBe(true)

            const chat = { id: `lua-trigger-chat-${entry.id}`, message: structuredClone(entry.rows), scriptstate: structuredClone(entry.scriptstate),
                note: entry.note, localLore: [] }
            const char = { type: 'character', chaId: `lua-trigger-${entry.id}`, name: 'Synthetic character', chatPage: 0, chats: [chat],
                triggerscript: structuredClone(entry.triggers), lowLevelAccess: entry.lowLevelAccess, defaultVariables: '', globalLore: [],
                alternateGreetings: [] }
            const database = { characters: [char], templateDefaultVariables: '' }
            DBState.db = database as never
            vi.mocked(getDatabase).mockReturnValue(database as never)
            vi.mocked(getModuleTriggers).mockReturnValue(structuredClone(entry.modules) as never)
            runtime.chat = chat
            runtime.session = new ActiveConversationSession({ characterId: char.chaId, conversationId: chat.id, conversation: chat as never, storeRevision: 1 })
            const ids = new Set<unknown>(entry.rows.map(message => message.chatId))
            const result: Record<string, unknown> = {}
            try {
                // The send's start trigger: it owns the conversation access, commits it when the run ends, and commits
                // variables and the author note without messages when the run throws.
                errors.mockClear()
                let start: { threw: boolean; stopSending: boolean }
                try {
                    const outcome = await triggers.runTrigger(char as never, 'start', { chat: chat as never })
                    start = { threw: false, stopSending: outcome?.stopSending ?? false }
                } catch {
                    start = { threw: true, stopSending: false }
                }
                // `errors` counts what the host logged and continued past, such as a Lua runtime error.
                result.start = { ...start, errors: errors.mock.calls.length, ...conversation(chat, ids) }
                if (entry.request) {
                    errors.mockClear()
                    const data = await scriptings.runLuaEditTrigger(char as never, 'editRequest', structuredClone(entry.request) as never)
                    result.editRequest = { data: canonicalRequest(data), errors: errors.mock.calls.length, ...conversation(chat, ids) }
                }
            } finally {
                runtime.session = null
                runtime.chat = null
            }
            results.push(result)
        }
    } finally {
        errors.mockRestore()
        logs.mockRestore()
    }
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
}, 60000)
