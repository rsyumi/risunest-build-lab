import { afterEach, beforeEach, describe, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync } from 'node:fs'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// This oracle has no mounted UI. Application startup effects remain suspended.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (_start: () => void) => () => {} }
})

// The chat template belongs to the template lane: the real template runs and its output is recorded, because the
// native builder receives the rendered prompt as an input.
const templateTape = vi.hoisted(() => ({ prompts: [] as string[] }))
vi.mock('../../src/ts/process/templates/chatTemplate', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/templates/chatTemplate')>()
    return {
        ...actual,
        applyChatTemplate: (...args: Parameters<typeof actual.applyChatTemplate>) => {
            const prompt = actual.applyChatTemplate(...args)
            templateTape.prompts.push(prompt)
            return prompt
        },
    }
})

import { normalizeDatabaseDefaults, setDatabaseLite, getDatabase, getCurrentCharacter, type Database } from '../../src/ts/storage/database.svelte'
import { getUserName } from '../../src/ts/util'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { requestChatDataMain, type requestDataResponse } from '../../src/ts/process/request/request'
import { getModelInfo, LLMFlags } from '../../src/ts/model/modellist'

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }
type Reply = { status?: number; json?: Json; text?: string; throws?: string }
type Fixture = { schema: number; baseDb: Record<string, Json>; cases: Case[] }
type Case = OracleCase & {
    model: string
    persona?: string
    db?: Record<string, Json>
    customModels?: Json[]
    messages: Json[]
    maxTokens?: number
    mode?: 'model' | 'submodel' | 'memory' | 'emotion' | 'otherAx' | 'translate'
    // The Rust replay declines the case with this name.
    declines?: string
    // The create reply, then one reply per status request.
    replies?: Reply[]
}
type Captured = { url: string; method: string; headers: [string, string][]; body: string | null }

const flagNames = Object.fromEntries(Object.entries(LLMFlags).map(([name, value]) => [value, name]))

function makeResponse(reply: Reply) {
    const status = reply.status ?? 200
    if (reply.text !== undefined) return new Response(reply.text, { status })
    return new Response(JSON.stringify(reply.json ?? null), { status })
}

function headerPairs(headers: unknown): [string, string][] {
    if (headers instanceof Headers) return [...headers.entries()]
    return Object.entries((headers ?? {}) as Record<string, string>).map(([key, value]) => [key, String(value)])
}

function configure(fixture: Fixture, entry: Case) {
    const database = normalizeDatabaseDefaults({
        ...fixture.baseDb,
        aiModel: entry.model,
        customModels: entry.customModels ?? [],
        ...entry.db,
        characters: [], botPresets: [], plugins: [], modules: [], enabledModules: [],
    } as unknown as Database)
    // The prompt and the reply cleanup read the selected character; a bound persona replaces the user name.
    database.characters = [{ name: 'Aria', chats: [{ message: [], bindedPersona: entry.persona ? 'p1' : '' }], chatPage: 0 }] as unknown as Database['characters']
    database.personas = (entry.persona ? [{ id: 'p1', name: entry.persona, personaPrompt: '', icon: '' }] : []) as unknown as Database['personas']
    setDatabaseLite(database)
    selectedCharID.set(0)
}

function resolvedModel(entry: Case) {
    const db = getDatabase()
    const info = getModelInfo(entry.model)
    const reverse = entry.model === 'reverse_proxy'
    return {
        id: info.id,
        internalID: reverse ? db.customProxyRequestModel : info.internalID ?? null,
        format: reverse ? db.customAPIFormat : info.format,
        flags: info.flags.map(flag => flagNames[flag]),
        parameters: info.parameters,
        endpoint: info.endpoint ?? null,
    }
}

// `fetch` is a host boundary: every call is recorded, the create and status requests take the next scripted reply, and
// the cancel `DELETE` that TS never awaits gets an empty answer. The poll delay is a timer that completes at once.
const originalFetch = globalThis.fetch
const realSetTimeout = globalThis.setTimeout
const captured: (Captured & { signal: boolean })[] = []
const sleeps: number[] = []
let replyQueue: Reply[] = []

beforeEach(() => {
    captured.length = 0
    for (const level of ['log', 'warn', 'error', 'debug'] as const) vi.spyOn(console, level).mockImplementation(() => {})
    vi.stubGlobal('fetch', async (input: unknown, init: RequestInit = {}) => {
        const body = init.body
        const method = String(init.method ?? 'GET')
        captured.push({ url: String(input), method, headers: headerPairs(init.headers), body: typeof body === 'string' ? body : null, signal: init.signal !== undefined })
        if (method === 'DELETE') return new Response('{}')
        const next = replyQueue.shift()
        if (next === undefined) throw new Error('No scripted reply')
        if (next.throws !== undefined) throw new TypeError(next.throws)
        return makeResponse(next)
    })
    vi.stubGlobal('setTimeout', (callback: (...args: unknown[]) => void, ms?: number, ...args: unknown[]) => {
        if (ms !== 2000) return realSetTimeout(callback, ms, ...args)
        sleeps.push(ms)
        queueMicrotask(() => callback(...args))
        return 0
    })
})

afterEach(() => {
    globalThis.fetch = originalFetch
    globalThis.setTimeout = realSetTimeout
    vi.restoreAllMocks()
})

function settled(response: requestDataResponse) {
    const noRetry = (response as { noRetry?: boolean }).noRetry
    // `undefined` does not survive JSON, so an undefined result is recorded as a flag.
    const base = response.result === undefined ? { type: response.type, resultUndefined: true } : { type: response.type, result: response.result as unknown }
    return noRetry === undefined ? base : { ...base, noRetry }
}

async function observe(fixture: Fixture, entry: Case, wire: boolean) {
    configure(fixture, entry)
    replyQueue = structuredClone(wire ? entry.replies ?? [] : [])
    captured.length = 0
    sleeps.length = 0
    templateTape.prompts.length = 0
    // The names the reply cleanup receives, as the TS helpers report them.
    const names = { char: getCurrentCharacter()?.name ?? '', user: getUserName() }
    const controller = new AbortController()
    let outcome: ReturnType<typeof settled> | { type: 'thrown'; error: string }
    try {
        const response = await requestChatDataMain({
            formated: structuredClone(entry.messages) as never, bias: {}, staticModel: entry.model, maxTokens: entry.maxTokens, previewBody: !wire,
        } as never, entry.mode ?? 'model', controller.signal)
        outcome = settled(response)
    } catch (error) {
        outcome = { type: 'thrown', error: String(error) }
    }
    return { outcome, names, requests: captured.splice(0), sleeps: sleeps.splice(0), prompts: templateTape.prompts.splice(0) }
}

const lane = 'providers/horde'
const sources = [
    'src/ts/process/request/request.ts', 'src/ts/process/request/shared.ts', 'src/ts/process/request/additionalParams.ts',
    'src/ts/process/templates/chatTemplate.ts', 'src/ts/process/stringlize.ts', 'src/ts/util.ts', 'src/ts/model/modellist.ts',
]

describe('Horde requests and poll runs', () => {
    for (const name of ['requests', 'responses']) {
        it(`matches the committed ${name}`, async () => {
            const set = readOracleCases<Case>(lane, name)
            const fixture = JSON.parse(readFileSync(set.casesPath, 'utf8')) as Fixture
            const results: unknown[] = []
            for (const entry of set.cases) {
                const preview = await observe(fixture, entry, false)
                const model = resolvedModel(entry)
                const sent = await observe(fixture, entry, true)
                results.push({ names: sent.names, model, prompts: sent.prompts, preview: preview.outcome, requests: sent.requests, sleeps: sent.sleeps, outcome: sent.outcome })
            }
            settleOracle(set, results, { generator: import.meta.url, tsSources: sources })
        })
    }
})
