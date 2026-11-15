import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs'
import { setImmediate as immediate } from 'node:timers/promises'
import { readOracleCases, settleOracle } from '../rust-core-oracles/oracleFile'

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
const alerts = vi.hoisted(() => ({ errors: [] as string[] }))
vi.mock('../../src/ts/alert', async (original) => {
    const actual = await original<typeof import('../../src/ts/alert')>()
    return { ...actual, alertError: (message: string | Error) => { alerts.errors.push(String(message)) } } satisfies typeof actual
})
vi.mock('../../src/ts/process/luaRuntime', async (original) => (await import('./happyDomLuaRuntime')).luaRuntimeWithNodeLoader(original))

import { deriveEffectivePresetMirrors, normalizeDatabaseDefaults, setDatabase, getDatabase, type Chat, type Database } from '../../src/ts/storage/database.svelte'
import { deriveEffectivePersonaMirrors } from '../../src/ts/storage/effectiveIdentityState'
import { characterFormatUpdate } from '../../src/ts/characters'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { getPersistentDataStore } from '../../src/ts/storage/persistentDataStoreFactory'
import { getPersistentDataRuntime } from '../../src/ts/storage/persistentDataRuntime.svelte'
import { projectCompleteScalableWorkingSet } from '../../src/ts/storage/workingSetCatalog'
import { sendChat } from '../../src/ts/process/index.svelte'
import { reserveGeneration } from '../../src/ts/process/generationState'
import { resetScriptCache } from '../../src/ts/process/scripts'
import { tokenize } from '../../src/ts/tokenizer'
import { safeStructuredClone } from '../../src/ts/safeStructuredClone'
import { luaJsonModuleResponse } from './happyDomLuaRuntime'

type RecordValue = Record<string, unknown>
type Step = { phase: 'summary' | 'main'; content?: string; status?: number; error?: string }
type Entry = { id: string; mode: 'supa' | 'hypav3'; root?: RecordValue; preset?: RecordValue; conversation?: RecordValue;
    steps: Step[]; expected: 'success' | 'main-failure' | 'memory-failure'; summaryCount: number }
type Base = { root: RecordValue; preset: RecordValue; character: RecordValue; conversation: RecordValue; messages: unknown[] }
const set = readOracleCases<Entry>('memory', 'send')
const base = (JSON.parse(readFileSync(set.casesPath, 'utf8')) as { base: Base }).base
const memoryFields = ['supaMemoryData', 'hypaV2Data', 'hypaV3Data', 'lastMemory'] as const

function encoded(value: unknown): unknown {
    if (value === undefined) return { js: 'undefined' }
    if (typeof value === 'number' && (!Number.isFinite(value) || Object.is(value, -0))) return { js: Object.is(value, -0) ? '-0' : String(value) }
    if (Array.isArray(value)) return Array.from(value, encoded)
    if (value !== null && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encoded(item)]))
    return value
}

function memory(chat: Chat) {
    return Object.fromEntries(memoryFields.map(field => [field, Object.hasOwn(chat, field)
        ? { present: true, value: encoded(safeStructuredClone(chat[field])) } : { present: false }]))
}
function jsonMemory(chat: Chat) {
    const raw = Object.fromEntries(memoryFields.filter(field => Object.hasOwn(chat, field))
        .map(field => [field, safeStructuredClone(chat[field])]))
    return memory(JSON.parse(JSON.stringify(raw)) as Chat)
}
function failure(error: unknown) {
    return error instanceof Error ? { name: error.name, message: error.message } : { name: typeof error, message: String(error) }
}
function databaseFor(entry: Entry): Database {
    return structuredClone({ ...base.root, ...entry.root, botPresets: [{ ...base.preset, ...entry.preset }],
        characters: [{ ...base.character, chats: [{ ...base.conversation, ...entry.conversation, message: base.messages }] }] }) as unknown as Database
}
async function reload(label: string) {
    const store = getPersistentDataStore()
    const database = await store.materializeDatabase()
    const { revision } = await store.replaceFromDatabase(database)
    setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
    const runtime = getPersistentDataRuntime()
    await runtime.initializeActiveWorkingSet(getDatabase())
    await runtime.flushPendingDataLocally(label)
    await immediate()
}
async function activate(entry: Entry) {
    const store = getPersistentDataStore()
    const database = normalizeDatabaseDefaults(databaseFor(entry))
    characterFormatUpdate(database.characters[0])
    deriveEffectivePresetMirrors(database)
    deriveEffectivePersonaMirrors(database)
    const { revision } = await store.replaceFromDatabase(database)
    setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
    selectedCharID.set(0)
    startup.ready = true
    for (const activate of startup.roots.splice(0)) activate()
    const runtime = getPersistentDataRuntime()
    await runtime.initializeActiveWorkingSet(getDatabase())
    await runtime.flushPendingDataLocally(`memory-send-${entry.id}-before`)
    await immediate()
    await reload(`memory-send-${entry.id}-reload`)
    return runtime
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
})

it('captures Supa and HypaV3 Send requests and persisted memory through real closed boundaries', async () => {
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'debug').mockImplementation(() => {})
    const diagnostics = process.env.RISUNEST_MEMORY_SEND_DIAGNOSTICS
    if (diagnostics) writeFileSync(diagnostics, '', { flag: 'wx' })
    const store = getPersistentDataStore()
    await store.open()
    const results: unknown[] = []
    const failures: { id: string; error: ReturnType<typeof failure> }[] = []
    let fetchHandler: ((input: RequestInfo | URL, init?: RequestInit) => Promise<Response>) | undefined
    const outside: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const luaJson = luaJsonModuleResponse(input)
        if (luaJson) return luaJson
        if (!fetchHandler) {
            const message = `Unplanned request outside a Send: ${String(input)}`
            outside.push(message)
            throw new Error(message)
        }
        try { return await fetchHandler(input, init) }
        catch (error) { outside.push(`Fetch observation failed: ${failure(error).message}`); throw error }
    }))
    for (const entry of set.cases) {
        const violations: string[] = []
        const trace: RecordValue[] = []
        const consoleErrors: { kind: string; message: string }[] = []
        const observation: RecordValue = { id: entry.id, trace }
        const controller = new AbortController()
        let timer: ReturnType<typeof setTimeout> | undefined
        let expired = false
        const restoreClock = vi.spyOn(Date, 'now').mockReturnValue(1767225600000)
        const restoreError = vi.spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
            const error = args[0] as { name?: string; message?: string; stack?: string } | null
            const message = args.map(String).join(' ')
            // Empty request hooks currently log this error. Its presence is not a contract.
            if (args.length === 1 && error?.name === 'TypeError'
                && error.message === "Cannot read properties of null (reading 'displayData')"
                && error.stack?.includes('requestChatDataUnderHold')) {
                consoleErrors.push({ kind: 'empty-request-hooks', message: error.message })
                return
            }
            const plannedFailure = entry.steps.find(step => step.phase === 'summary' && step.error)?.error
            if (entry.mode === 'hypav3' && plannedFailure && String(args[0]).includes('Hypa')
                && args[1] === 'Summarization failed:' && message.includes(plannedFailure)) {
                consoleErrors.push({ kind: 'planned-summary-failure', message })
                return
            }
            violations.push(`Unexpected console error: ${message}`)
        })
        const reject = (message: string): never => { violations.push(message); throw new Error(message) }
        try {
            const runtime = await activate(entry)
            alerts.errors.length = 0
            expect(getDatabase().aiModel).toBe('ollama-hosted')
            expect(getDatabase().subModel).toBe('ollama-hosted')
            expect(getDatabase().characters[0].supaMemory).toBe(true)
            expect(getDatabase().characters[0].triggerscript).toEqual([])
            expect(getDatabase().modules).toEqual([])
            expect(getDatabase().enabledModules).toEqual([])
            for (const hooks of [pluginV2.replacerbeforeRequest, pluginV2.replacerafterRequest, pluginV2.chatOutput]) expect(hooks.size).toBe(0)
            const before = (await store.readConversation('character', 'conversation'))!.value
            observation.before = memory(before)
            // These are the actual hosted model's cl100k counts, never stand-in policy counts.
            observation.historyTextTokens = await Promise.all(before.message.map(async message => ({ id: message.chatId, count: await tokenize(message.data) })))
            let position = 0
            fetchHandler = async (input, init) => {
                const step = entry.steps[position++]
                if (!step) return reject(`${entry.id}: unplanned request ${position}`)
                if (String(input) !== 'http://127.0.0.1:80/api/chat' || init?.method !== 'POST') return reject(`${entry.id}: unexpected endpoint or method`)
                if (typeof init?.body !== 'string') return reject(`${entry.id}: expected a buffered request body`)
                const body = init.body
                const parsed = JSON.parse(body) as { model?: string; stream?: boolean; messages?: unknown[] }
                if (parsed.model !== 'synthetic-memory-model' || parsed.stream !== false || !Array.isArray(parsed.messages)) return reject(`${entry.id}: unexpected hosted body`)
                if ('temperature' in parsed || 'max_tokens' in parsed) return reject(`${entry.id}: unplanned hosted parameters`)
                const live = getDatabase().characters[0].chats[0]
                trace.push({ phase: step.phase, body, liveMemory: memory(live), jsonMemory: jsonMemory(live) })
                return new Response(JSON.stringify(step.error ? { error: step.error } : {
                    model: 'synthetic-memory-model', done: true, message: { role: 'assistant', content: step.content },
                }), { status: step.status ?? 200, headers: { 'Content-Type': 'application/json' } })
            }
            resetScriptCache()
            const reservation = reserveGeneration()
            expect(reservation).not.toBeNull()
            const pending = sendChat({ historyLimit: false, signal: controller.signal }, reservation!).finally(() => reservation!.release())
            const guard = new Promise<never>((_resolve, reject) => {
                timer = setTimeout(() => { expired = true; violations.push('External Send completion guard expired'); controller.abort(); reject(new Error('External Send completion guard expired')) }, 15000)
            })
            let outcome: { kind: 'returned'; value: boolean } | { kind: 'thrown'; error: ReturnType<typeof failure> }
            try { outcome = { kind: 'returned', value: await Promise.race([pending, guard]) } }
            catch (error) { outcome = { kind: 'thrown', error: failure(error) } }
            finally { clearTimeout(timer) }
            observation.outcome = outcome
            observation.alerts = [...alerts.errors]
            expect(expired, entry.id).toBe(false)
            await runtime.flushPendingDataLocally(`memory-send-${entry.id}-after`)
            const saved = (await store.readConversation('character', 'conversation'))!.value
            observation.afterFlush = memory(saved)
            observation.output = entry.expected === 'success' ? {
                role: saved.message.at(-1)?.role, data: saved.message.at(-1)?.data,
                inputTokens: saved.message.at(-1)?.generationInfo?.inputTokens,
                outputTokens: saved.message.at(-1)?.generationInfo?.outputTokens,
            } : null
            await reload(`memory-send-${entry.id}-after-reload`)
            observation.afterReload = memory(getDatabase().characters[0].chats[0])
            observation.afterReloadStored = memory((await store.readConversation('character', 'conversation'))!.value)
            expect(violations, entry.id).toEqual([])
            expect(position, entry.id).toBe(entry.steps.length)
            expect(saved.message.slice(0, before.message.length), entry.id).toEqual(before.message)
            expect(saved.message, entry.id).toHaveLength(before.message.length + Number(entry.expected === 'success'))
            expect((saved as unknown as RecordValue).futureConversation, entry.id).toEqual((before as unknown as RecordValue).futureConversation)
            expect(observation.afterReload, entry.id).toEqual(observation.afterFlush)
            expect(observation.afterReloadStored, entry.id).toEqual(observation.afterFlush)
            if (entry.expected === 'success') {
                expect(outcome, entry.id).toEqual({ kind: 'returned', value: true })
                expect(saved.message.at(-1)?.data, entry.id).toBe('Synthetic final response.')
                expect(alerts.errors, entry.id).toEqual([])
            } else if (entry.expected === 'main-failure') {
                expect(outcome, entry.id).toEqual({ kind: 'thrown', error: { name: 'ResponseError', message: 'Synthetic main failure.' } })
                expect(alerts.errors, entry.id).toEqual([])
            } else {
                expect(outcome, entry.id).toEqual({ kind: 'returned', value: false })
                expect(alerts.errors, entry.id).toHaveLength(1)
                expect(alerts.errors[0], entry.id).toContain('Synthetic second summary failure.')
                expect(trace.every(step => step.phase === 'summary'), entry.id).toBe(true)
            }
            const main = trace.find(step => step.phase === 'main')
            if (entry.mode === 'supa') {
                if (entry.summaryCount === 0) {
                    expect(saved.supaMemoryData, entry.id).toBe(before.supaMemoryData)
                    expect(saved.lastMemory, entry.id).toBe(before.lastMemory)
                } else {
                    expect(saved.supaMemoryData, entry.id).toContain('Synthetic summary A.')
                    expect(saved.supaMemoryData?.split('\n')[0], entry.id).toBe(saved.lastMemory)
                    expect(saved.lastMemory, entry.id).not.toBe(before.lastMemory)
                    expect(main?.jsonMemory, entry.id).toEqual(memory(saved))
                }
            } else {
                expect(saved.hypaV3Data?.summaries.map(summary => summary.text), entry.id)
                    .toEqual(entry.summaryCount ? ['Synthetic summary A.'] : [])
                expect(saved.lastMemory, entry.id).toBe(before.lastMemory)
                expect(saved.hypaV3Data?.summaries.map(summary => summary.chatMemos), entry.id)
                    .toEqual(entry.summaryCount ? [['memory-message-1', 'memory-message-2']] : [])
                if (main) expect(main.jsonMemory, entry.id).toEqual(memory(saved))
            }
            results.push(observation)
        } catch (error) {
            observation.failure = failure(error)
            failures.push({ id: entry.id, error: failure(error) })
        } finally {
            clearTimeout(timer)
            controller.abort()
            fetchHandler = undefined
            observation.violations = [...violations]
            if (diagnostics) appendFileSync(diagnostics, JSON.stringify({ ...observation, consoleErrors }) + '\n')
            restoreClock.mockRestore()
            restoreError.mockRestore()
        }
    }
    expect(outside).toEqual([])
    expect(failures).toEqual([])
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: [
        'src/ts/process/index.svelte.ts', 'src/ts/process/memory/supaMemory.ts', 'src/ts/process/memory/hypav3.ts',
        'src/ts/process/request/request.ts', 'src/ts/process/generationState.ts', 'src/ts/tokenizer.ts',
        'src/ts/storage/database.svelte.ts', 'src/ts/storage/effectiveIdentityState.ts',
        'src/ts/storage/persistentDataRuntime.svelte.ts', 'src/ts/model/modellist.ts',
        'benchmarks/generation-send/ollama-memory-send.oracle.test.ts',
    ] })).toMatch(/written|matched/)
}, 120000)
