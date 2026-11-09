import { readFileSync } from 'node:fs'
import { setImmediate as immediate } from 'node:timers/promises'
import { get } from 'svelte/store'
import { afterEach, expect, it, vi } from 'vitest'

// Vitest's asynchronous module graph can schedule Svelte effects before cyclic imports initialize, so the module
// roots start once the imports and a synthetic database exist.
const startup = vi.hoisted(() => ({ roots: [] as (() => void)[], ready: false, dispose: [] as (() => void)[] }))
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (fn: () => void) => {
        if (startup.ready) return actual.effect_root(fn)
        let dispose: (() => void) | undefined
        startup.roots.push(() => { dispose = actual.effect_root(fn); startup.dispose.push(dispose) })
        return () => dispose?.()
    } }
})
// The conversation is held by an active session, so the trigger reaches it through the script access scope. Host
// boundaries (Lua, slash commands, model and image requests, embeddings, dialogs) are recorded, not executed: the native
// runner declines every case that reaches one.
const runtime = vi.hoisted(() => ({ session: null as unknown, hosts: [] as unknown[] }))
vi.mock('../../src/ts/storage/persistentDataRuntime.svelte', async (original) => ({
    ...await original<typeof import('../../src/ts/storage/persistentDataRuntime.svelte')>(),
    ...(await import('../../src/ts/process/tests/scriptingsTestHarness')).persistentRuntimeModule(() => runtime.session as never),
}))
vi.mock('../../src/ts/alert', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).alertModule())
vi.mock('../../src/ts/process/request/request', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).requestModule())
vi.mock('../../src/ts/process/stableDiff', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).stableDiffModule())
vi.mock('../../src/ts/process/files/inlays', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).inlaysModule())
vi.mock('../../src/ts/process/memory/hypamemory', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).hypamemoryModule())
vi.mock('../../src/ts/process/command', async () => (await import('../../src/ts/process/tests/scriptingsTestHarness')).commandModule())
vi.mock('../../src/ts/process/scriptings', async (original) => ({
    ...await original<typeof import('../../src/ts/process/scriptings')>(),
    runScripted: vi.fn(async (code: string, options: { chat: unknown }) => {
        runtime.hosts.push(['lua', code])
        return { stopSending: false, chat: options.chat }
    }),
}))
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'
import { ActiveConversationSession } from '../../src/ts/storage/activeConversationSession'
import { deriveEffectivePresetMirrors, getDatabase, normalizeDatabaseDefaults, setDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { CurrentTriggerIdStore, selectedCharID } from '../../src/ts/stores.svelte'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { ScriptConversationAccess } from '../../src/ts/process/scriptConversationAccess'
import { getHistoryMessageAt, toHistorySpliceIndex } from '../../src/ts/process/historyWindowIndex'
import { processMultiCommand } from '../../src/ts/process/command'
import { requestChatData } from '../../src/ts/process/request/request'
import { HypaProcesser } from '../../src/ts/process/memory/hypamemory'
import { alertNormal } from '../../src/ts/alert'
import { runTrigger } from '../../src/ts/process/triggers'

type Fields = { [key: string]: unknown }
type TriggerCase = OracleCase & {
    mode: 'start' | 'output' | 'input' | 'manual' | 'display' | 'request'
    manualName?: string
    triggerId?: string
    previousTriggerId?: string | null
    root?: Fields
    character?: Fields
    chat?: Fields
    random?: number[]
    // The decline the native runner gives instead of TS's result, when the case is outside what it runs.
    native?: `unsupported-${string}`
    // The native run declines while running, after an admission that passed.
    admitted?: true
    // Variable writes an earlier stage committed through the session before the trigger runs.
    earlier?: [string, unknown][]
    // Chat edits an earlier Lua run left in the stored chat before the trigger runs.
    earlierEdits?: EarlierEdit[]
    // The replies `requestChatData` gives, in call order.
    llm?: LlmReply[]
}
type LlmReply =
    | { type: 'success' | 'fail'; result: string }
    | { type: 'multiline'; result: [string, string][] }
    | { type: 'streaming'; chunks: string[] }
    | { type: 'throw'; message: string }
type Base = { root: Fields; character: Fields; chat: Fields }
type Message = { role: string; data: string; chatId: string }
// `removeChat`, `insertChat`, `setChatRole` and `setFullChatMain` (after its chatId merge, which the array already
// shows) as the Lua API makes them on the chat it writes (`src/ts/process/scriptings.ts:343-353`, `:511-556`,
// `:600-633`).
type EarlierEdit =
    | { remove: number }
    | { insert: [number, 'user' | 'char', string, string] }
    | { setRole: [number, 'user' | 'char'] }
    | { replace: Fields[] }
function applyEarlierEdit(chat: Parameters<typeof toHistorySpliceIndex>[0], edit: EarlierEdit) {
    if ('setRole' in edit) {
        const message = getHistoryMessageAt(chat, edit.setRole[0])
        if (message) message.role = edit.setRole[1]
        return
    }
    if ('replace' in edit) {
        chat.message = structuredClone(edit.replace) as never
        return
    }
    const start = toHistorySpliceIndex(chat, 'remove' in edit ? edit.remove : edit.insert[0])
    if (start === null) return
    if ('remove' in edit) chat.message.splice(start, 1)
    else chat.message.splice(start, 0, { role: edit.insert[1], data: edit.insert[2], chatId: edit.insert[3] })
}

const set = readOracleCases<TriggerCase>('triggers', 'run')
// The base records live beside the cases so the Rust replay merges them the same way.
const base = (JSON.parse(readFileSync(set.casesPath, 'utf8')) as { base: Base }).base

/** The database a case describes: base records shallowly overridden by the case's records. */
function databaseFor(entry: TriggerCase): Fields {
    const chat = { ...base.chat, ...entry.chat }
    const character = { ...base.character, ...entry.character, chats: [chat] }
    return structuredClone({ ...base.root, ...entry.root, characters: [character] })
}

const characterFields = ['chaId', 'triggerscript', 'lowLevelAccess', 'defaultVariables', 'firstMessage', 'alternateGreetings', 'modules']
const rootFields = ['templateDefaultVariables', 'modules', 'enabledModules', 'moduleIntergration', 'personas', 'aiModel',
    'currentPluginProvider', 'useTokenizerCaching']
// What the trigger reads must be what the case wrote, so the Rust replay can read the case directly.
function triggerView(database: Fields): Fields {
    const character = (database.characters as Fields[])[0]
    const chat = (character.chats as Fields[])[0]
    return {
        root: Object.fromEntries(rootFields.map(field => [field, database[field]])),
        character: Object.fromEntries(characterFields.map(field => [field, character[field]])),
        chat: { message: chat.message, scriptstate: chat.scriptstate, note: chat.note, fmIndex: chat.fmIndex, modules: chat.modules },
    }
}

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    vi.restoreAllMocks()
})

it('records real runTrigger runs over synthetic characters and chats', async () => {
    // Regex operations run on the main thread, as on a host without Worker support.
    vi.stubGlobal('Worker', undefined)
    vi.mocked(processMultiCommand).mockImplementation(async (command: string) => { runtime.hosts.push(['command', command]); return '' })
    // Each call takes the case's next scripted reply; a case that runs out throws.
    const llm: LlmReply[] = []
    vi.mocked(requestChatData).mockImplementation(async (arg, model) => {
        const { formated, ...options } = arg
        runtime.hosts.push(['llm', model, structuredClone(formated), structuredClone(options)])
        const reply = llm.shift()
        if (!reply) throw new Error('llm-script exhausted')
        if (reply.type === 'throw') throw new Error(reply.message)
        if (reply.type === 'streaming') {
            const result = new ReadableStream<{ [key: string]: string }>({
                start(controller) {
                    for (const chunk of reply.chunks) controller.enqueue({ 0: chunk })
                    controller.close()
                },
            })
            return { type: 'streaming', result }
        }
        return reply as never
    })
    // `similaritySearch` returns the added texts in reverse: the cases compare the trigger's side of the search (split,
    // documents, query, join, outputVar), not ranking or the embedding and cache flow.
    vi.mocked(HypaProcesser).mockImplementation(function () {
        let added: string[] = []
        return {
            addText: async (texts: string[]) => { added = texts.slice() },
            similaritySearch: async (query: string) => {
                runtime.hosts.push(['similarity', added.slice(), query])
                return added.slice().reverse()
            },
        } as never
    })
    vi.mocked(alertNormal).mockImplementation((text: string) => { runtime.hosts.push(['alert', text]) })
    const tokenizer: string[] = []
    const results: unknown[] = []
    for (const entry of set.cases) {
        const database = databaseFor(entry)
        // A case with a preset stores stale root mirrors; the load gives the effective ones from the selected preset.
        if (entry.root?.botPresets !== undefined) {
            normalizeDatabaseDefaults(database as unknown as Database)
            deriveEffectivePresetMirrors(database as unknown as Database)
        }
        const provider = String(database.currentPluginProvider)
        pluginV2.providerOptions.set(provider, {
            tokenizer: 'custom',
            tokenizerFunc: (text: string) => {
                tokenizer.push(text)
                return new Array<number>(Math.ceil(text.length / 4)).fill(0)
            },
        })
        const char = (database.characters as Fields[])[0] as never as Parameters<typeof runTrigger>[0]
        const chat = char.chats[0]
        // A stored `message` that is not an array is recorded as it is left.
        const rows = () => Array.isArray(chat.message) ? chat.message as Message[] : null
        const originalIds = new Set((rows() ?? []).map(message => message.chatId))
        // The stored chat is the one the database and the session both hold, so CBS and the effects read the edits.
        for (const edit of entry.earlierEdits ?? []) applyEarlierEdit(chat, edit)
        const expectedView = triggerView(structuredClone(database))
        setDatabase(database as unknown as Database)
        selectedCharID.set(0)
        if (!startup.ready) {
            startup.ready = true
            for (const activate of startup.roots.splice(0)) activate()
        }
        // Let the store effects settle before the tape starts.
        await immediate()
        expect(triggerView(getDatabase({ snapshot: true }) as unknown as Fields), entry.id).toEqual(expectedView)
        runtime.session = new ActiveConversationSession({ characterId: char.chaId, conversationId: chat.id!, conversation: chat, storeRevision: 1 })
        if (entry.earlier) (runtime.session as ActiveConversationSession).updateChatVariables(new Map(entry.earlier as [string, string][]))
        runtime.hosts.splice(0)
        tokenizer.splice(0)
        llm.splice(0, llm.length, ...structuredClone(entry.llm ?? []))
        CurrentTriggerIdStore.set(entry.previousTriggerId ?? null)
        const writes: [string, string | null][] = []
        const setVariable = ScriptConversationAccess.prototype.setVariable
        vi.spyOn(ScriptConversationAccess.prototype, 'setVariable').mockImplementation(function (this: ScriptConversationAccess, key: string, value: string) {
            const changed = setVariable.call(this, key, value)
            if (changed) writes.push([key, value === undefined ? null : value])
            return changed
        })
        const logged: unknown[] = []
        vi.spyOn(console, 'log').mockImplementation((...args: unknown[]) => { logged.push(args.map(String)) })
        const tape = entry.random ?? []
        let drawn = 0
        vi.spyOn(Math, 'random').mockImplementation(() => {
            if (drawn >= tape.length) throw new Error('rng-tape exhausted')
            return tape[drawn++]
        })
        const display = entry.mode === 'display' || entry.mode === 'request'
        let result: Awaited<ReturnType<typeof runTrigger>> | undefined
        let error: { name: string; message: string } | undefined
        try {
            result = await runTrigger(char, entry.mode, {
                chat,
                ...(entry.manualName !== undefined ? { manualName: entry.manualName } : {}),
                ...(entry.triggerId !== undefined ? { triggerId: entry.triggerId } : {}),
                ...(display ? { displayMode: true, displayData: entry.mode === 'request' ? '[]' : 'shown' } : {}),
            })
        } catch (thrown) {
            error = { name: (thrown as Error).name, message: (thrown as Error).message }
        }
        vi.mocked(Math.random).mockRestore()
        vi.mocked(console.log).mockRestore()
        vi.mocked(ScriptConversationAccess.prototype.setVariable).mockRestore()
        pluginV2.providerOptions.delete(provider)
        runtime.session = null
        const messages = rows()?.map(message => {
            const known = originalIds.has(message.chatId)
            if (!known) expect(message.chatId, entry.id).toMatch(uuid)
            return [message.role, message.data, known ? message.chatId : 'new']
        }) ?? chat.message
        results.push({
            returned: result === undefined ? 'undefined' : result === null ? 'null' : 'result',
            ...(result ? {
                systemPrompt: result.additonalSysPrompt, tokens: result.tokens, stopSending: result.stopSending,
                sendAIprompt: result.sendAIprompt,
            } : {}),
            writes,
            messages,
            note: chat.note ?? null,
            scriptstate: Object.entries(chat.scriptstate ?? {}).map(([key, value]) => [key, value === undefined ? null : value]),
            triggerId: get(CurrentTriggerIdStore),
            random: drawn,
            tokenizer: tokenizer.slice(),
            console: logged,
            hosts: runtime.hosts.slice(),
            ...(error ? { error } : {}),
        })
    }
    expect(results.length).toBeGreaterThanOrEqual(150)
    settleOracle(set, results, {
        generator: import.meta.url,
        tsSources: ['src/ts/process/triggers.ts', 'src/ts/process/scriptConversationAccess.ts', 'src/ts/process/historyWindowIndex.ts',
            'src/ts/process/regexOperations.ts', 'src/ts/process/modules.ts', 'src/ts/util.ts', 'src/ts/tokenizer.ts',
            'src/ts/storage/database.svelte.ts', 'src/ts/storage/effectiveIdentityState.ts'],
    })
})

type RouteCase = TriggerCase & { route: Fields }

// A contract, not a run of `runTrigger`: TS runs every one of these triggers, and the native Send route declines them
// because it answers no model request, acts on neither `stopSending` nor `sendAIprompt` yet, or cannot reproduce the
// engine's case tables. The decline comes before any host request or effect: in admission, or where a case marks it
// `admitted`, when the run reaches the text it would map.
it('records the route declines of the native trigger runner', () => {
    const contracts = readOracleCases<RouteCase>('triggers', 'route-declines')
    const results = contracts.cases.map(entry => entry.native === undefined
        ? { contract: 'route', decline: null }
        : { contract: 'route', decline: entry.native.slice('unsupported-'.length), phase: entry.admitted ? 'running' : 'admission' })
    expect(settleOracle(contracts, results, { generator: import.meta.url, tsSources: [] })).toMatch(/^(written|matched)$/)
})
