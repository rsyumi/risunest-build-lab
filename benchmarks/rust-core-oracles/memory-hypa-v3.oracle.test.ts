import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readOracleCases, settleOracle } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))
vi.mock('@mlc-ai/web-tokenizers', async (original) => {
    await original()
    return { ...(globalThis as { tokenizers?: object }).tokenizers }
})
const boundary = vi.hoisted(() => ({ cache: null as unknown, embedding: null as unknown, request: null as unknown }))
vi.mock('../../src/ts/storage/hypaEmbeddingCache', async (original) => ({
    ...(await original<typeof import('../../src/ts/storage/hypaEmbeddingCache')>()),
    getHypaEmbeddingCache: () => boundary.cache as HypaEmbeddingCache,
}))
vi.mock('../../src/ts/process/transformers', async (original) => ({
    ...(await original<typeof import('../../src/ts/process/transformers')>()),
    runEmbedding: (async (...args) => (boundary.embedding as typeof import('../../src/ts/process/transformers')['runEmbedding'])(...args)) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding'],
    runSummarizer: (async () => { throw new Error('Unplanned local summarizer') }) satisfies typeof import('../../src/ts/process/transformers')['runSummarizer'],
}))
// Loading the real module here would import HypaV3 through a cycle before this mock exists, so only the auxiliary
// model call is provided; HypaV3 reaches nothing else in it.
vi.mock('../../src/ts/process/request/request', () => ({
    requestChatData: (async (...args) => (boundary.request as typeof import('../../src/ts/process/request/request')['requestChatData'])(...args)) satisfies typeof import('../../src/ts/process/request/request')['requestChatData'],
}))
// The local WebLLM engine is an inference boundary; these cases summarize through the auxiliary model only.
vi.mock('../../src/ts/process/webllm', () => ({
    chatCompletion: async () => { throw new Error('Unplanned WebLLM completion') },
    unloadEngine: async () => { throw new Error('Unplanned WebLLM unload') },
}) satisfies typeof import('../../src/ts/process/webllm'))

import { normalizeDatabaseDefaults, setDatabase, type Chat, type character, type Database } from '../../src/ts/storage/database.svelte'
import { setRuntimePerformanceProfile } from '../../src/ts/runtimePerformanceProfile'
import { HypaProcesserEx, hypaMemoryV3, type HypaV3Settings } from '../../src/ts/process/memory/hypav3'
import { HypaProcesser } from '../../src/ts/process/memory/hypamemory'
import { createNativeHypaEmbeddingCache, resilientHypaEmbeddingCache, type HypaEmbeddingCache } from '../../src/ts/storage/hypaEmbeddingCache'
import { ChatTokenizer } from '../../src/ts/tokenizer'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import type { OpenAIChat } from '../../src/ts/process/index.svelte'

type Encoded = null | boolean | number | string | Encoded[] | { [key: string]: Encoded }
type Failure = { name: string; message: string }
type Request = { reply: { type: 'success' | 'fail'; result: string } } | { throw: string }
type Invocation = { currentTokens: Encoded; maxContextTokens: Encoded; persistPrevious?: true }
type Entry = { id: string; input: {
    settings: Partial<HypaV3Settings>; maxResponse: number; chats: Encoded[]; room: Encoded; prepared?: Encoded
    invocations: Invocation[]; summaries: Request[]; random?: number[]
    embeddings?: { vectors?: Record<string, number[]>; fail?: { purpose: 'document' | 'query'; text?: string; message: string } }
    tokenizer?: { length?: number; additional?: number; overrides?: { text: string; length: number }[]; failText?: string }
} }
type Row = { key: string; dimensions: number; byteLength: number; bytes: Uint8Array; [key: string]: unknown }

// Tags preserve values that JSON alone cannot transport. Ordinary fixture objects remain ordinary objects.
function unpack(value: Encoded): unknown {
    if (Array.isArray(value)) return value.map(unpack)
    if (value && typeof value === 'object') {
        if (Object.keys(value).length === 1) {
            if ('$undefined' in value) return undefined
            if ('$number' in value) return value.$number === '-0' ? -0 : Number(value.$number)
            if ('$utf16' in value) return (value.$utf16 as number[]).map(unit => String.fromCharCode(unit)).join('')
        }
        return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, unpack(child)]))
    }
    return value
}
function pack(value: unknown): Encoded {
    if (value === undefined) return { $undefined: true }
    if (typeof value === 'number') return Number.isFinite(value) && !Object.is(value, -0) ? value : { $number: Object.is(value, -0) ? '-0' : String(value) }
    if (typeof value === 'string') {
        for (let i = 0; i < value.length; i++) {
            const unit = value.charCodeAt(i)
            if (unit >= 0xdc00 && unit <= 0xdfff || unit >= 0xd800 && unit <= 0xdbff && !(value.charCodeAt(i + 1) >= 0xdc00 && value.charCodeAt(i + 1) <= 0xdfff)) {
                return { $utf16: Array.from({ length: value.length }, (_, index) => value.charCodeAt(index)) }
            }
            if (unit >= 0xd800 && unit <= 0xdbff) i++
        }
        return value
    }
    if (value instanceof Float32Array) return Array.from(value, pack)
    if (Array.isArray(value)) return Array.from(value, pack)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, pack(child)]))
    return value as null | boolean
}
const failure = (error: unknown): Failure => error instanceof Error ? { name: error.name, message: error.message } : { name: typeof error, message: String(error) }
// A synthetic embedding for any text without a fixture vector: three small components from an FNV-1a hash.
function hashVector(text: string): number[] {
    let hash = 0x811c9dc5
    for (let i = 0; i < text.length; i++) hash = Math.imul(hash ^ text.charCodeAt(i), 0x01000193) >>> 0
    return [1 + hash % 5, (hash >>> 3) % 7, (hash >>> 6) % 3]
}
function readFrame(rows: Row[]): Uint8Array {
    const header = new TextEncoder().encode(JSON.stringify({ entries: rows.map(({ key, dimensions, byteLength }) => ({ key, dimensions, byteLength })) }))
    const result = new Uint8Array(4 + header.length + rows.reduce((sum, row) => sum + row.bytes.length, 0))
    new DataView(result.buffer).setUint32(0, header.length, true)
    result.set(header, 4)
    let offset = 4 + header.length
    for (const row of rows) { result.set(row.bytes, offset); offset += row.bytes.length }
    return result
}
function writeRows(frame: Uint8Array): Row[] {
    const size = new DataView(frame.buffer, frame.byteOffset, frame.byteLength).getUint32(0, true)
    const header = JSON.parse(new TextDecoder().decode(frame.subarray(4, 4 + size))) as { entries: Omit<Row, 'bytes'>[] }
    let offset = 4 + size
    return header.entries.map(row => {
        const bytes = frame.slice(offset, offset + Number(row.byteLength))
        offset += Number(row.byteLength)
        return { ...row, bytes } as Row
    })
}

afterEach(() => {
    setRuntimePerformanceProfile('normal')
    pluginV2.providerOptions.clear()
    boundary.cache = boundary.embedding = boundary.request = null
    vi.restoreAllMocks()
})

it('records actual HypaV3 summaries, selection, retrieval and stored data over closed synthetic boundaries', async () => {
    const set = readOracleCases<Entry>('memory/hypa-v3', 'policy')
    const results: unknown[] = []
    const failures: string[] = []
    const violations: string[] = []
    const reject = (message: string): never => { violations.push(message); throw new Error(message) }
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (url) => reject(`Unplanned network or model download ${String(url)} ${new Error().stack}`))
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const realPrepare = HypaProcesser.prototype.addText
    const realEmbeds = HypaProcesser.prototype.getEmbeds
    const realQuery = HypaProcesserEx.prototype.similaritySearchScoredEx
    const realTokenize = ChatTokenizer.prototype.tokenizeChat
    let events: Record<string, unknown>[] = []
    let random: number[] = []
    let randomIndex = 0
    let active: { purpose: 'document' | 'query'; input: string | string[] } | undefined
    vi.spyOn(Math, 'random').mockImplementation(() => {
        if (randomIndex >= random.length) reject('Math.random tape exhausted')
        return random[randomIndex++]
    })
    vi.spyOn(HypaProcesser.prototype, 'getEmbeds').mockImplementation(async function(this: HypaProcesser, input, purpose = 'query') {
        active = { input, purpose }
        try { return await realEmbeds.call(this, input, purpose) } finally { active = undefined }
    })
    vi.spyOn(HypaProcesser.prototype, 'addText').mockImplementation(async function(this: HypaProcesser, input) {
        const event: Record<string, unknown> = { kind: 'prepare', input: [...input] }
        events.push(event)
        try { await realPrepare.call(this, input) } catch (error) { event.failure = failure(error); throw error }
    })
    vi.spyOn(HypaProcesserEx.prototype, 'similaritySearchScoredEx').mockImplementation(async function(this: HypaProcesserEx, input) {
        const event: Record<string, unknown> = { kind: 'query', input }
        events.push(event)
        try {
            const result = await realQuery.call(this, input)
            event.result = result.map(([chunk, score]) => [chunk.text, score])
            return result
        } catch (error) { event.failure = failure(error); throw error }
    })
    vi.spyOn(ChatTokenizer.prototype, 'tokenizeChat').mockImplementation(async function(this: ChatTokenizer, message, args) {
        const event: Record<string, unknown> = { kind: 'tokenizeChat', message: { ...message }, args }
        events.push(event)
        try { const count = await realTokenize.call(this, message, args); event.count = count; return count }
        catch (error) { event.failure = failure(error); throw error }
    })
    for (const entry of set.cases) {
        const input = entry.input
        violations.length = 0
        events = []
        random = input.random ?? []
        randomIndex = 0
        const provider = `hypa-v3-oracle-${entry.id}`
        setRuntimePerformanceProfile('normal')
        setDatabase(normalizeDatabaseDefaults({ aiModel: 'custom', currentPluginProvider: provider, useTokenizerCaching: false,
            hypaModel: 'MiniLM', supaMemoryKey: '', maxResponse: input.maxResponse, hypaV3: true,
            hypaV3Presets: [{ name: 'oracle', settings: input.settings }], hypaV3PresetId: 0,
        } as unknown as Database))
        pluginV2.providerOptions.set(provider, { tokenizer: 'custom', tokenizerFunc: (text: string) => {
            if (text === input.tokenizer?.failText) throw new Error('synthetic tokenizer failure')
            const length = input.tokenizer?.overrides?.find(row => row.text === text)?.length ?? input.tokenizer?.length ?? 3
            return new Array<number>(length).fill(0)
        } })
        let requestIndex = 0
        boundary.request = (async (arg, model, signal) => {
            const next = input.summaries[requestIndex++]
            if (!next) reject(`Unplanned summary request in ${entry.id}`)
            const event: Record<string, unknown> = { kind: 'summary', formated: arg.formated.map(chat => ({ ...chat })), model,
                options: { bias: arg.bias, useStreaming: arg.useStreaming, noMultiGen: arg.noMultiGen }, signal: signal === undefined ? 'absent' : 'present' }
            events.push(event)
            if ('throw' in next) { event.failure = { name: 'Error', message: next.throw }; throw new Error(next.throw) }
            event.reply = next.reply
            return { ...next.reply }
        }) satisfies typeof import('../../src/ts/process/request/request')['requestChatData']
        boundary.embedding = (async (texts, model, device, signal) => {
            if (!active) reject(`Unplanned embedding in ${entry.id}`)
            if (model !== 'Xenova/all-MiniLM-L6-v2' || device !== 'wasm' || signal !== undefined) reject(`Unexpected embedding target in ${entry.id}`)
            const event: Record<string, unknown> = { kind: 'local-embedding', purpose: active!.purpose, input: [...texts] }
            events.push(event)
            const fail = input.embeddings?.fail
            if (fail && fail.purpose === active!.purpose && (fail.text === undefined || texts.includes(fail.text))) {
                event.failure = { name: 'Error', message: fail.message }
                throw new Error(fail.message)
            }
            return texts.map(text => Float32Array.from(input.embeddings?.vectors?.[text] ?? hashVector(text)))
        }) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding']
        const stored = new Map<string, Row>()
        boundary.cache = resilientHypaEmbeddingCache(createNativeHypaEmbeddingCache(async <T>(command: string, args?: unknown): Promise<T> => {
            if (command === 'pds_read_hypa_embeddings') {
                const keys = (args as { keys: string[] }).keys
                events.push({ kind: 'cache-read', keys: keys.length })
                return readFrame(keys.flatMap(key => stored.has(key) ? [stored.get(key)!] : [])) as T
            }
            if (command === 'pds_write_hypa_embeddings') {
                const rows = writeRows(args as Uint8Array)
                events.push({ kind: 'cache-write', keys: rows.length })
                for (const row of rows) stored.set(row.key, row)
                return undefined as T
            }
            return reject(`Unplanned cache command ${command}`)
        }, false))
        const chats = unpack(input.chats) as OpenAIChat[]
        const before = JSON.stringify(chats)
        let room = unpack(input.room) as Chat
        let previous: Awaited<ReturnType<typeof hypaMemoryV3>> | undefined
        const invocations: unknown[] = []
        for (const invocation of input.invocations) {
            if (invocation.persistPrevious) {
                if (previous?.error !== undefined || previous?.memory === undefined) reject(`No successful memory to persist in ${entry.id}`)
                // The fixture feeds the real returned memory through JSON as storage would; it does not emulate the caller.
                room = { ...room, hypaV3Data: JSON.parse(JSON.stringify(previous!.memory)) }
            }
            const roomBefore = pack(room)
            const eventStart = events.length
            const randomStart = randomIndex
            let outcome: unknown
            try {
                previous = await hypaMemoryV3(chats, unpack(invocation.currentTokens) as number, unpack(invocation.maxContextTokens) as number, room, {} as character,
                    new ChatTokenizer(input.tokenizer?.additional ?? 2, 'name'), unpack(input.prepared ?? { $undefined: true }) as Parameters<typeof hypaMemoryV3>[6])
                outcome = { returned: previous }
            } catch (error) { previous = undefined; outcome = { thrown: failure(error) } }
            const serialized = previous?.memory === undefined ? undefined : JSON.stringify(previous.memory)
            invocations.push(pack({ input: { currentTokens: unpack(invocation.currentTokens), maxContextTokens: unpack(invocation.maxContextTokens) },
                roomBefore, roomAfter: pack(room), outcome, memoryJSON: serialized, events: events.slice(eventStart),
                random: random.slice(randomStart, randomIndex) }))
        }
        if (JSON.stringify(chats) !== before) reject(`HypaV3 changed its input chats in ${entry.id}`)
        if (requestIndex !== input.summaries.length) violations.push(`${requestIndex} of ${input.summaries.length} summary requests`)
        failures.push(...violations.map(violation => `${entry.id}: ${violation}`))
        results.push(pack({ scope: 'actual-hypav3-submodel-minilm-policy', chats, invocations }))
        pluginV2.providerOptions.delete(provider)
    }
    expect(failures).toEqual([])
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: [
        'src/ts/process/memory/hypav3.ts', 'src/ts/process/memory/hypamemory.ts', 'src/ts/process/memory/hypaCacheKey.ts',
        'src/ts/storage/hypaEmbeddingCache.ts', 'src/ts/tokenizer.ts', 'src/ts/plugins/plugins.svelte.ts', 'src/ts/parser/chatML.ts',
        'src/ts/util/inlayTokens.ts', 'src/ts/runtimePerformanceProfile.ts', 'src/ts/storage/database.svelte.ts',
        'benchmarks/rust-core-oracles/memory-hypa-v3.oracle.test.ts',
    ] })).toMatch(/written|matched/)
}, 120000)
