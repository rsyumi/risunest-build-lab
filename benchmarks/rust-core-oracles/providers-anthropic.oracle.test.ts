import { afterEach, beforeEach, describe, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync } from 'node:fs'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// This oracle has no mounted UI. Application startup effects remain suspended.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (_start: () => void) => () => {} }
})

// The MCP client is a host boundary: tool replies come from the fixture.
const toolTape = vi.hoisted(() => ({
    calls: [] as { name: string; args: unknown }[],
    replies: [] as unknown[],
    decodings: {} as Record<string, unknown>,
}))
vi.mock('../../src/ts/process/mcp/mcp', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/mcp/mcp')>()
    return {
        ...actual,
        getTools: async () => [],
        callTool: async (name: string, args: unknown) => {
            toolTape.calls.push({ name, args })
            const reply = toolTape.replies.shift()
            if (reply === undefined) throw new Error('No scripted tool reply')
            if (reply !== null && typeof reply === 'object' && 'throws' in reply) throw new Error(String((reply as { throws: unknown }).throws))
            return reply as { type: string; text: string }[]
        },
        encodeToolCall: async (call: { call: { id: string; name: string } }) => `<tool_call>${call.call.id}${call.call.name}</tool_call>\n\n`,
        decodeToolCall: async (text: string) => toolTape.decodings[text.trim()],
    }
})

// The batch id is a host boundary and is fixed. The poll delay is a timer: the wrapper in `observe` completes it at once and moves the faked clock.
const sleepTape = vi.hoisted(() => ({ calls: [] as number[], abortAt: 0, controller: null as AbortController | null }))
vi.mock('uuid', async (original) => ({ ...(await original<typeof import('uuid')>()), v4: () => '00000000-0000-4000-8000-000000000001' }))

import { normalizeDatabaseDefaults, setDatabaseLite, getDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { requestChatDataMain, type requestDataResponse } from '../../src/ts/process/request/request'
import { getModelInfo, LLMFlags } from '../../src/ts/model/modellist'
import { language } from '../../src/lang'

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }
type Reply = { advanceMs?: number; status?: number; contentType?: string; json?: Json; text?: string; throws?: string; chunks?: ({ utf8: string } | { hex: string })[] }
type Fixture = {
    schema: number
    baseDb: Record<string, Json>
    cases: Case[]
}
type Case = OracleCase & {
    model: string
    mainModel?: string
    db?: Record<string, Json>
    customModels?: Json[]
    messages: Json[]
    stream?: boolean
    maxTokens?: number
    mode?: 'model' | 'submodel' | 'memory' | 'emotion' | 'otherAx' | 'translate'
    tools?: Json[]
    schema?: string
    extractJson?: string
    noMultiGen?: boolean
    continue?: boolean
    rememberToolUsage?: boolean
    // The Rust replay declines the case with this name; `terminal` marks one that is declined after a snapshot or a tool run.
    declines?: string
    terminal?: boolean
    // Fixes `Date` for requests that sign with the clock.
    clock?: string
    // The web build sends the request through a proxy: only the body and the preview describe the builder.
    transport?: 'risu-proxy'
    // Aborts the request signal while the poll loop sleeps for the n-th time.
    abortAtSleep?: number
    // Responses and streams.
    reply?: Reply
    replies?: Reply[]
    toolReplies?: Json[]
    decodings?: Record<string, Json>
}

type Captured = { url: string; method: string; headers: [string, string][]; body: string | null }

const flagNames = Object.fromEntries(Object.entries(LLMFlags).map(([name, value]) => [value, name]))

function bytesOf(part: { utf8: string } | { hex: string }) {
    return 'utf8' in part ? new TextEncoder().encode(part.utf8) : Uint8Array.from(part.hex.match(/../g)!.map(byte => parseInt(byte, 16)))
}

function makeResponse(reply: Reply | undefined, stream: boolean) {
    const status = reply?.status ?? 200
    if (reply?.chunks) {
        const chunks = reply.chunks.map(bytesOf)
        const body = new ReadableStream<Uint8Array>({
            start(controller) {
                for (const chunk of chunks) controller.enqueue(chunk)
                controller.close()
            },
        })
        return new Response(body, { status, headers: { 'Content-Type': reply.contentType ?? 'text/event-stream' } })
    }
    if (stream && reply?.text === undefined && reply?.json === undefined) {
        return new Response('data: [DONE]\n\n', { status, headers: { 'Content-Type': 'text/event-stream' } })
    }
    if (reply?.text !== undefined) return new Response(reply.text, { status, headers: { 'Content-Type': reply.contentType ?? 'application/json' } })
    return new Response(JSON.stringify(reply?.json ?? { content: [{ type: 'text', text: 'ok' }] }), {
        status, headers: { 'Content-Type': reply?.contentType ?? 'application/json' },
    })
}

function headerPairs(headers: unknown): [string, string][] {
    if (headers instanceof Headers) return [...headers.entries()]
    if (Array.isArray(headers)) return headers.map(([key, value]) => [String(key), String(value)])
    return Object.entries((headers ?? {}) as Record<string, string>).map(([key, value]) => [key, String(value)])
}

function configure(fixture: Fixture, entry: Case) {
    const database = normalizeDatabaseDefaults({
        ...fixture.baseDb,
        aiModel: entry.mainModel ?? entry.model,
        customModels: entry.customModels ?? [],
        ...entry.db,
        useStreaming: entry.stream ?? false,
        characters: [], botPresets: [], plugins: [], modules: [], enabledModules: [],
    } as unknown as Database)
    setDatabaseLite(database)
    selectedCharID.set(0)
}

// Mirrors the `reverse_proxy` assignments in `requestChatDataMain`, so the fixture records the model the builder saw.
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
        keyIdentifier: info.keyIdentifier ?? null,
        provider: info.provider,
        mainImageInput: getModelInfo(db.aiModel).flags.includes(LLMFlags.hasImageInput),
    }
}

function requestArgument(entry: Case, preview: boolean) {
    return {
        abortSignal: sleepTape.controller?.signal,
        formated: structuredClone(entry.messages) as never,
        bias: {} as never,
        staticModel: entry.model,
        useStreaming: entry.stream ?? false,
        forceStreaming: entry.stream ?? false,
        maxTokens: entry.maxTokens,
        tools: structuredClone(entry.tools ?? []) as never,
        schema: entry.schema,
        extractJson: entry.extractJson,
        noMultiGen: entry.noMultiGen,
        continue: entry.continue,
        rememberToolUsage: entry.rememberToolUsage,
        previewBody: preview,
    }
}

const originalFetch = globalThis.fetch
const captured: Captured[] = []
let replyQueue: Reply[] = []

beforeEach(() => {
    captured.length = 0
    toolTape.calls.length = 0
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(console, 'debug').mockImplementation(() => {})
    vi.stubGlobal('fetch', async (input: unknown, init: RequestInit = {}) => {
        if ((init.signal as AbortSignal | undefined)?.aborted) throw new DOMException('This operation was aborted', 'AbortError')
        const body = init.body
        const text = body === undefined || body === null ? null : typeof body === 'string' ? body : new TextDecoder('utf-8', { fatal: true }).decode(body as Uint8Array)
        captured.push({ url: String(input), method: String(init.method ?? 'GET'), headers: headerPairs(init.headers), body: text })
        const next = replyQueue.shift()
        if (vi.isFakeTimers()) {
            if (next?.advanceMs) vi.setSystemTime(Date.now() + next.advanceMs)
            // A tape that runs dry would poll for a simulated day; the guard makes it time out instead.
            if (captured.length > 400) vi.setSystemTime(Date.now() + 90_000_000)
        }
        if (next?.throws !== undefined) throw new TypeError(next.throws)
        return makeResponse(next, text !== null && (JSON.parse(text) as { stream?: boolean }).stream === true)
    })
})

afterEach(() => {
    globalThis.fetch = originalFetch
    vi.restoreAllMocks()
})

async function drain(response: requestDataResponse) {
    if (response.type !== 'streaming') {
        const failByServerError = (response as { failByServerError?: boolean }).failByServerError
        return failByServerError === undefined ? { type: response.type, result: response.result as unknown } : { type: response.type, result: response.result as unknown, failByServerError }
    }
    const reader = response.result.getReader()
    const snapshots: Record<string, string>[] = []
    try {
        while (true) {
            const next = await reader.read()
            if (next.done) break
            snapshots.push(next.value)
        }
    } catch (error) {
        return { type: 'streaming', snapshots, error: String(error) }
    }
    return { type: 'streaming', snapshots }
}

async function observe(fixture: Fixture, entry: Case, wire: boolean) {
    configure(fixture, entry)
    toolTape.replies = structuredClone(entry.toolReplies ?? []) as unknown[]
    toolTape.decodings = structuredClone(entry.decodings ?? {}) as Record<string, unknown>
    replyQueue = structuredClone(wire ? entry.replies ?? (entry.reply ? [entry.reply] : []) : [])
    captured.length = 0
    toolTape.calls.length = 0
    sleepTape.calls = []
    sleepTape.abortAt = entry.abortAtSleep ?? 0
    sleepTape.controller = entry.abortAtSleep ? new AbortController() : null
    let outcome: Awaited<ReturnType<typeof drain>> | { type: 'thrown'; error: string }
    if (entry.clock) { vi.useFakeTimers({ toFake: ['Date'] }); vi.setSystemTime(new Date(entry.clock)) }
    const realSetTimeout = globalThis.setTimeout
    if (entry.db?.claudeBatching) {
        vi.stubGlobal('setTimeout', (callback: (...args: unknown[]) => void, ms?: number, ...args: unknown[]) => {
            if (ms !== 3000) return realSetTimeout(callback, ms, ...args)
            sleepTape.calls.push(ms)
            if (vi.isFakeTimers()) vi.setSystemTime(Date.now() + ms)
            if (sleepTape.abortAt !== 0 && sleepTape.abortAt === sleepTape.calls.length) sleepTape.controller?.abort()
            queueMicrotask(() => callback(...args))
            return 0
        })
    }
    try {
        const response = await requestChatDataMain(requestArgument(entry, !wire) as never, entry.mode ?? 'model', sleepTape.controller?.signal)
        outcome = await drain(response)
    } catch (error) {
        outcome = { type: 'thrown', error: String(error) }
    } finally {
        if (entry.db?.claudeBatching) vi.stubGlobal('setTimeout', realSetTimeout)
        if (entry.clock) vi.useRealTimers()
    }
    return { outcome, requests: captured.splice(0), toolCalls: structuredClone(toolTape.calls), sleeps: [...sleepTape.calls] }
}

const lane = 'providers/anthropic'
const readSet = <C extends OracleCase>(set: string) => {
    const loaded = readOracleCases<C>(lane, set)
    return { ...loaded, file: JSON.parse(readFileSync(loaded.casesPath, 'utf8')) as Fixture }
}

const requestSources = [
    'src/ts/process/request/request.ts', 'src/ts/process/request/shared.ts', 'src/ts/process/request/additionalParams.ts',
    'src/ts/process/request/anthropic.ts', 'src/ts/util.ts', 'src/ts/globalApi.svelte.ts', 'src/ts/model/types.ts',
]
const replySources = [...requestSources, 'src/ts/process/templates/jsonSchema.ts']

describe('Anthropic request bytes', () => {
    it('matches the committed expectations', async () => {
        const set = readSet<Case>('requests')
        const fixture = set.file as Fixture
        const results: unknown[] = []
        for (const entry of set.cases) {
            const preview = await observe(fixture, entry, false)
            const model = resolvedModel(entry)
            const sent = await observe(fixture, entry, true)
            results.push({
                model,
                preview: preview.outcome,
                requests: sent.requests,
                outcome: sent.outcome.type === 'streaming' ? { type: 'streaming' } : sent.outcome.type === 'thrown' ? sent.outcome : { type: sent.outcome.type },
            })
        }
        settleOracle(set, results, { generator: import.meta.url, tsSources: requestSources })
    })

    it('matches the committed response expectations', async () => {
        const set = readSet<Case>('responses')
        const fixture = set.file as Fixture
        const results: unknown[] = []
        for (const entry of set.cases) {
            const sent = await observe(fixture, entry, true)
            results.push({ httpError: language.errors.httpError, model: resolvedModel(entry), requests: sent.requests, toolCalls: sent.toolCalls, outcome: sent.outcome })
        }
        settleOracle(set, results, { generator: import.meta.url, tsSources: replySources })
    })

    it('matches the committed stream expectations', async () => {
        const set = readSet<Case>('streams')
        const fixture = set.file as Fixture
        const results: unknown[] = []
        for (const entry of set.cases) {
            const sent = await observe(fixture, entry, true)
            results.push({ model: resolvedModel(entry), requests: sent.requests, toolCalls: sent.toolCalls, outcome: sent.outcome })
        }
        settleOracle(set, results, { generator: import.meta.url, tsSources: replySources })
    })

    it('matches the committed Bedrock expectations', async () => {
        const set = readSet<Case>('bedrock')
        const fixture = set.file as Fixture
        const results: unknown[] = []
        for (const entry of set.cases) {
            const preview = await observe(fixture, entry, false)
            const model = resolvedModel(entry)
            const sent = await observe(fixture, entry, true)
            results.push({
                httpError: language.errors.httpError, model, preview: preview.outcome, requests: sent.requests, toolCalls: sent.toolCalls, outcome: sent.outcome,
            })
        }
        settleOracle(set, results, { generator: import.meta.url, tsSources: replySources })
    })

    it('matches the committed batching expectations', async () => {
        const set = readSet<Case>('batching')
        const fixture = set.file as Fixture
        const results: unknown[] = []
        for (const entry of set.cases) {
            const preview = await observe(fixture, entry, false)
            const sent = await observe(fixture, entry, true)
            results.push({ model: resolvedModel(entry), preview: preview.outcome, requests: sent.requests, sleeps: sent.sleeps, outcome: sent.outcome })
        }
        settleOracle(set, results, { generator: import.meta.url, tsSources: replySources })
    })
})
