import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readOracleCases, settleOracle } from './oracleFile'

// Stores stay inert during this policy-only invocation, as in the tokenizer oracle.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: () => () => {} }
})
vi.mock('@mlc-ai/web-tokenizers', async (original) => {
    await original()
    return { ...(globalThis as { tokenizers?: object }).tokenizers }
})
const boundary = vi.hoisted(() => ({
    cache: null as unknown,
    embed: null as unknown,
    alerts: [] as string[],
}))
vi.mock('../../src/ts/alert', async (original) => ({
    ...(await original<typeof import('../../src/ts/alert')>()),
    alertError: (value: string | Error) => { boundary.alerts.push(String(value)) },
}))
vi.mock('../../src/ts/process/transformers', async (original) => ({
    ...(await original<typeof import('../../src/ts/process/transformers')>()),
    runEmbedding: (async (...args) => (boundary.embed as typeof import('../../src/ts/process/transformers')['runEmbedding'])(...args)) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding'],
}))
vi.mock('../../src/ts/storage/hypaEmbeddingCache', async (original) => ({
    ...(await original<typeof import('../../src/ts/storage/hypaEmbeddingCache')>()),
    getHypaEmbeddingCache: () => boundary.cache as HypaEmbeddingCache,
}))

import { normalizeDatabaseDefaults, setDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { ChatTokenizer } from '../../src/ts/tokenizer'
import { language } from '../../src/lang'
import type { OpenAIChat } from '../../src/ts/process/index.svelte'
import { hanuraiMemory } from '../../src/ts/process/memory/hanuraiMemory'
import { HypaProcesser } from '../../src/ts/process/memory/hypamemory'
import { hypaCacheKeys, hypaEmbeddingIdentity } from '../../src/ts/process/memory/hypaCacheKey'
import type { HypaCachedEmbedding, HypaEmbeddingCache } from '../../src/ts/storage/hypaEmbeddingCache'

type Failure = { message: string; name: string }
type Fault = { at: number; message: string; name?: string }
type Score = number | 'NaN' | 'Infinity' | '-Infinity'
type Entry = { id: string; input: {
    chats: OpenAIChat[]; currentTokens: number; maxContextTokens: number; split: boolean; reserve: number
    tokenizer: { additionalTokens: number; useName: 'name' | 'noName' }
    embeddings: { documents?: Record<string, number[]>; queries?: Record<string, number[]>; defaultDocument?: number[]; defaultQuery?: number[]; fail?: Fault }
    cache?: { text: string; vector: number[] }[]
    tokenFailure?: Fault
} }
type Event = { kind: 'prepare'; documents: string[]; prepared?: string[]; failure?: Failure }
    | { kind: 'query'; query: string; results?: [string, Score][]; failure?: Failure }
    | { kind: 'token'; message: OpenAIChat; count?: number; failure?: Failure }
type IO = { kind: string; [key: string]: unknown }
const copy = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T
const failure = (value: unknown): Failure => value instanceof Error
    ? { message: value.message, name: value.name } : { message: String(value), name: 'Error' }
const score = (value: number): Score => Number.isNaN(value) ? 'NaN'
    : value === Infinity ? 'Infinity' : value === -Infinity ? '-Infinity' : value
function fail(fault: Fault | undefined, index: number) {
    if (fault?.at === index) {
        const error = new Error(fault.message)
        if (fault.name) error.name = fault.name
        throw error
    }
}

afterEach(() => {
    pluginV2.providerOptions.clear()
    boundary.cache = null
    boundary.embed = null
    vi.restoreAllMocks()
})

it('records real Hanurai policy with real retrieval and message accounting', async () => {
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => { throw new Error(`Unexpected network request: ${String(input)}`) })
    const set = readOracleCases<Entry>('memory/hanurai', 'policy')
    const results: unknown[] = []
    const prepare = HypaProcesser.prototype.addText
    const query = HypaProcesser.prototype.similaritySearchScored
    const tokenize = ChatTokenizer.prototype.tokenizeChat
    let events: Event[] = []
    // Observe the real method result before the caller can perform its next policy operation.
    vi.spyOn(HypaProcesser.prototype, 'addText').mockImplementation(async function(this: HypaProcesser, documents) {
        try {
            await prepare.call(this, documents)
            events.push({ kind: 'prepare', documents: [...documents], prepared: this.vectors.map(vector => vector.content) })
        } catch (error) {
            events.push({ kind: 'prepare', documents: [...documents], failure: failure(error) })
            throw error
        }
    })
    vi.spyOn(HypaProcesser.prototype, 'similaritySearchScored').mockImplementation(async function(this: HypaProcesser, text) {
        try {
            const result = await query.call(this, text)
            events.push({ kind: 'query', query: text, results: result.map(([text, value]) => [text, score(value)]) })
            return result
        } catch (error) {
            events.push({ kind: 'query', query: text, failure: failure(error) })
            throw error
        }
    })
    vi.spyOn(ChatTokenizer.prototype, 'tokenizeChat').mockImplementation(async function(this: ChatTokenizer, message, args) {
        try {
            const count = await tokenize.call(this, message, args)
            events.push({ kind: 'token', message: copy(message), count })
            return count
        } catch (error) {
            events.push({ kind: 'token', message: copy(message), failure: failure(error) })
            throw error
        }
    })
    for (const entry of set.cases) {
        const input = entry.input
        const provider = `hanurai-oracle-${entry.id}`
        setDatabase(normalizeDatabaseDefaults({
            aiModel: 'custom', currentPluginProvider: provider, useTokenizerCaching: false,
            hanuraiSplit: input.split, hanuraiTokens: input.reserve, hypaModel: 'MiniLM',
            enableCustomFlags: true, customFlags: [0], gptVisionQuality: 'high',
        } as unknown as Database))
        let tokenCalls = 0
        const io: IO[] = []
        pluginV2.providerOptions.set(provider, { tokenizer: 'custom', tokenizerFunc: (text: string) => {
            tokenCalls++
            try {
                fail(input.tokenFailure, tokenCalls)
                const count = Math.ceil(text.length / 4)
                io.push({ kind: 'plugin-tokenizer', text, count })
                return new Array<number>(count).fill(0)
            } catch (error) {
                io.push({ kind: 'plugin-tokenizer', text, failure: failure(error) })
                throw error
            }
        } })
        const seeds = input.cache ?? []
        const keys = await hypaCacheKeys(seeds.map(seed => ({ producer: 'hypa-v1-text', content: seed.text, identity: hypaEmbeddingIdentity('MiniLM') })))
        const stored = new Map<string, HypaCachedEmbedding>(seeds.map((seed, index) => [keys[index], {
            vector: new Float32Array(seed.vector), dimensions: seed.vector.length,
        }]))
        boundary.cache = {
            async read(requested) {
                const hits = new Map([...stored].filter(([key]) => requested.includes(key)))
                io.push({ kind: 'cache-read', keys: [...requested], hits: [...hits].map(([key, value]) => ({ key, vector: [...value.vector] })) })
                return hits
            },
            async write(entries) {
                io.push({ kind: 'cache-write', entries: entries.map(value => ({ ...value, vector: [...new Float32Array(value.vector)] })) })
                for (const value of entries) stored.set(value.key, { vector: new Float32Array(value.vector), dimensions: value.dimensions })
            },
        } satisfies HypaEmbeddingCache
        let inferenceCalls = 0
        boundary.embed = (async (texts) => {
            inferenceCalls++
            try {
                fail(input.embeddings.fail, inferenceCalls)
                const vectors = texts.map(text => {
                    const vector = input.embeddings.documents?.[text] ?? input.embeddings.queries?.[text]
                        ?? (text.startsWith('search_document: ') ? input.embeddings.defaultDocument : input.embeddings.defaultQuery)
                    if (!vector) throw new Error(`Unplanned synthetic embedding input: ${text}`)
                    return new Float32Array(vector)
                })
                io.push({ kind: 'inference', texts: [...texts], vectors: vectors.map(vector => [...vector]) })
                return vectors
            } catch (error) {
                io.push({ kind: 'inference', texts: [...texts], failure: failure(error) })
                throw error
            }
        }) satisfies typeof import('../../src/ts/process/transformers')['runEmbedding']
        events = []
        boundary.alerts.length = 0
        const chats = copy(input.chats)
        const contextLimitError = language.errors.toomuchtoken
        let outcome: unknown
        try {
            const result = await hanuraiMemory(chats, {
                currentTokens: input.currentTokens, maxContextTokens: input.maxContextTokens,
                tokenizer: new ChatTokenizer(input.tokenizer.additionalTokens, input.tokenizer.useName),
            })
            outcome = result === false ? { error: boundary.alerts.at(-1), chats } : result
        } catch (error) {
            outcome = { failure: failure(error), chats }
        }
        results.push(copy({ boundary: 'hanurai-policy-replay', contextLimitError, events, outcome, alerts: boundary.alerts, io }))
        pluginV2.providerOptions.delete(provider)
    }
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: [
        'src/ts/process/memory/hanuraiMemory.ts', 'src/ts/process/memory/hypamemory.ts',
        'src/ts/process/memory/hypaCacheKey.ts', 'src/ts/storage/hypaEmbeddingCache.ts',
        'src/ts/tokenizer.ts', 'src/ts/process/files/inlays.ts', 'src/ts/model/modellist.ts',
    ] })).toMatch(/written|matched/)
}, 60000)
