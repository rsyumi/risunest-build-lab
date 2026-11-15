import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { compressSync, zlibSync, deflateSync, decompressSync } from 'fflate'
import { encode as encodeMsgpack, decode as decodeMsgpack } from 'msgpackr/index-no-eval'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({ ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {} }))
const io = vi.hoisted(() => ({ next: 0, stored: undefined as unknown, pushed: [] as unknown[][], downloads: [] as Uint8Array[] }))
vi.mock('uuid', async (original) => ({ ...(await original<typeof import('uuid')>()), v4: () => `00000000-0000-4000-8000-${String(++io.next).padStart(12, '0')}` }))
vi.mock('../../src/ts/storage/persistentDataRuntime.svelte', async (original) => ({
    ...(await original<typeof import('../../src/ts/storage/persistentDataRuntime.svelte')>()),
    mutatePersistentPresets: async (_reason: string, mutate: (state: { root: object; presets: unknown[] }) => void) => {
        const state = { root: {}, presets: [] as unknown[] }; mutate(state); if (state.presets.length) io.pushed.push(state.presets)
    },
    getPersistentDataRuntime: () => ({ revision: 1, store: {
        queryPresets: async () => ({ revision: 1, items: [{ configuredIndex: 0, id: 'stored' }] }),
        readPreset: async () => ({ revision: 1, value: structuredClone(io.stored) }),
    } }),
}))
vi.mock('../../src/ts/globalApi.svelte', async (original) => ({
    ...(await original<typeof import('../../src/ts/globalApi.svelte')>()),
    downloadFile: async (_name: string, data: Uint8Array) => { io.downloads.push(new Uint8Array(data)); return true },
}))
import { importPreset, downloadPreset } from '../../src/ts/storage/database.svelte'
import { DBState } from '../../src/ts/stores.svelte'
import { alertStore } from '../../src/ts/alertQueue'
import { encryptBuffer, decryptBuffer } from '../../src/ts/bufferCrypto'
import { encodeRPack, decodeRPack } from '../../src/ts/rpack/rpack_js'

alertStore.subscribe(shown => { if (shown.type === 'normal') queueMicrotask(() => alertStore.set({ type: 'none', msg: '' })) })
afterEach(() => vi.restoreAllMocks())
type CodecCase = OracleCase & { kind: 'json' | 'binary' | 'export'; body: Record<string, unknown>; raw?: number[]; rawJson?: string; extension?: string;
    compression?: string; concatenated?: 'same' | 'different'; trailing?: boolean; version?: unknown; type?: string; field?: string; preset?: unknown; presetUndefined?: boolean; duplicates?: boolean;
    innerDuplicate?: boolean; special?: string; truncate?: number; tamper?: boolean; decline?: string; declineBinaryExport?: boolean }
const set = readOracleCases<CodecCase>('formats', 'preset-codec')
const tsSources = ['src/ts/storage/database.svelte.ts', 'src/ts/bufferCrypto.ts', 'src/ts/rpack/rpack_js.js', 'src/ts/storage/presetWorkingSetOperations.ts']
function specialBody(entry: CodecCase) {
    const value = entry.special === 'binary' ? new Uint8Array([1, 2, 255]) : entry.special === 'date' ? new Date(0)
        : entry.special === 'bigint' ? 9007199254740993n : entry.special === 'map' ? new Map([[1, 'one']])
            : entry.special === 'nonfinite' ? Infinity : entry.special === 'surrogate' ? '\ud800' : undefined
    return entry.special ? { ...entry.body, extra: value } : entry.body
}
function pairs(entries: [string, unknown][]) {
    return new Uint8Array(Buffer.concat([Buffer.from([0x80 | entries.length]), ...entries.flatMap(([key, value]) => [encodeMsgpack(key), encodeMsgpack(value)])]))
}
async function file(entry: CodecCase) {
    if (entry.kind === 'json') return { name: `synthetic.${entry.extension ?? 'json'}`, data: entry.raw ? new Uint8Array(entry.raw) : new TextEncoder().encode((entry.rawJson ?? JSON.stringify(entry.body))) }
    const inner = entry.innerDuplicate ? pairs([['name', 'first'], ['10', 'ten'], ['a', 1], ['2', 'two'], ['a', 2], ['name', 'last']]) : encodeMsgpack(specialBody(entry))
    const encrypted = new Uint8Array(await encryptBuffer(inner, 'risupreset'))
    if (entry.tamper) encrypted[encrypted.length - 1] ^= 1
    const field = entry.field ?? 'preset'
    const entries: [string, unknown][] = [['presetVersion', entry.version ?? 2], ['type', entry.type ?? 'preset']]
    if (entry.duplicates) entries.unshift(['presetVersion', 9], ['type', 'wrong'], ['preset', false])
    if (entry.presetUndefined) entries.push(['preset', undefined])
    if (Object.hasOwn(entry, 'preset')) entries.push(['preset', entry.preset])
    entries.push([field, encrypted])
    const envelope = pairs(entries)
    const compress = entry.compression === 'zlib' ? zlibSync : entry.compression === 'raw' ? deflateSync : compressSync
    let data = compress(envelope)
    if (entry.concatenated) data = new Uint8Array(Buffer.concat([data, compress(entry.concatenated === 'same' ? envelope : encodeMsgpack({ other: true }))]))
    if (entry.trailing) data = new Uint8Array(Buffer.concat([data, new Uint8Array(8)]))
    if (entry.truncate) data = data.slice(0, -entry.truncate)
    const extension = entry.extension ?? 'risupreset'
    return { name: `synthetic.${extension}`, data: extension === 'risup' ? await encodeRPack(data) : data }
}
async function imported(file: { name: string; data: Uint8Array }) {
    io.next = 0; io.pushed.length = 0; DBState.db = { botPresets: [] } as unknown as typeof DBState.db
    await importPreset(file)
    expect(io.pushed).toHaveLength(1)
    return JSON.stringify(io.pushed[0][0], (_key, value) => typeof value === 'bigint' ? { $bigint: String(value) } : value)
}
async function decodedInput(input: { name: string; data: Uint8Array }) {
    if (!input.name.endsWith('.risup') && !input.name.endsWith('.risupreset')) return JSON.parse(Buffer.from(input.data).toString('utf8'))
    const packed = input.name.endsWith('.risup') ? await decodeRPack(input.data) : input.data
    const outer = decodeMsgpack(decompressSync(packed)) as { preset?: Uint8Array; pres?: Uint8Array }
    return decodeMsgpack(new Uint8Array(await decryptBuffer((outer.preset ?? outer.pres)!, 'risupreset')))
}async function binaryBody(data: Uint8Array) {
    const outer = decodeMsgpack(decompressSync(await decodeRPack(data))) as { presetVersion: number; type: string; preset: Uint8Array }
    expect({ version: outer.presetVersion, type: outer.type }).toEqual({ version: 2, type: 'preset' })
    return decodeMsgpack(new Uint8Array(await decryptBuffer(outer.preset, 'risupreset')))
}
it('records real preset bytes and TS import/export outcomes for native cross-readers', async () => {
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(Date, 'now').mockReturnValue(1700000000000)
    const rpackMap = readFileSync('src/ts/rpack/rpack_map.bin')
    vi.spyOn(globalThis, 'fetch').mockImplementation(async input => {
        if (String(input).endsWith('rpack_map.bin')) return new Response(rpackMap)
        throw new Error(`unexpected fetch ${String(input)}`)
    })
    const results: unknown[] = []
    for (const entry of set.cases) {
        if (entry.kind === 'export') {
            io.stored = entry.rawJson ? JSON.parse(entry.rawJson) : structuredClone(entry.body); io.downloads.length = 0
            DBState.db = { botPresets: [{ name: (io.stored as { name: string }).name }] } as unknown as typeof DBState.db
            await downloadPreset(0, 'json')
            const json = Buffer.from(io.downloads[0]).toString('utf8')
            const returned = await downloadPreset(0, 'return')
            const nativeDir = process.env.RUST_CORE_PRESET_CODEC_NATIVE_DIR
            if (nativeDir) {
                expect(readFileSync(join(nativeDir, `${entry.id}.json`), 'utf8')).toBe(json)
                if (!entry.declineBinaryExport) {
                    const native = new Uint8Array(readFileSync(join(nativeDir, `${entry.id}.risup`)))
                    expect(JSON.stringify(await binaryBody(native))).toBe(JSON.stringify(JSON.parse(json)))
                    expect(await imported({ name: 'native.risup', data: native })).toBe(await imported({ name: 'ts.risup', data: returned.buf! }))
                }
            }
            results.push({ json, binaryBase64: Buffer.from(returned.buf!).toString('base64'), localData: JSON.stringify(returned.data) })
            continue
        }
        const input = await file(entry)
        let outcome: { importedJson: string } | { error: string }
        try { outcome = { importedJson: await imported(input) } } catch (error) { outcome = { error: error instanceof Error ? error.message : String(error) } }
        const decodedJson = entry.decline ? {} : { decodedJson: JSON.stringify(await decodedInput(input)) }
        results.push({ name: input.name, bytesBase64: Buffer.from(input.data).toString('base64'), ...decodedJson, ...outcome })
    }
    const nativeDir = process.env.RUST_CORE_PRESET_CODEC_NATIVE_DIR
    if (nativeDir) for (const entry of set.cases.filter(entry => entry.kind === 'export' && !entry.declineBinaryExport)) {
        io.stored = JSON.parse(readFileSync(join(nativeDir, `job-${entry.id}.source.json`), 'utf8'))
        io.downloads.length = 0
        DBState.db = { botPresets: [{ name: (io.stored as { name: string }).name }] } as unknown as typeof DBState.db
        await downloadPreset(0, 'json')
        const tsJson = Buffer.from(io.downloads[0]).toString('utf8')
        expect(readFileSync(join(nativeDir, `job-${entry.id}.json`), 'utf8')).toBe(tsJson)
        if (!entry.declineBinaryExport) {
            const returned = await downloadPreset(0, 'return')
            const native = new Uint8Array(readFileSync(join(nativeDir, `job-${entry.id}.risup`)))
            expect(JSON.stringify(await binaryBody(native))).toBe(JSON.stringify(JSON.parse(tsJson)))
            expect(await imported({ name: 'native-job.risup', data: native })).toBe(await imported({ name: 'ts.risup', data: returned.buf! }))
        }
    }
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources })).toMatch(/written|matched/)
})
