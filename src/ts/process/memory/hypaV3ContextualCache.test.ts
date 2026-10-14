import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({
    database: { hypaModel: 'voyageContext3', hypaCustomSettings: { url: '', key: '', model: '' } },
    groups: [] as string[][][],
    width: 4,
    queryVector: null as number[] | null,
    distinct: false,
    cache: null as unknown,
}))

vi.mock('../transformers', () => ({ runEmbedding: vi.fn() }))
// The app graph runs store effects on import that a loaded database would feed.
vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule())

vi.mock('src/ts/storage/hypaEmbeddingCache', async (importOriginal) => ({
    ...(await importOriginal<typeof import('src/ts/storage/hypaEmbeddingCache')>()),
    getHypaEmbeddingCache: () => state.cache,
}))

import { DBState } from 'src/ts/stores.svelte'
import { setDatabaseLite } from 'src/ts/storage/database.svelte'
import { HypaProcesserEx } from './hypav3'
import { HypaProcessorV2 } from './hypamemoryv2'
import { getContextProvider } from './contextualEmbedding'
import * as globalApi from 'src/ts/globalApi.svelte'
import type {
    HypaCachedEmbedding,
    HypaEmbeddingCache,
    HypaEmbeddingEntry,
} from 'src/ts/storage/hypaEmbeddingCache'

function recordingCache(seed: Map<string, HypaCachedEmbedding> = new Map()) {
    const stored = new Map(seed)
    const reads: string[][] = []
    const writes: HypaEmbeddingEntry[][] = []
    const cache: HypaEmbeddingCache = {
        async read(keys: string[]) {
            reads.push([...keys])
            return new Map([...stored].filter(([key]) => keys.includes(key)))
        },
        async write(entries: HypaEmbeddingEntry[]) {
            writes.push([...entries])
            for (const entry of entries) {
                stored.set(entry.key, {
                    vector: new Float32Array(entry.vector),
                    dimensions: entry.dimensions,
                })
            }
        },
    }
    return { reads, writes, stored, cache }
}

function summary(text: string) {
    return { text, chatMemos: new Set<string>(), isImportant: false }
}

function chunksOf(groups: string[][]) {
    return groups.flatMap((texts, index) => {
        const owner = summary(`summary ${index}`)
        return texts.map((text) => ({ text, summary: owner }))
    })
}

const groups = [
    ['alpha one', 'alpha two', 'alpha three'],
    ['beta one', 'beta two'],
]

beforeEach(() => {
    state.groups = []
    state.width = 4
    state.queryVector = null
    vi.spyOn(globalApi, 'globalFetch').mockImplementation(async (_url, options) => {
        const groups = options.body.inputs as string[][]
        if (options.body.input_type === 'document') state.groups.push(groups.map(group => [...group]))
        return {
            ok: true, status: 200, headers: {},
            data: { data: groups.map(group => ({ data: group.map(() => ({
                embedding: options.body.input_type === 'query' && state.queryVector
                    ? state.queryVector
                    : new Array(state.width).fill(state.distinct && group.includes('second context') ? 2 : 1),
            })) })) },
        }
    })
    state.distinct = false
    setDatabaseLite({ ...state.database, voyageApiKey: 'synthetic-voyage-key' } as never)
    expect(DBState.db.hypaModel).toBe('voyageContext3')
})

afterEach(() => {
    state.cache = null
    vi.restoreAllMocks()
})

describe('HypaProcesserEx contextual chunks', () => {
    it('reads and writes every chunk in one call', async () => {
        const recorder = recordingCache()
        state.cache = recorder.cache

        const processer = new HypaProcesserEx('voyageContext3')
        await processer.addSummaryChunks(chunksOf(groups))

        expect(recorder.reads).toHaveLength(1)
        expect(recorder.reads[0]).toHaveLength(5)
        expect(recorder.writes).toHaveLength(1)
        expect(recorder.writes[0]).toHaveLength(5)
        expect(recorder.writes[0][0].producer).toBe('hypa-v3-group')
        expect(processer.summaryChunkVectors).toHaveLength(5)
        expect(new Set(recorder.reads[0]).size).toBe(5)
    })

    it('embeds nothing when every group is already cached', async () => {
        const recorder = recordingCache()
        state.cache = recorder.cache
        await new HypaProcesserEx('voyageContext3').addSummaryChunks(chunksOf(groups))
        state.groups = []

        const processer = new HypaProcesserEx('voyageContext3')
        await processer.addSummaryChunks(chunksOf(groups))

        expect(state.groups).toHaveLength(0)
        expect(processer.summaryChunkVectors).toHaveLength(5)
    })

    it('re-embeds a whole group when one of its chunks is missing', async () => {
        const recorder = recordingCache()
        state.cache = recorder.cache
        await new HypaProcesserEx('voyageContext3').addSummaryChunks(chunksOf(groups))

        // Drop one chunk of the first group, leaving its siblings cached.
        const firstGroupKeys = recorder.writes[0].slice(0, 3).map((entry) => entry.key)
        recorder.stored.delete(firstGroupKeys[1])
        state.groups = []

        await new HypaProcesserEx('voyageContext3').addSummaryChunks(chunksOf(groups))

        expect(state.groups).toEqual([[groups[0]]])
    })

    it('recomputes cached groups that disagree with the freshly computed width', async () => {
        const recorder = recordingCache()
        state.cache = recorder.cache
        await new HypaProcesserEx('voyageContext3').addSummaryChunks(chunksOf(groups))

        // The first group must be recomputed, which changes the vector width.
        recorder.stored.delete(recorder.writes[0][0].key)
        state.width = 6
        state.groups = []

        const processer = new HypaProcesserEx('voyageContext3')
        await processer.addSummaryChunks(chunksOf(groups))

        expect(state.groups.flat()).toEqual([groups[0], groups[1]])
        for (const entry of processer.summaryChunkVectors) {
            expect(entry.vector.embedding).toHaveLength(6)
        }
    })

    it('separates the chunk key from the group it was embedded with', async () => {
        const first = recordingCache()
        state.cache = first.cache
        await new HypaProcesserEx('voyageContext3').addSummaryChunks(chunksOf(groups))

        const second = recordingCache(first.stored)
        state.cache = second.cache
        state.groups = []
        await new HypaProcesserEx('voyageContext3').addSummaryChunks(
            chunksOf([[groups[0][0], 'gamma one', 'gamma two'], groups[1]]),
        )

        expect(state.groups).toEqual([[[groups[0][0], 'gamma one', 'gamma two']]])
    })
})


it.each(['cold', 'warm', 'mixed'] as const)('preserves repeated text vector identity with a %s contextual cache', async (mode) => {
    state.distinct = true
    const recorder = recordingCache()
    state.cache = recorder.cache
    const groups = [['repeated text', 'first context'], ['repeated text', 'second context']]
    if (mode !== 'cold') {
        await new HypaProcesserEx('voyageContext3').addSummaryChunks(chunksOf(groups))
        if (mode === 'mixed') recorder.stored.delete(recorder.writes[0][0].key)
    }
    state.groups = []
    const chunks = chunksOf(groups)
    const processor = new HypaProcesserEx('voyageContext3')
    await processor.addSummaryChunks(chunks)
    expect(processor.summaryChunkVectors.map(({ chunk, vector }) => [chunks.indexOf(chunk), [...vector.embedding]]))
        .toEqual([[0, [1, 1, 1, 1]], [1, [1, 1, 1, 1]], [2, [2, 2, 2, 2]], [3, [2, 2, 2, 2]]])
    state.queryVector = [1, 0, 0, 0]
    const ranked = await processor.similaritySearchScoredEx('query')
    expect(ranked.map(([chunk, score]) => [chunks.indexOf(chunk), score])).toEqual([[2, 2], [3, 2], [0, 1], [1, 1]])
})


describe.each([
    ['voyageContext3', 'voyage-context-3'],
    ['voyageContext4', 'voyage-context-4'],
] as const)('%s provider integration', (model, apiModel) => {
    it.each(['standard', 'experimental'] as const)('embeds summary groups and queries through the %s processor', async (implementation) => {
        state.cache = recordingCache().cache
        setDatabaseLite({ ...state.database, hypaModel: model, voyageApiKey: '  synthetic-voyage-key  ' } as never)
        const chunks = chunksOf(groups)
        if (implementation === 'standard') {
            const processor = new HypaProcesserEx()
            await processor.addSummaryChunks(chunks)
            const results = await processor.similaritySearchScoredEx('recent conversation')
            expect(results.map(([chunk]) => chunk)).toEqual(chunks)
        } else {
            const processor = new HypaProcessorV2()
            const texts = chunks.map((chunk, index) => ({ id: String(index), content: chunk.text, metadata: chunk.summary }))
            await processor.addTexts(texts)
            const results = await processor.similaritySearchScored('recent conversation')
            expect(results.map(([result]) => result.content)).toEqual(chunks.map(chunk => chunk.text))
            expect(results.every(([, score]) => Number.isFinite(score))).toBe(true)
        }
        const requests = vi.mocked(globalApi.globalFetch).mock.calls
        expect(requests.map(([url, options]) => ({ url, headers: options.headers, body: options.body }))).toEqual([
            {
                url: 'https://api.voyageai.com/v1/contextualizedembeddings',
                headers: { Authorization: 'Bearer synthetic-voyage-key', 'Content-Type': 'application/json' },
                body: { model: apiModel, inputs: groups, input_type: 'document' },
            },
            {
                url: 'https://api.voyageai.com/v1/contextualizedembeddings',
                headers: { Authorization: 'Bearer synthetic-voyage-key', 'Content-Type': 'application/json' },
                body: { model: apiModel, inputs: [['recent conversation']], input_type: 'query' },
            },
        ])
    })

    it.each(['document', 'query'] as const)('batches %s inputs within the model budget and preserves vector order', async (inputType) => {
        const limit = model === 'voyageContext4' ? 32000 : 120000
        const text = inputType === 'document' ? '가'.repeat(Math.floor(limit / 6)) : '\u0344'.repeat(limit / 8)
        const input = inputType === 'document'
            ? [[text + '0', 'context 1'], [text + '2', 'context 3'], [text + '4']]
            : [[text + '0'], [text + '1'], [text + '2']]
        vi.mocked(globalApi.globalFetch).mockImplementation(async (_url, options) => {
            const groups = options.body.inputs as string[][]
            const bytes = groups.flat().reduce((total, value) => total + Buffer.byteLength(value.normalize('NFC'), 'utf8'), 0)
            if (bytes > limit) return { ok: false, status: 400, headers: {}, data: { detail: 'Too many tokens' } }
            return { ok: true, status: 200, headers: {}, data: { data: groups.map(group => ({
                data: group.map(value => ({ embedding: [Number(value.at(-1)), group.length] })),
            })) } }
        })
        const provider = getContextProvider(model)
        if (inputType === 'document') {
            expect(await provider.embedDocumentGroups(input)).toEqual([[[0, 2], [1, 2]], [[2, 2], [3, 2]], [[4, 1]]])
        } else {
            expect(await provider.embedQueries(input.flat())).toEqual([[0, 1], [1, 1], [2, 1]])
        }
    })

    it('sends a large contextual group intact without rejecting a conservative byte estimate', async () => {
        const limit = model === 'voyageContext4' ? 32000 : 120000
        const large = ['a'.repeat(limit), 'same document']
        const input = [['first'], large, ['last']]
        vi.mocked(globalApi.globalFetch).mockImplementation(async (_url, options) => {
            const groups = options.body.inputs as string[][]
            const oversized = groups.some(group => group.some(text => text.length >= limit))
            if (oversized && groups.length > 1) return { ok: false, status: 400, headers: {}, data: { detail: 'Too many tokens' } }
            return { ok: true, status: 200, headers: {}, data: { data: groups.map(group => ({
                data: group.map(value => ({ embedding: [value.length, group.length] })),
            })) } }
        })
        expect(await getContextProvider(model).embedDocumentGroups(input))
            .toEqual([[[5, 1]], [[limit, 2], [13, 2]], [[4, 1]]])
    })

    it.each(['cancel', 'failure'] as const)('keeps earlier batches out of the cache after a later %s', async (outcome) => {
        const recorder = recordingCache()
        state.cache = recorder.cache
        const controller = new AbortController()
        const processor = new HypaProcesserEx(model)
        processor.signal = controller.signal
        const fetch = vi.mocked(globalApi.globalFetch)
        fetch.mockImplementationOnce(fetch.getMockImplementation()!)
        fetch.mockImplementationOnce(async (_url, options) => {
            expect(options.abortSignal).toBe(controller.signal)
            if (outcome === 'cancel') controller.abort()
            return { ok: false, status: 503, headers: {}, data: { detail: 'Synthetic outage' } }
        })
        const limit = model === 'voyageContext4' ? 32000 : 120000
        const text = '가'.repeat(Math.floor(limit / 6))
        const pending = processor.addSummaryChunks(chunksOf([[text + ' first'], [text + ' second'], [text + ' third']]))
        if (outcome === 'cancel') await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
        else await expect(pending).rejects.toThrow('Synthetic outage')
        expect(recorder.stored.size).toBe(0)
        expect(processor.summaryChunkVectors).toEqual([])
        expect(fetch).toHaveBeenCalledTimes(2)
    })

    it('rejects a missing Voyage key before sending a request', async () => {
        setDatabaseLite({ ...state.database, hypaModel: model, voyageApiKey: ' ' } as never)
        await expect(getContextProvider(model).embedQueries(['query'])).rejects.toThrow('requires a Voyage API Key')
        expect(globalApi.globalFetch).not.toHaveBeenCalled()
    })
})

it.each(['standard', 'experimental'] as const)('separates Context 3 and 4 caches in the %s processor', async (implementation) => {
    const recorder = recordingCache()
    state.cache = recorder.cache
    const chunks = chunksOf(groups)
    for (const model of ['voyageContext3', 'voyageContext4', 'voyageContext3', 'voyageContext4'] as const) {
        if (implementation === 'standard') await new HypaProcesserEx(model).addSummaryChunks(chunks)
        else await new HypaProcessorV2({ model }).addTexts(chunks.map((chunk, index) => ({
            id: String(index), content: chunk.text, metadata: chunk.summary,
        })))
    }
    expect(state.groups).toEqual([groups, groups])
    expect(new Set(recorder.writes.flat().map(entry => entry.model))).toEqual(new Set(['voyageContext3', 'voyageContext4']))
    expect(recorder.stored.size).toBe(10)
    // These suffixes are part of the persisted Context 3 cache identity.
    expect(getContextProvider('voyageContext3').getCacheKeySuffix()).toBe('|voyageContext3')
    expect(getContextProvider('voyageContext4').getCacheKeySuffix()).toBe('|voyageContext4')
})
