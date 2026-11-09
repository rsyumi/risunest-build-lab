import { afterEach, expect, it, vi } from 'vitest'
import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'

type Reply = { value: string } | { null: true } | { undefined: true } | { throw: string }
type Recorded = string | { length: number; sha256: string }
type LuaCall = { mode: string; data: Recorded; index: unknown }
// What a `@@repeat_back` read of the conversation found: the `lastChat` it matched, or what the walk threw.
type Read = { source: Recorded } | { undefined: true } | { error: string }
type OpenRead = { open: true }

// Long text is recorded by its UTF-16 length and the SHA-256 of its UTF-16LE code units.
function record(value: string): Recorded {
    return value.length <= 4096 ? value : { length: value.length, sha256: createHash('sha256').update(Buffer.from(value, 'utf16le')).digest('hex') }
}
type Tape = { lua?: Reply; luaCalls: LuaCall[]; applyLua?: () => void; reads: (Read | OpenRead)[] }

// Module-level effects read the database, so their roots start once the first synthetic database is set.
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

// Lua edit listeners are a host callback: runLuaEditTrigger keeps its own gating and the reply comes from the tape.
const tape = vi.hoisted(() => ({ current: null as Tape | null }))
vi.mock('../../src/ts/process/scriptings', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/scriptings')>()
    const runLuaEditTrigger: typeof actual.runLuaEditTrigger = async (char, mode, content, meta, ...rest) => {
        const listens = (char.triggerscript ?? []).some((trigger) => trigger?.effect?.[0]?.type === 'triggerlua')
        const run = tape.current
        if (mode === 'editprocess' || !listens || !run || typeof content !== 'string') {
            return actual.runLuaEditTrigger(char, mode, content, meta, ...rest)
        }
        run.luaCalls.push({ mode, data: record(content), index: (meta as { index?: unknown } | undefined)?.index })
        // The listeners' conversation writes, which the access commits whatever they return.
        run.applyLua?.()
        const reply = run.lua
        // runLuaEditTriggerBatch returns the content when a listener throws or returns nothing.
        if (!reply || !('value' in reply)) return content
        return reply.value as typeof content
    }
    return { ...actual, runLuaEditTrigger }
})

// A passthrough that marks where `@@repeat_back` starts its walk over the conversation (`scripts.ts`); the walk ends
// at the `match` operation on `lastChat` or at what it throws.
vi.mock('../../src/ts/process/historyWindowIndex', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/historyWindowIndex')>()
    const resolveHistoryWindowChat: typeof actual.resolveHistoryWindowChat = (chat) => {
        tape.current?.reads.push({ open: true })
        return actual.resolveHistoryWindowChat(chat)
    }
    return { ...actual, resolveHistoryWindowChat }
})

import { get } from 'svelte/store'
import { normalizeDatabaseDefaults, setDatabase, getDatabase, type Database, type character, type customscript } from '../../src/ts/storage/database.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { CharEmotion, selectedCharID } from '../../src/ts/stores.svelte'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { processScriptFull, resetScriptCache, type ProcessScriptCaptureContext, type ScriptMode } from '../../src/ts/process/scripts'
import { RegexExecutor } from '../../src/ts/process/regexExecutor'
import { setRuntimePerformanceProfile } from '../../src/ts/runtimePerformanceProfile'
import { readOracleCases, settleOracle } from './oracleFile'

type Script = { in: string; out: string; type: string; flag?: string; ableFlag?: boolean }
type Input = string | { repeat: string; times: number }
// Lua listener writes as the Lua host would make them, applied to the selected chat.
type LuaEffect = { setChat: [number, string] } | { addChat: ['user' | 'char', string] } | { removeChat: number } | { cutChat: [number, number?] }
type Run = {
    mode: ScriptMode; input: Input; chatIndex?: number; role?: string; lua?: Reply; luaEffects?: LuaEffect[]; hooks?: Reply[]; fill?: number
    unsupported?: string
}
type ChatSetup = { messages?: { role: 'user' | 'char'; data: string }[]; firstMessage?: string; alternateGreetings?: string[]; fmIndex?: number }
type Case = {
    id: string
    scripts: Script[]
    preset?: Script[]
    capture?: boolean
    lua?: boolean
    hooks?: Partial<Record<ScriptMode, number>>
    emotions?: [string, string][] | null
    priorEmotions?: [string, string, number][]
    profile?: 'low-spec'
    cache?: 'bypass'
    chat?: ChatSetup
    runs: Run[]
}
type HookCall = { mode: string; hook: number; data: Recorded }
type Outcome = { data: Recorded; emoChanged: boolean } | { error: string }
type Message = { role: string; data: Recorded }
type RunResult = Outcome & { lua: LuaCall[]; hooks: HookCall[]; emotions: [string, string, number][] | null; messages: Message[]; reads: Read[] }
type FillResult = { filled: string; emoChangedCount: number }

// The case file comes from regex-scripts.cases.mjs.
const set = readOracleCases<Case>('regex', 'scripts')
const now = 1_000

function inputOf(input: Input) { return typeof input === 'string' ? input : input.repeat.repeat(input.times) }

function toScript(script: Script): customscript {
    return { comment: '', in: script.in, out: script.out, type: script.type, flag: script.flag, ableFlag: script.ableFlag } as customscript
}

function prepare(entry: Case): character {
    const emotions = entry.emotions === undefined ? [] : entry.emotions
    const chat = entry.chat ?? {}
    const message = (chat.messages ?? []).map((value, index) => ({ ...value, chatId: `oracle-message-${index}` }))
    const draft = {
        chaId: 'oracle-character', name: 'Oracle', type: 'character', firstMessage: chat.firstMessage ?? '',
        alternateGreetings: chat.alternateGreetings ?? [],
        customscript: entry.scripts.map(toScript), emotionImages: emotions ?? [], additionalAssets: [], globalLore: [],
        triggerscript: entry.lua ? [{ comment: '', type: 'manual', conditions: [], effect: [{ type: 'triggerlua', code: '' }] }] : [],
        chats: [{ message, note: '', name: 'Chat', localLore: [], id: 'oracle-chat', fmIndex: chat.fmIndex ?? -1 }], chatPage: 0,
    }
    const database = normalizeDatabaseDefaults({ characters: [draft], presetRegex: (entry.preset ?? []).map(toScript) } as unknown as Database)
    characterFormatUpdate(database.characters[0])
    setDatabase(database)
    selectedCharID.set(0)
    startup.ready = true
    for (const activate of startup.roots.splice(0)) activate()
    const char = getDatabase().characters[0] as character
    if (entry.emotions === null) (char as { type: string }).type = 'simple'
    return char
}

function captureContext(char: character): ProcessScriptCaptureContext {
    return {
        presetRegex: getDatabase().presetRegex ?? [], moduleRegexScripts: [], moduleAssets: [], dynamicAssets: false, dynamicAssetsEditDisplay: false,
        parserContext: {
            database: getDatabase(), character: char, userName: 'User', personaPrompt: '', modules: [], moduleLorebooks: [],
            selectedCharID: 0, chatVariables: {}, globalChatVariables: {}, currentTime: now,
        },
    }
}

async function runOnce(entry: Case, char: character, run: Run, input: string): Promise<Outcome> {
    try {
        const result = await processScriptFull(char, input, run.mode, run.chatIndex ?? -1, run.role === undefined ? {} : { chatRole: run.role }, {
            regexWorker: false, cache: entry.cache, captureContext: entry.capture ? captureContext(char) : undefined,
        })
        return { data: record(result.data), emoChanged: result.emoChanged }
    } catch (error) {
        return { error: error instanceof Error ? error.message : String(error) }
    }
}

function applyLua(char: character, effects: LuaEffect[]) {
    const chat = char.chats[char.chatPage]
    for (const effect of effects) {
        if ('setChat' in effect) chat.message[effect.setChat[0]].data = effect.setChat[1]
        else if ('addChat' in effect) chat.message.push({ role: effect.addChat[0], data: effect.addChat[1], chatId: `lua-${chat.message.length}` })
        else if ('removeChat' in effect) chat.message.splice(effect.removeChat, 1)
        else chat.message = chat.message.slice(effect.cutChat[0], effect.cutChat[1])
    }
}

function closedReads(id: string, reads: (Read | OpenRead)[]): Read[] {
    return reads.map((read) => {
        if ('open' in read) throw new Error(`${id}: a conversation read that neither matched nor threw`)
        return read
    })
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    vi.restoreAllMocks()
})

it('records processScriptFull results for synthetic regex scripts and host tapes', async () => {
    // The rule's caller logs what a rule throws, including a `@@repeat_back` walk over a missing message.
    vi.spyOn(console, 'error').mockImplementation((error: unknown) => {
        const reads = tape.current?.reads
        if (reads?.length && 'open' in reads[reads.length - 1]) {
            reads[reads.length - 1] = { error: error instanceof Error ? error.message : String(error) }
        }
    })
    const scriptOperation = RegexExecutor.prototype.scriptOperation
    vi.spyOn(RegexExecutor.prototype, 'scriptOperation').mockImplementation(function (this: RegexExecutor, operation, key) {
        const reads = tape.current?.reads
        if (operation.kind === 'match' && reads?.length && 'open' in reads[reads.length - 1]) {
            // A missing greeting leaves `lastChat` undefined, which the `regex` helper's default parameter reads as the
            // rule input; the pattern has just failed on it, so the match finds nothing.
            const source: unknown = operation.input
            if (source !== undefined && typeof source !== 'string') throw new Error('a repeat_back source that is neither text nor undefined')
            reads[reads.length - 1] = typeof source === 'string' ? { source: record(source) } : { undefined: true }
        }
        return scriptOperation.call(this, operation, key)
    } as typeof scriptOperation)
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(Date, 'now').mockReturnValue(now)
    const results: (RunResult | FillResult)[][] = []
    for (const entry of set.cases) {
        setRuntimePerformanceProfile(entry.profile ?? 'normal')
        resetScriptCache()
        const char = prepare(entry)
        CharEmotion.set(entry.priorEmotions ? { [char.chaId]: structuredClone(entry.priorEmotions) } : {})
        let hookCalls: HookCall[] = []
        let hookReplies: Reply[] = []
        const registered: [ScriptMode, (data: string) => Promise<string | null | undefined>][] = []
        for (const [mode, count] of Object.entries(entry.hooks ?? {}) as [ScriptMode, number][]) {
            for (let hook = 0; hook < count; hook++) {
                const fn = async (data: string) => {
                    hookCalls.push({ mode, hook, data: record(data) })
                    const reply = hookReplies[hookCalls.length - 1]
                    if (!reply || 'undefined' in reply) return undefined
                    if ('null' in reply) return null
                    if ('throw' in reply) throw new Error(reply.throw)
                    return reply.value
                }
                pluginV2[mode].add(fn)
                registered.push([mode, fn])
            }
        }
        const caseResults: (RunResult | FillResult)[] = []
        try {
            for (const run of entry.runs) {
                if (run.fill !== undefined) {
                    const outputs: string[] = []
                    let emoChangedCount = 0
                    for (let index = 0; index < run.fill; index++) {
                        tape.current = { lua: run.lua, luaCalls: [], reads: [] }
                        hookCalls = []
                        hookReplies = run.hooks ?? []
                        const outcome = await runOnce(entry, char, run, `${inputOf(run.input)}${index}`)
                        if ('error' in outcome) throw new Error(`${entry.id}: fill run failed: ${outcome.error}`)
                        outputs.push(typeof outcome.data === 'string' ? outcome.data : outcome.data.sha256)
                        if (outcome.emoChanged) emoChangedCount++
                    }
                    caseResults.push({ filled: outputs.join('\u0001'), emoChangedCount })
                    continue
                }
                const effects = run.luaEffects
                tape.current = { lua: run.lua, luaCalls: [], reads: [], applyLua: effects ? () => applyLua(char, effects) : undefined }
                hookCalls = []
                hookReplies = run.hooks ?? []
                const outcome = await runOnce(entry, char, run, inputOf(run.input))
                const emotions = get(CharEmotion)[char.chaId] ?? null
                const messages = char.chats[char.chatPage].message.map((message) => ({ role: message.role, data: record(message.data) }))
                caseResults.push({
                    ...outcome, lua: tape.current.luaCalls, hooks: hookCalls, emotions: emotions ? structuredClone(emotions) : null, messages,
                    reads: closedReads(entry.id, tape.current.reads),
                })
            }
        } finally {
            tape.current = null
            for (const [mode, fn] of registered) pluginV2[mode].delete(fn)
        }
        results.push(caseResults)
    }
    setRuntimePerformanceProfile('normal')
    resetScriptCache()
    expect(results.length).toBe(set.cases.length)
    settleOracle(set, results, {
        generator: import.meta.url,
        tsSources: [
            'src/ts/process/scripts.ts', 'src/ts/process/regexExecutionPlan.ts', 'src/ts/process/regexOperations.ts',
            'src/ts/process/regexExecutor.ts', 'src/ts/process/historyWindowIndex.ts', 'src/ts/util/byteBudgetLru.ts',
        ],
    })
}, 600_000)
