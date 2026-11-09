import { afterEach, beforeEach, describe, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync } from 'node:fs'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// This oracle has no mounted UI. Application startup effects remain suspended.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (_start: () => void) => () => {} }
})

import { normalizeDatabaseDefaults, setDatabaseLite, getDatabase, getCurrentCharacter, type Database } from '../../src/ts/storage/database.svelte'
import { getUserName } from '../../src/ts/util'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { requestChatDataMain, type requestDataResponse } from '../../src/ts/process/request/request'
import { getModelInfo, LLMFlags } from '../../src/ts/model/modellist'

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }
type Reply = { status?: number; contentType?: string; json?: Json; text?: string; throws?: string }
type Fixture = { schema: number; baseDb: Record<string, Json>; cases: Case[] }
type Case = OracleCase & {
    model: string
    persona?: string
    biasString?: [string, number][]
    continue?: boolean
    db?: Record<string, Json>
    customModels?: Json[]
    messages: Json[]
    maxTokens?: number
    mode?: 'model' | 'submodel' | 'memory' | 'emotion' | 'otherAx' | 'translate'
    // The Rust replay declines the case with this name.
    declines?: string
    replies?: Reply[]
}
type Captured = { url: string; method: string; headers: [string, string][]; body: string | null }

const flagNames = Object.fromEntries(Object.entries(LLMFlags).map(([name, value]) => [value, name]))

function makeResponse(reply: Reply | undefined) {
    const status = reply?.status ?? 200
    if (reply?.text !== undefined) return new Response(reply.text, { status, headers: { 'Content-Type': reply.contentType ?? 'application/json' } })
    return new Response(JSON.stringify(reply && 'json' in reply ? reply.json : { data: ['ok'] }), {
        status, headers: { 'Content-Type': reply?.contentType ?? 'application/json' },
    })
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

const originalFetch = globalThis.fetch
const captured: Captured[] = []
let replyQueue: Reply[] = []

beforeEach(() => {
    captured.length = 0
    for (const level of ['log', 'warn', 'error', 'debug'] as const) vi.spyOn(console, level).mockImplementation(() => {})
    vi.stubGlobal('fetch', async (input: unknown, init: RequestInit = {}) => {
        const body = init.body
        captured.push({ url: String(input), method: String(init.method ?? 'GET'), headers: headerPairs(init.headers), body: typeof body === 'string' ? body : null })
        const next = replyQueue.shift()
        if (next?.throws !== undefined) throw new TypeError(next.throws)
        return makeResponse(next)
    })
})

afterEach(() => {
    globalThis.fetch = originalFetch
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
    // The names the reply cleanup receives, as the TS helpers report them.
    const names = { char: getCurrentCharacter()?.name ?? '', user: getUserName() }
    let outcome: ReturnType<typeof settled> | { type: 'thrown'; error: string }
    try {
        const response = await requestChatDataMain({
            formated: structuredClone(entry.messages) as never, bias: {}, staticModel: entry.model, maxTokens: entry.maxTokens, previewBody: !wire,
            biasString: structuredClone(entry.biasString ?? []), continue: entry.continue,
        } as never, entry.mode ?? 'model')
        outcome = settled(response)
    } catch (error) {
        outcome = { type: 'thrown', error: String(error) }
    }
    return { outcome, names, requests: captured.splice(0) }
}

const lane = 'providers/novellist'
const sources = [
    'src/ts/process/request/request.ts', 'src/ts/process/request/shared.ts', 'src/ts/process/request/additionalParams.ts',
    'src/ts/process/stringlize.ts', 'src/ts/util.ts', 'src/ts/globalApi.svelte.ts', 'src/ts/model/modellist.ts',
]

describe('NovelList requests and replies', () => {
    for (const name of ['requests', 'responses']) {
        it(`matches the committed ${name}`, async () => {
            const set = readOracleCases<Case>(lane, name)
            const fixture = JSON.parse(readFileSync(set.casesPath, 'utf8')) as Fixture
            const results: unknown[] = []
            for (const entry of set.cases) {
                const preview = await observe(fixture, entry, false)
                const model = resolvedModel(entry)
                const sent = await observe(fixture, entry, true)
                results.push({ names: sent.names, model, preview: preview.outcome, requests: sent.requests, outcome: sent.outcome })
            }
            settleOracle(set, results, { generator: import.meta.url, tsSources: sources })
        })
    }
})
