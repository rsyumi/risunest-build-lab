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
const boundary = vi.hoisted(() => ({ cache: null as unknown, embedding: null as unknown, summary: null as unknown }))
vi.mock('../../src/ts/storage/hypaEmbeddingCache', async (original) => ({
    ...(await original<typeof import('../../src/ts/storage/hypaEmbeddingCache')>()),
    getHypaEmbeddingCache: () => boundary.cache as HypaEmbeddingCache,
}))
vi.mock('../../src/ts/process/transformers', async (original) => ({
    ...(await original<typeof import('../../src/ts/process/transformers')>()),
    runEmbedding: (async (...args) => (boundary.embedding as typeof import('../../src/ts/process/transformers')['runEmbedding'])(...args)) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding'],
    runSummarizer: (async (...args) => (boundary.summary as typeof import('../../src/ts/process/transformers')['runSummarizer'])(...args)) satisfies typeof import('../../src/ts/process/transformers')['runSummarizer'],
}))

import { normalizeDatabaseDefaults, setDatabase, type Chat, type character, type Database } from '../../src/ts/storage/database.svelte'
import { setRuntimePerformanceProfile } from '../../src/ts/runtimePerformanceProfile'
import { hypaMemoryV2 } from '../../src/ts/process/memory/hypav2'
import { HypaProcesser } from '../../src/ts/process/memory/hypamemory'
import { hypaCacheKeys, hypaEmbeddingIdentity } from '../../src/ts/process/memory/hypaCacheKey'
import { createNativeHypaEmbeddingCache, resilientHypaEmbeddingCache, type HypaEmbeddingCache } from '../../src/ts/storage/hypaEmbeddingCache'
import { ChatTokenizer } from '../../src/ts/tokenizer'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import type { OpenAIChat } from '../../src/ts/process/index.svelte'

type Encoded = null | boolean | number | string | Encoded[] | { [key: string]: Encoded }
type Failure = { name: string; message: string }
type Summary = { input: string; reply: string } | { input: string; failure: Failure }
type Embedding = { purpose: 'document' | 'query'; input: string | string[]; vectors: Encoded[][] } | { purpose: 'document' | 'query'; input: string | string[]; failure: Failure }
type CacheCall = { op: 'read' | 'write'; texts: string[]; failure?: Failure; missingKeys?: number }
type Invocation = { currentTokens: Encoded; maxContextTokens: Encoded; persistPrevious?: true }
type Entry = { id: string; input: {
    chats: Encoded[]; room: Encoded; allocatedTokens: number; chunkSize: number; maxResponse?: number
    invocations: Invocation[]; summaries: Summary[]; embeddings: Embedding[]; cache: CacheCall[]
    tokenizer?: { length?: number; additional?: Encoded; overrides?: { text: string; length: number }[]; failText?: string; maxCalls?: number }
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
    if (value instanceof Float32Array) {
        const view = new DataView(value.buffer, value.byteOffset, value.byteLength)
        return { f32bits: Array.from({ length: value.length }, (_, i) => view.getUint32(i * 4, true)) }
    }
    if (Array.isArray(value)) return Array.from(value, pack)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, pack(child)]))
    return value as null | boolean
}
const failure = (error: unknown): Failure => error instanceof Error ? { name: error.name, message: error.message } : { name: 'Error', message: String(error) }
const throwFailure = (value: Failure): never => { const error = new Error(value.message); error.name = value.name; throw error }
const displayRows = (rows: Row[]) => rows.map(({ bytes, ...row }) => ({ ...row, bytes: [...bytes] }))
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
    const result = header.entries.map(row => {
        const bytes = frame.slice(offset, offset + Number(row.byteLength))
        offset += Number(row.byteLength)
        return { ...row, bytes } as Row
    })
    expect(offset).toBe(frame.byteLength)
    return result
}

afterEach(() => {
    setRuntimePerformanceProfile('normal')
    pluginV2.providerOptions.clear()
    boundary.cache = boundary.embedding = boundary.summary = null
    vi.restoreAllMocks()
})

it('records actual HypaV2 policy, migration, summaries and retrieval over closed synthetic boundaries', async () => {
    const set = readOracleCases<Entry>('memory/hypa-v2', 'policy')
    const results: unknown[] = []
    const violations: string[] = []
    const reject = (message: string): never => { violations.push(message); throw new Error(message) }
    const check = (assertion: () => void) => { try { assertion() } catch (error) { violations.push(failure(error).message); throw error } }
    vi.spyOn(globalThis, 'fetch').mockImplementation(async () => reject('Unplanned network or model download'))
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const realPrepare = HypaProcesser.prototype.addText
    const realQuery = HypaProcesser.prototype.similaritySearchScored
    const realEmbeds = HypaProcesser.prototype.getEmbeds
    const realTokenize = ChatTokenizer.prototype.tokenizeChat
    let events: unknown[] = []
    let active: { purpose: 'document' | 'query'; input: string | string[] } | undefined
    vi.spyOn(HypaProcesser.prototype, 'getEmbeds').mockImplementation(async function(this: HypaProcesser, input, purpose = 'query') {
        active = { input, purpose }
        try { return await realEmbeds.call(this, input, purpose) } finally { active = undefined }
    })
    vi.spyOn(HypaProcesser.prototype, 'addText').mockImplementation(async function(this: HypaProcesser, input) {
        const event: Record<string, unknown> = { kind: 'prepare', input: [...input] }
        events.push(event)
        try { await realPrepare.call(this, input); event.vectors = pack(this.vectors) }
        catch (error) { event.failure = failure(error); event.vectors = pack(this.vectors); throw error }
    })
    vi.spyOn(HypaProcesser.prototype, 'similaritySearchScored').mockImplementation(async function(this: HypaProcesser, input) {
        const event: Record<string, unknown> = { kind: 'query', input }
        events.push(event)
        try { const result = await realQuery.call(this, input); event.result = result; return result }
        catch (error) { event.failure = failure(error); throw error }
    })
    vi.spyOn(ChatTokenizer.prototype, 'tokenizeChat').mockImplementation(async function(this: ChatTokenizer, message, args) {
        const event: Record<string, unknown> = { kind: 'tokenizeChat', message: pack(message), args: pack(args) }
        events.push(event)
        try { const count = await realTokenize.call(this, message, args); event.count = count; return count }
        catch (error) { event.failure = failure(error); throw error }
    })
    for (const entry of set.cases) {
        const input = entry.input
        violations.length = 0
        events = []
        const provider = `hypa-v2-oracle-${entry.id}`
        setRuntimePerformanceProfile('normal')
        setDatabase(normalizeDatabaseDefaults({ aiModel: 'custom', currentPluginProvider: provider, useTokenizerCaching: false,
            supaModelType: 'distilbart', hypaModel: 'MiniLM', supaMemoryKey: '', maxResponse: input.maxResponse ?? 0,
            hypaAllocatedTokens: input.allocatedTokens, hypaChunkSize: input.chunkSize,
        } as unknown as Database))
        let tokens = 0
        pluginV2.providerOptions.set(provider, { tokenizer: 'custom', tokenizerFunc: (text: string) => {
            if (++tokens > (input.tokenizer?.maxCalls ?? 256)) reject(`Tokenizer call bound exceeded in ${entry.id}`)
            const event: Record<string, unknown> = { kind: 'plugin-tokenizer', input: text }
            events.push(event)
            if (text === input.tokenizer?.failText) { event.failure = { name: 'Error', message: 'synthetic tokenizer failure' }; throw new Error('synthetic tokenizer failure') }
            const length = input.tokenizer?.overrides?.find(row => row.text === text)?.length ?? input.tokenizer?.length ?? 3
            event.length = length
            return new Array<number>(length).fill(0)
        } })
        let summaryIndex = 0
        boundary.summary = (async (text) => {
            const next = input.summaries[summaryIndex++]
            if (!next) reject(`Unplanned summary in ${entry.id}`)
            check(() => expect(text).toBe(next.input))
            const event: Record<string, unknown> = { kind: 'summarizer', input: text }
            events.push(event)
            if ('failure' in next) { event.failure = next.failure; return throwFailure(next.failure) }
            event.reply = next.reply
            return next.reply
        }) satisfies typeof import('../../src/ts/process/transformers')['runSummarizer']
        let embeddingIndex = 0
        boundary.embedding = (async (texts, model, device, signal) => {
            const next = input.embeddings[embeddingIndex++]
            if (!next || !active) reject(`Unplanned embedding in ${entry.id}`)
            check(() => {
                expect(pack(active)).toEqual(pack({ purpose: next.purpose, input: next.input }))
                expect(texts).toEqual(Array.isArray(next.input) ? next.input : [next.input])
                expect(model).toBe('Xenova/all-MiniLM-L6-v2')
                expect(device).toBe('wasm')
                expect(signal).toBeUndefined()
            })
            const event: Record<string, unknown> = { kind: 'local-embedding', ...active, model, device }
            events.push(event)
            if ('failure' in next) { event.failure = next.failure; return throwFailure(next.failure) }
            const reply = next.vectors.map(vector => Float32Array.from(vector.map(value => unpack(value) as number)))
            event.reply = pack(reply)
            return reply
        }) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding']
        const cacheTape = await Promise.all(input.cache.map(async call => {
            if (call.missingKeys !== undefined) check(() => {
                expect(call.op).toBe('write')
                expect(call.failure).toBeDefined()
                expect(Number.isSafeInteger(call.missingKeys) && call.missingKeys > 0).toBe(true)
            })
            const keys: (string | undefined)[] = await hypaCacheKeys(call.texts.map(content => ({ producer: 'hypa-v1-text' as const, content, identity: hypaEmbeddingIdentity('MiniLM') })))
            keys.push(...new Array<undefined>(call.missingKeys ?? 0).fill(undefined))
            return { ...call, keys }
        }))
        const stored = new Map<string, Row>()
        let cacheIndex = 0
        boundary.cache = resilientHypaEmbeddingCache(createNativeHypaEmbeddingCache(async <T>(command: string, args?: unknown): Promise<T> => {
            const next = cacheTape[cacheIndex++]
            if (!next) reject(`Unplanned cache operation ${command} in ${entry.id}`)
            const op = command === 'pds_read_hypa_embeddings' ? 'read' : command === 'pds_write_hypa_embeddings' ? 'write' : reject(`Unplanned cache command ${command}`)
            let rows: Row[] = []
            check(() => {
                expect(op).toBe(next.op)
                if (op === 'read') expect(args).toEqual({ keys: next.keys })
                else { expect(args).toBeInstanceOf(Uint8Array); rows = writeRows(args as Uint8Array); expect(rows.map(row => row.key)).toEqual(next.keys) }
            })
            const event: Record<string, unknown> = { kind: `cache-${op}`, keys: next.keys }
            events.push(event)
            if (op === 'write') event.rows = displayRows(rows)
            if (next.failure) { event.failure = next.failure; return throwFailure(next.failure) }
            if (op === 'read') { rows = next.keys.flatMap(key => key !== undefined && stored.has(key) ? [stored.get(key)!] : []); event.rows = displayRows(rows); return readFrame(rows) as T }
            for (const row of rows) stored.set(row.key, row)
            event.committed = true
            return undefined as T
        }, false))
        const chats = unpack(input.chats) as OpenAIChat[]
        let room = unpack(input.room) as Chat
        let previous: Awaited<ReturnType<typeof hypaMemoryV2>> | undefined
        const invocations: unknown[] = []
        for (const invocation of input.invocations) {
            if (invocation.persistPrevious) {
                check(() => { expect(previous?.error).toBeUndefined(); expect(previous?.memory).toBeDefined() })
                // This fixture explicitly feeds the real successful memory through JSON. It does not emulate the application caller.
                room = { ...room, hypaV2Data: JSON.parse(JSON.stringify(previous!.memory)) }
            }
            const before = pack(room)
            const eventStart = events.length
            let outcome: unknown
            try {
                previous = await hypaMemoryV2(chats, unpack(invocation.currentTokens) as number, unpack(invocation.maxContextTokens) as number, room, {} as character,
                    new ChatTokenizer(unpack(input.tokenizer?.additional ?? 2) as number, 'name'))
                outcome = { returned: previous }
            } catch (error) { previous = undefined; outcome = { thrown: failure(error) } }
            const serialized = previous?.memory === undefined ? undefined : JSON.stringify(previous.memory)
            invocations.push(pack({ input: { chats, currentTokens: unpack(invocation.currentTokens), maxContextTokens: unpack(invocation.maxContextTokens) }, roomBefore: before, roomAfter: pack(room),
                outcome, memoryJSON: serialized, memoryJSONReadback: serialized === undefined ? undefined : JSON.parse(serialized), events: events.slice(eventStart) }))
        }
        expect(violations, entry.id).toEqual([])
        expect({ summaries: summaryIndex, embeddings: embeddingIndex, cache: cacheIndex }, JSON.stringify({ id: entry.id, invocations })).toEqual({ summaries: input.summaries.length, embeddings: input.embeddings.length, cache: input.cache.length })
        results.push(pack({ scope: 'actual-hypav2-distilbart-minilm-policy', invocations, durableCache: displayRows([...stored.values()]) }))
        pluginV2.providerOptions.delete(provider)
    }
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: [
        'src/ts/process/memory/hypav2.ts', 'src/ts/process/memory/hypamemory.ts', 'src/ts/process/memory/hypaCacheKey.ts',
        'src/ts/storage/hypaEmbeddingCache.ts', 'src/ts/tokenizer.ts', 'src/ts/plugins/plugins.svelte.ts',
        'src/ts/runtimePerformanceProfile.ts', 'src/ts/storage/database.svelte.ts',
        'benchmarks/rust-core-oracles/memory-hypa-v2.oracle.test.ts',
    ] })).toMatch(/written|matched/)
}, 60000)
