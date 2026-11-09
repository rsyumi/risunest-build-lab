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
        encodeToolCall: async (call: { call: { id: string; name: string } }) => `<tool_call>${call.call.id}${call.call.name}</tool_call>\n\n`,
        decodeToolCall: async (text: string) => toolTape.decodings[text.trim()],
    }
})

import { normalizeDatabaseDefaults, setDatabaseLite, getDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { requestChatDataMain, type requestDataResponse } from '../../src/ts/process/request/request'
import { getModelInfo, LLMFlags } from '../../src/ts/model/modellist'
import { language } from '../../src/lang'

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }
type Reply = { status?: number; contentType?: string; json?: Json; text?: string; throws?: string; chunks?: ({ utf8: string } | { hex: string })[] }
type Fixture = {
    schema: number
    baseDb: Record<string, Json>
    cases: Case[]
}
type Case = OracleCase & {
    model: string
    db?: Record<string, Json>
    customModels?: Json[]
    messages: Json[]
    stream?: boolean
    maxTokens?: number
    mode?: 'model' | 'submodel' | 'memory' | 'emotion' | 'otherAx' | 'translate'
    tools?: Json[]
    schema?: string
    // The Rust replay declines the case with this name; `terminal` marks one that is declined after a snapshot or a tool run.
    declines?: string
    terminal?: boolean
    extractJson?: string
    rememberToolUsage?: boolean
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
    if (reply?.text !== undefined) return new Response(reply.text, { status, headers: { 'Content-Type': reply.contentType ?? 'application/json' } })
    if (stream && reply?.text === undefined && reply?.json === undefined) {
        return new Response('data: [DONE]\n\n', { status, headers: { 'Content-Type': 'text/event-stream' } })
    }
    return new Response(JSON.stringify(reply && 'json' in reply ? reply.json : { text: 'ok' }), {
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
        aiModel: entry.model,
        customModels: entry.customModels ?? [],
        ...entry.db,
        useStreaming: entry.stream ?? false,
        characters: [], botPresets: [], plugins: [], modules: [], enabledModules: [],
    } as unknown as Database)
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
        keyIdentifier: info.keyIdentifier ?? null,
        provider: info.provider,
    }
}

function requestArgument(entry: Case, preview: boolean) {
    return {
        formated: structuredClone(entry.messages) as never,
        bias: {} as never,
        staticModel: entry.model,
        useStreaming: entry.stream ?? false,
        forceStreaming: entry.stream ?? false,
        maxTokens: entry.maxTokens,
        tools: structuredClone(entry.tools ?? []) as never,
        schema: entry.schema,
        extractJson: entry.extractJson,
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
    // The token request comes first, in the preview run as well; the preview run sends nothing else.
    replyQueue = structuredClone(wire ? entry.replies ?? [] : [])
    captured.length = 0
    toolTape.calls.length = 0
    let outcome: Awaited<ReturnType<typeof drain>> | { type: 'thrown'; error: string }
    try {
        const response = await requestChatDataMain(requestArgument(entry, !wire) as never, entry.mode ?? 'model')
        outcome = await drain(response)
    } catch (error) {
        outcome = { type: 'thrown', error: String(error) }
    }
    return { outcome, requests: captured.splice(0), toolCalls: structuredClone(toolTape.calls) }
}

const lane = 'providers/cohere'
const readSet = <C extends OracleCase>(set: string) => {
    const loaded = readOracleCases<C>(lane, set)
    return { ...loaded, file: JSON.parse(readFileSync(loaded.casesPath, 'utf8')) as Fixture }
}

const requestSources = [
    'src/ts/process/request/request.ts', 'src/ts/process/request/shared.ts', 'src/ts/process/request/additionalParams.ts',
    'src/ts/util.ts', 'src/ts/globalApi.svelte.ts', 'src/ts/model/types.ts',
]
const replySources = requestSources

describe('Cohere request bytes', () => {
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

    for (const name of ['responses']) {
        it(`matches the committed ${name}`, async () => {
            const set = readSet<Case>(name)
            const fixture = set.file as Fixture
            const results: unknown[] = []
            for (const entry of set.cases) {
                const sent = await observe(fixture, entry, true)
                results.push({ httpError: language.errors.httpError, model: resolvedModel(entry), requests: sent.requests, toolCalls: sent.toolCalls, outcome: sent.outcome })
            }
            settleOracle(set, results, { generator: import.meta.url, tsSources: replySources })
        })
    }
})
