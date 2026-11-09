import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { readOracleCases, settleOracle } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))
vi.mock('@mlc-ai/web-tokenizers', async (original) => {
    await original()
    return { ...(globalThis as { tokenizers?: object }).tokenizers }
})
const boundary = vi.hoisted(() => ({ cache: null as unknown, fetch: null as unknown, local: null as unknown, alerts: [] as string[] }))
vi.mock('../../src/ts/storage/hypaEmbeddingCache', async (original) => ({
    ...(await original<typeof import('../../src/ts/storage/hypaEmbeddingCache')>()),
    getHypaEmbeddingCache: () => boundary.cache as HypaEmbeddingCache,
}))

vi.mock('../../src/ts/process/transformers', async (original) => ({
    ...(await original<typeof import('../../src/ts/process/transformers')>()),
    runEmbedding: (async (...args) => (boundary.local as typeof import('../../src/ts/process/transformers')['runEmbedding'])(...args)) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding'],
}))
vi.mock('../../src/ts/alert', async (original) => ({
    ...(await original<typeof import('../../src/ts/alert')>()),
    alertError: (message: string | Error) => { boundary.alerts.push(String(message)) },
}))

import { normalizeDatabaseDefaults, setDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { setRuntimePerformanceProfile, getRuntimePerformanceBudgets } from '../../src/ts/runtimePerformanceProfile'
import { HypaProcesser } from '../../src/ts/process/memory/hypamemory'
import { hanuraiMemory } from '../../src/ts/process/memory/hanuraiMemory'
import { hypaCacheKeys, hypaEmbeddingIdentity } from '../../src/ts/process/memory/hypaCacheKey'
import { createNativeHypaEmbeddingCache, resilientHypaEmbeddingCache, toVectorBuffer, type HypaEmbeddingCache } from '../../src/ts/storage/hypaEmbeddingCache'
import { ChatTokenizer } from '../../src/ts/tokenizer'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import type { OpenAIChat } from '../../src/ts/process/index.svelte'
import { language } from '../../src/lang'
import { hubURL } from '../../src/ts/characterCards'

type Text = string | { u: number[] }
type Scalar = number | 'NaN' | 'Infinity' | '-Infinity' | '-0'
type Vector = { precision: 'f64'; values: Scalar[] } | { precision: 'f32'; bits: number[] } | { absent: true } | { null: true }
type Failure = { name: string; message: string }
type Request = { purpose: 'document' | 'query'; shape: 'scalar' | 'batch'; texts: Text[]; result?: Vector[]; failure?: Failure }
type Entry = { id: string; input: {
    mode: 'retrieval' | 'hanurai' | 'keys'
    target: { model: 'custom' | 'MiniLM'; endpoint?: string; customModel?: string; cacheBatchLimit: number }
    profile: 'normal' | 'low-spec'; documents: Text[]; queries: Text[]; embeddings: Request[]
    cache?: { seeds?: { text?: Text; key?: string; vector: Vector }[]; readFaultAt?: number; writeFaultAt?: number; malformedReadAt?: number }
    hanurai?: { chats: OpenAIChat[]; currentTokens: number; maxContextTokens: number; split: boolean; reserve: number }
} }
type NativeRow = { key?: string; producer?: string; model?: string; endpoint?: string | null; preprocessVersion?: number; dimensions: number; byteLength: number; metadata?: string | null; bytes: Uint8Array }
const text = (value: Text): string => typeof value === 'string' ? value : value.u.map(unit => String.fromCharCode(unit)).join('')
const scalar = (value: Scalar): number => typeof value === 'number' ? value : value === '-0' ? -0 : Number(value)
function vector(value: Vector): number[] | Float32Array | undefined | null {
    if ('absent' in value) return undefined
    if ('null' in value) return null
    if (value.precision === 'f64') return value.values.map(scalar)
    const result = new Float32Array(value.bits.length)
    const view = new DataView(result.buffer)
    value.bits.forEach((bits, index) => view.setUint32(index * 4, bits, true))
    return result
}
function encodeText(value: string): Text {
    for (let index = 0; index < value.length; index++) {
        const unit = value.charCodeAt(index)
        if (unit >= 0xdc00 && unit <= 0xdfff) return { u: Array.from({ length: value.length }, (_, index) => value.charCodeAt(index)) }
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = value.charCodeAt(index + 1)
            if (!(next >= 0xdc00 && next <= 0xdfff)) return { u: Array.from({ length: value.length }, (_, index) => value.charCodeAt(index)) }
            index++
        }
    }
    return value
}
function pack(value: unknown): unknown {
    if (value === undefined) return { undefined: true }
    if (typeof value === 'string') return encodeText(value)
    if (typeof value === 'number') return Number.isNaN(value) ? 'NaN' : value === Infinity ? 'Infinity' : value === -Infinity ? '-Infinity' : Object.is(value, -0) ? '-0' : value
    if (value instanceof Float32Array) {
        const view = new DataView(value.buffer, value.byteOffset, value.byteLength)
        return { precision: 'f32', bits: Array.from({ length: value.length }, (_, index) => view.getUint32(index * 4, true)) }
    }
    if (Array.isArray(value)) return Array.from(value, pack)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, pack(child)]))
    return value
}
function prepared(processor: HypaProcesser) {
    return processor.vectors.map(entry => ({ document: entry.content, vector: entry.embedding instanceof Float32Array
        ? pack(entry.embedding) : { precision: 'f64', values: entry.embedding }, alreadySaved: entry.alreadySaved ?? false }))
}
const failed = (error: unknown): Failure => error instanceof Error ? { name: error.name, message: error.message } : { name: 'Error', message: String(error) }
const hex = (bytes: Uint8Array) => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')
// JSON permits numeric overflow and signed zero; NaN has no JSON numeric representation.
function wireJSON(value: unknown): string {
    if (typeof value === 'number') {
        if (Number.isNaN(value)) throw new Error('NaN cannot be returned by the synthetic JSON transport')
        return value === Infinity ? '1e999' : value === -Infinity ? '-1e999' : Object.is(value, -0) ? '-0' : String(value)
    }
    if (Array.isArray(value)) return `[${value.map(item => wireJSON(item === undefined ? null : item)).join(',')}]`
    if (value && typeof value === 'object') return `{${Object.entries(value).filter(([, item]) => item !== undefined).map(([key, item]) => `${JSON.stringify(key)}:${wireJSON(item)}`).join(',')}}`
    const encoded = JSON.stringify(value)
    if (encoded === undefined) throw new Error('Unsupported synthetic JSON value')
    return encoded
}
function readFrame(rows: NativeRow[]): Uint8Array {
    const header = new TextEncoder().encode(JSON.stringify({ entries: rows.map(({ key, dimensions, byteLength }) => ({ key, dimensions, byteLength })) }))
    const result = new Uint8Array(4 + header.length + rows.reduce((count, row) => count + row.bytes.length, 0))
    new DataView(result.buffer).setUint32(0, header.length, true)
    result.set(header, 4)
    let offset = 4 + header.length
    for (const row of rows) { result.set(row.bytes, offset); offset += row.bytes.length }
    return result
}
function writtenRows(frame: Uint8Array): NativeRow[] {
    const size = new DataView(frame.buffer, frame.byteOffset, frame.byteLength).getUint32(0, true)
    const header = JSON.parse(new TextDecoder().decode(frame.subarray(4, 4 + size))) as { entries: Omit<NativeRow, 'bytes'>[] }
    let offset = 4 + size
    return header.entries.map(row => {
        const bytes = frame.slice(offset, offset + row.byteLength)
        offset += row.byteLength
        return { ...row, bytes }
    })
}
const displayRows = (rows: NativeRow[]) => rows.map(({ bytes, ...row }) => ({ ...row, key: row.key, bytes: hex(bytes) }))

afterEach(() => {
    setRuntimePerformanceProfile('normal')
    pluginV2.providerOptions.clear()
    boundary.cache = null
    boundary.fetch = null
    boundary.local = null
    vi.restoreAllMocks()
})

it('records bounded observations from actual Hypa-v1 preparation and scoring', async () => {
    const violations: string[] = []
    const rejectBoundary = (message: string): never => { violations.push(message); throw new Error(message) }
    const assertBoundary = (assertion: () => void) => {
        try { assertion() } catch (error) { violations.push(failed(error).message); throw error }
    }
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (...args) => {
        if (!boundary.fetch) rejectBoundary('Unexpected network request before synthetic boundary setup')
        return (boundary.fetch as typeof fetch)(...args)
    })
    const set = readOracleCases<Entry>('memory/hypa-v1', 'observations')
    const results: unknown[] = []
    const getEmbeds = HypaProcesser.prototype.getEmbeds
    const addText = HypaProcesser.prototype.addText
    const query = HypaProcesser.prototype.similaritySearchScored
    const countChat = ChatTokenizer.prototype.tokenizeChat
    const digest = crypto.subtle.digest.bind(crypto.subtle)
    let events: unknown[] = []
    let hashes: { bytes: number[]; hex?: string }[] = []
    let hashing = false
    let activeEmbedding: { purpose: 'document' | 'query'; shape: 'batch' | 'scalar' } | undefined
    vi.spyOn(crypto.subtle, 'digest').mockImplementation(async (algorithm, data) => {
        const bytes = ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : new Uint8Array(data)
        const record = { bytes: [...bytes] } as { bytes: number[]; hex?: string }
        if (hashing) hashes.push(record)
        const result = await digest(algorithm, data)
        record.hex = hex(new Uint8Array(result))
        return result
    })
    vi.spyOn(HypaProcesser.prototype, 'getEmbeds').mockImplementation(async function(this: HypaProcesser, input, purpose = 'query') {
        const request = { purpose, shape: (Array.isArray(input) ? 'batch' : 'scalar') as 'batch' | 'scalar', texts: Array.isArray(input) ? [...input] : [input] }
        const event: Record<string, unknown> = { kind: 'embedding', request }
        events.push(event)
        activeEmbedding = request
        try {
            const result = await getEmbeds.call(this, input, purpose)
            event.result = result.map(value => value instanceof Float32Array ? pack(value) : value === undefined ? { absent: true } : value === null ? { null: true } : { precision: 'f64', values: value })
            return result
        } catch (error) { event.failure = failed(error); throw error } finally { activeEmbedding = undefined }
    })
    vi.spyOn(HypaProcesser.prototype, 'addText').mockImplementation(async function(this: HypaProcesser, documents) {
        try { await addText.call(this, documents); events.push({ kind: 'prepared', entries: prepared(this) }) }
        catch (error) { events.push({ kind: 'prepare-failed', failure: failed(error), entries: prepared(this) }); throw error }
    })
    vi.spyOn(HypaProcesser.prototype, 'similaritySearchScored').mockImplementation(async function(this: HypaProcesser, input) {
        try { const result = await query.call(this, input); events.push({ kind: 'scored', query: input, results: result }); return result }
        catch (error) { events.push({ kind: 'query-failed', query: input, failure: failed(error) }); throw error }
    })
    vi.spyOn(ChatTokenizer.prototype, 'tokenizeChat').mockImplementation(async function(this: ChatTokenizer, message, args) {
        const count = await countChat.call(this, message, args)
        events.push({ kind: 'token', message: { ...message }, count })
        return count
    })
    for (const entry of set.cases) {
        const input = entry.input
        violations.length = 0
        const provider = `hypa-v1-${entry.id}`
        setRuntimePerformanceProfile(input.profile)
        expect(Math.min(1024, getRuntimePerformanceBudgets().hypaCacheBatchEntries)).toBe(input.target.cacheBatchLimit)
        setDatabase(normalizeDatabaseDefaults({
            aiModel: 'custom', currentPluginProvider: provider, useTokenizerCaching: false,
            hypaModel: input.target.model, hypaCustomSettings: { url: input.target.endpoint ?? '', model: input.target.customModel ?? '', key: '' },
            hanuraiSplit: input.hanurai?.split ?? false, hanuraiTokens: input.hanurai?.reserve ?? 0,
        } as unknown as Database))
        pluginV2.providerOptions.set(provider, { tokenizer: 'custom', tokenizerFunc: (value: string) => new Array<number>(Math.ceil(value.length / 4)).fill(0) })
        const identity = hypaEmbeddingIdentity(input.target.model, input.target.endpoint?.trim(), input.target.customModel)
        const seeds = input.cache?.seeds ?? []
        const seedKeys = await hypaCacheKeys(seeds.map(seed => ({ producer: 'hypa-v1-text', content: seed.text === undefined ? '' : text(seed.text), identity })))
        const stored = new Map<string, NativeRow>()
        seeds.forEach((seed, index) => {
            const value = vector(seed.vector)
            if (!value) throw new Error('A synthetic cache seed must have a vector')
            const bytes = new Uint8Array((value instanceof Float32Array ? value : Float32Array.from(value)).buffer)
            const key = seed.key ?? seedKeys[index]
            stored.set(key, { key, dimensions: value.length, byteLength: bytes.length, bytes })
        })
        const initial = displayRows([...stored.values()])
        let reads = 0
        let writes = 0
        boundary.cache = resilientHypaEmbeddingCache(createNativeHypaEmbeddingCache(async <T>(command: string, args?: unknown): Promise<T> => {
            if (command === 'pds_read_hypa_embeddings') {
                const keys = (args as { keys: string[] }).keys
                const event: Record<string, unknown> = { kind: 'cache-read', keys }
                events.push(event)
                reads++
                if (input.cache?.readFaultAt === reads) { event.failure = { name: 'Error', message: 'synthetic cache read failure' }; throw new Error('synthetic cache read failure') }
                if (input.cache?.malformedReadAt === reads) { event.malformed = [0]; return Uint8Array.of(0) as T }
                const rows = keys.flatMap(key => stored.has(key) ? [stored.get(key)!] : [])
                event.rows = displayRows(rows)
                return readFrame(rows) as T
            }
            if (command === 'pds_write_hypa_embeddings') {
                writes++
                const rows = writtenRows(args as Uint8Array)
                const event: Record<string, unknown> = { kind: 'cache-write', entries: displayRows(rows) }
                events.push(event)
                if (input.cache?.writeFaultAt === writes || rows.some(row => typeof row.key !== 'string')) {
                    const message = rows.some(row => typeof row.key !== 'string') ? 'synthetic invalid cache key' : 'synthetic cache write failure'
                    event.failure = { name: 'Error', message }; throw new Error(message)
                }
                for (const row of rows) stored.set(row.key!, row)
                event.committed = true
                return undefined as T
            }
            return rejectBoundary(`Unplanned cache command: ${command}`)
        }, false))
        let embeddingIndex = 0
        const takeEmbedding = (purpose: 'document' | 'query', shape: 'scalar' | 'batch', texts: string[]) => {
            const next = input.embeddings[embeddingIndex++]
            if (!next) rejectBoundary(`Unplanned embedding request in ${entry.id}`)
            assertBoundary(() => expect(pack({ purpose, shape, texts })).toEqual({ purpose: next.purpose, shape: next.shape, texts: next.texts }))
            if (next.failure) { const error = new Error(next.failure.message); error.name = next.failure.name; throw error }
            return next.result!.map(vector)
        }
        boundary.fetch = (async (url, init) => {
            let decoded!: { destination: string; body: { input: string[] | string; model?: string }; purpose: 'document' | 'query'; shape: 'batch' | 'scalar' }
            assertBoundary(() => {
                if (String(url) !== `${hubURL}/proxy2` || init?.method !== 'POST') throw new Error(`Unexpected synthetic transport: ${String(url)}`)
                const headers = new Headers(init.headers)
                const destination = decodeURIComponent(headers.get('risu-url') ?? '')
                const endpoint = input.target.endpoint?.trim() ?? ''
                const expected = endpoint.endsWith('/embeddings') ? endpoint : `${endpoint}/embeddings`
                if (destination !== expected || input.target.model !== 'custom') throw new Error(`Unexpected synthetic destination: ${destination}`)
                if (typeof init.body !== 'string') throw new Error('Synthetic embedding request must have a JSON body')
                const body = JSON.parse(init.body) as typeof decoded.body
                expect(Object.keys(body).sort()).toEqual(['input', 'model'])
                if (body.model !== input.target.customModel?.trim()) throw new Error('Unexpected synthetic embedding model')
                const value = body.input
                if (!(typeof value === 'string' || (Array.isArray(value) && value.every(item => typeof item === 'string')))) throw new Error('Unexpected synthetic embedding input shape')
                const shape = Array.isArray(value) ? 'batch' : 'scalar'
                expect(shape).toBe(activeEmbedding!.shape)
                decoded = { destination, body, purpose: activeEmbedding!.purpose, shape }
            })
            const { destination, body, purpose, shape } = decoded
            events.push({ kind: 'wire', destination, method: init!.method, purpose, shape, body })
            const values = takeEmbedding(purpose, shape, Array.isArray(body.input) ? body.input : [body.input])
            let response!: string
            assertBoundary(() => { response = wireJSON({ data: values.map(embedding => ({ embedding })) }) })
            return new Response(response, { status: 200, headers: { 'Content-Type': 'application/json' } })
        }) satisfies typeof fetch
        boundary.local = (async (texts) => takeEmbedding(activeEmbedding!.purpose, activeEmbedding!.shape, texts) as Float32Array[]) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding']
        events = []
        hashes = []
        boundary.alerts.length = 0
        hashing = true
        let outcome: unknown
        try {
            if (input.mode === 'keys') {
                outcome = { keys: await hypaCacheKeys(input.documents.map(content => ({ producer: 'hypa-v1-text', content: text(content), identity }))) }
            } else if (input.mode === 'hanurai') {
                const arg = input.hanurai!
                const chats = structuredClone(arg.chats)
                const result = await hanuraiMemory(chats, { currentTokens: arg.currentTokens, maxContextTokens: arg.maxContextTokens, tokenizer: new ChatTokenizer(2, 'name') })
                outcome = result === false ? { error: boundary.alerts.at(-1), chats } : result
            } else {
                const processor = new HypaProcesser()
                await processor.addText(input.documents.map(text))
                const queries: unknown[] = []
                for (const value of input.queries) queries.push(await processor.similaritySearchScored(text(value)))
                outcome = { prepared: prepared(processor), queries }
            }
        } catch (error) { outcome = { failure: failed(error) } }
        hashing = false
        expect(violations, entry.id).toEqual([])
        expect(embeddingIndex, JSON.stringify({ id: entry.id, outcome: pack(outcome), events: pack(events) })).toBe(input.embeddings.length)
        results.push(pack({ boundary: 'hypa-v1-real-retrieval', contextLimitError: language.errors.toomuchtoken, initial, hashes, events, outcome, durable: displayRows([...stored.values()]), alerts: boundary.alerts }))
        pluginV2.providerOptions.delete(provider)
    }
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: [
        'src/ts/process/memory/hypamemory.ts', 'src/ts/process/memory/hypaCacheKey.ts',
        'src/ts/storage/hypaEmbeddingCache.ts', 'src/ts/runtimePerformanceProfile.ts',
        'src/ts/process/memory/hanuraiMemory.ts', 'src/ts/tokenizer.ts', 'src/ts/globalApi.svelte.ts',
    ] })).toMatch(/written|matched/)
    if (process.env.RUST_CORE_ORACLE_WRITE === '1') {
        const expected = JSON.parse(readFileSync(set.expectedPath, 'utf8')) as { tsSourceSha256: Record<string, string> }
        writeFileSync(join(dirname(set.expectedPath), 'engine.json'), `${JSON.stringify({ node: process.version, v8: process.versions.v8, unicode: process.versions.unicode, tsSourceSha256: expected.tsSourceSha256 }, null, 2)}\n`)
    }
}, 60000)
type ConversionVector = { precision: 'f64'; values: Scalar[] }
    | { precision: 'f32'; bits: number[]; view?: { offset: number; length: number } }

it('records explicit typed vector conversion through the real cache helper', () => {
    const set = readOracleCases<{ id: string; input: { vectors: ConversionVector[] } }>('memory/hypa-v1', 'precision')
    const results = set.cases.map(entry => ({
        boundary: 'hypa-v1-typed-vector-conversion',
        conversions: entry.input.vectors.map(input => {
            let value = vector(input)
            if (!value) throw new Error('Typed conversion fixtures require numeric vectors')
            if (input.precision === 'f32' && input.view) {
                if (!(value instanceof Float32Array)) throw new Error('A view requires an f32 fixture')
                value = value.subarray(input.view.offset, input.view.offset + input.view.length)
            }
            return { dimensions: value.length, bytesHex: hex(new Uint8Array(toVectorBuffer(value))) }
        }),
    }))
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: ['src/ts/storage/hypaEmbeddingCache.ts'] })).toMatch(/written|matched/)
})
