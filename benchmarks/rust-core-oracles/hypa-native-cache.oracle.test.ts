import { afterEach, expect, it, vi } from 'vitest'
import { writeFileSync } from 'node:fs'
import { join } from 'node:path'
import {
    createNativeHypaEmbeddingCache, encodeHypaWriteFrame, resilientHypaEmbeddingCache,
    type HypaEmbeddingCache,
} from '../../src/ts/storage/hypaEmbeddingCache'
import { setRuntimePerformanceProfile } from '../../src/ts/runtimePerformanceProfile'
import { readOracleCases, settleOracle } from './oracleFile'

type Utf16 = string | { u: number[] }
type WriteEntry = { key: string; model: Utf16; endpoint: Utf16 | null; bits: number[] }
type ReadRow = { key: string; dimensions: number; bytesHex: string }
type Input =
    | { mode: 'write'; entries: WriteEntry[] }
    | { mode: 'read'; keys: string[]; rows: ReadRow[]; profile?: 'normal' | 'low-spec' }
type Case = { id: string; input: Input }

const text = (value: Utf16) => typeof value === 'string' ? value : String.fromCharCode(...value.u)
const hex = (bytes: Uint8Array) => Buffer.from(bytes).toString('hex')
const bytes = (value: string) => Uint8Array.from(Buffer.from(value, 'hex'))

// This is the invoke response fixture, not an implementation of the TS decoder.
function readFrame(rows: ReadRow[]): Uint8Array {
    const vectors = rows.map(row => bytes(row.bytesHex))
    const header = new TextEncoder().encode(JSON.stringify({ entries: rows.map((row, index) => ({
        key: row.key, dimensions: row.dimensions, byteLength: vectors[index].length,
    })) }))
    const frame = new Uint8Array(4 + header.length + vectors.reduce((sum, vector) => sum + vector.length, 0))
    new DataView(frame.buffer).setUint32(0, header.length, true)
    frame.set(header, 4)
    let offset = 4 + header.length
    for (const vector of vectors) { frame.set(vector, offset); offset += vector.length }
    return frame
}

async function observe(input: Input) {
    if (input.mode === 'write') {
        const frame = encodeHypaWriteFrame(input.entries.map(entry => {
            const vector = new ArrayBuffer(entry.bits.length * 4)
            const view = new DataView(vector)
            entry.bits.forEach((bits, index) => view.setUint32(index * 4, bits, true))
            return {
                key: entry.key, producer: 'hypa-v1-text', model: text(entry.model),
                endpoint: entry.endpoint === null ? null : text(entry.endpoint),
                preprocessVersion: 1, dimensions: entry.bits.length, vector, metadata: null,
            }
        }))
        const headerBytes = new DataView(frame.buffer, frame.byteOffset, frame.byteLength).getUint32(0, true)
        return { frameHex: hex(frame), frameBytes: frame.length, headerBytes, bodyBytes: frame.length - 4 - headerBytes }
    }
    setRuntimePerformanceProfile(input.profile ?? 'normal')
    const rows = new Map(input.rows.map(row => [row.key, row]))
    const native = createNativeHypaEmbeddingCache(async <T>(command: string, args?: unknown): Promise<T> => {
        expect(command).toBe('pds_read_hypa_embeddings')
        const { keys } = args as { keys: string[] }
        return readFrame(keys.map(key => rows.get(key) ?? { key, dimensions: 0, bytesHex: '' })) as T
    }, false)
    let readStatus: 'ok' | 'unavailable' = 'ok'
    const observed: HypaEmbeddingCache = {
        async read(keys) {
            try { return await native.read(keys) }
            catch (error) { readStatus = 'unavailable'; throw error }
        },
        write: entries => native.write(entries),
    }
    const hits = await resilientHypaEmbeddingCache(observed).read(input.keys)
    return { readStatus, hits: [...hits].map(([key, row]) => ({
        key, dimensions: row.dimensions,
        bits: Array.from({ length: row.vector.length }, (_, index) =>
            new DataView(row.vector.buffer, row.vector.byteOffset, row.vector.byteLength).getUint32(index * 4, true)),
    })) }
}

afterEach(() => {
    setRuntimePerformanceProfile('normal')
    vi.restoreAllMocks()
})

it('observes native cache frames through the actual TS encoder and resilient decoder', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => undefined)
    const set = readOracleCases<Case>('memory/hypa-native-cache', 'frames')
    const results: unknown[] = []
    for (const entry of set.cases) {
        setRuntimePerformanceProfile('normal')
        results.push(await observe(entry.input))
    }
    settleOracle(set, results, {
        generator: import.meta.url,
        tsSources: ['src/ts/storage/hypaEmbeddingCache.ts', 'src/ts/runtimePerformanceProfile.ts'],
    })
    if (process.env.RUST_CORE_ORACLE_WRITE === '1') {
        writeFileSync(join(set.root, 'tests/fixtures/rust-core/memory/hypa-native-cache/engine.json'), `${JSON.stringify({
            node: process.version, v8: process.versions.v8, unicode: process.versions.unicode,
        }, null, 2)}\n`)
    }
})
