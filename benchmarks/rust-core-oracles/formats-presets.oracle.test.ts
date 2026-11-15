import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync } from 'node:fs'
import { compressSync } from 'fflate'
import { encode as encodeMsgpack } from 'msgpackr/index-no-eval'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))
// Fresh preset IDs come from the random source; a counter per case keeps the expected files stable.
const ids = vi.hoisted(() => ({ next: 0 }))
vi.mock('uuid', async (original) => {
    const actual = await original<typeof import('uuid')>()
    return { ...actual, v4: () => `00000000-0000-4000-8000-${String(++ids.next).padStart(12, '0')}` }
})
// The persistent store and the download are the I/O boundary. The store holds the case's preset as its only record
// and keeps what a mutation pushes; the download keeps the file it is given.
const io = vi.hoisted(() => ({ stored: undefined as unknown, pushed: [] as unknown[][], downloads: [] as { name: string; data: Uint8Array }[] }))
vi.mock('../../src/ts/storage/persistentDataRuntime.svelte', async (original) => {
    const actual = await original<typeof import('../../src/ts/storage/persistentDataRuntime.svelte')>()
    return {
        ...actual,
        mutatePersistentPresets: async (_reason: string, mutate: (state: { root: object; presets: unknown[] }) => void) => {
            const state = { root: {}, presets: [] as unknown[] }
            mutate(state)
            if (state.presets.length > 0) io.pushed.push(state.presets)
        },
        getPersistentDataRuntime: () => ({
            revision: 1,
            store: {
                queryPresets: async () => ({ revision: 1, items: [{ configuredIndex: 0, id: 'stored' }] }),
                readPreset: async () => ({ revision: 1, value: structuredClone(io.stored) }),
            },
        }),
    }
})
vi.mock('../../src/ts/globalApi.svelte', async (original) => ({
    ...(await original<typeof import('../../src/ts/globalApi.svelte')>()),
    downloadFile: async (name: string, data: Uint8Array) => { io.downloads.push({ name, data: new Uint8Array(data) }); return true },
}))

import { downloadPreset, importPreset, presetTemplate } from '../../src/ts/storage/database.svelte'
import { prebuiltPresets } from '../../src/ts/process/templates/templates'
import { DBState } from '../../src/ts/stores.svelte'
import { alertStore } from '../../src/ts/alertQueue'
import { encryptBuffer } from '../../src/ts/bufferCrypto'
import { encodeRPack } from '../../src/ts/rpack/rpack_js'

// Notices are the UI boundary: each one is dismissed where the app shows it.
alertStore.subscribe((shown) => {
    if (shown.type === 'normal') queueMicrotask(() => alertStore.set({ type: 'none', msg: '' }))
})

type PresetCase = OracleCase & { decline?: string } & (
    | { kind: 'constants' }
    | { kind: 'import'; data: unknown }
    | { kind: 'export'; preset: unknown }
    | { kind: 'chain'; via: 'json' | 'risup'; preset: unknown }
    | { kind: 'risup'; extension: 'risup' | 'risupreset'; container: unknown }
)

// The case file comes from formats-presets.cases.mjs and drives the real mapping functions.
const settle = {
    generator: import.meta.url,
    tsSources: ['src/ts/storage/database.svelte.ts', 'src/ts/storage/upstreamImport.ts', 'src/ts/process/templates/templates.ts', 'src/ts/storage/presetWorkingSetOperations.ts', 'src/ts/bufferCrypto.ts'],
}
const set = readOracleCases<PresetCase>('formats', 'presets')
const message = (error: unknown) => error instanceof Error ? error.message : String(error)
const rpackMap = readFileSync('src/ts/rpack/rpack_map.bin')

afterEach(() => { vi.restoreAllMocks() })

// The RPack codec of `.risup` files loads its map over fetch, from a file the app serves.
function serveAppFiles() {
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
        const url = String(input)
        if (url.endsWith('rpack_map.bin')) return new Response(rpackMap)
        throw new Error(`unexpected fetch ${url}`)
    })
}

// The preset `importPreset` adds, as the store keeps it.
async function imported(file: { name: string; data: Uint8Array }): Promise<string> {
    io.pushed.length = 0
    await importPreset(file)
    if (io.pushed.length !== 1 || io.pushed[0].length !== 1) throw new Error(`${io.pushed.length} mutations pushed a preset`)
    return JSON.stringify(io.pushed[0][0])
}

// The stored preset `downloadPreset` writes, as the file it hands the download or returns.
async function download(preset: unknown, type: 'json' | 'return'): Promise<Uint8Array> {
    io.stored = structuredClone(preset)
    io.downloads.length = 0
    DBState.db = { botPresets: [{ name: (preset as { name?: string }).name }] } as unknown as typeof DBState.db
    const result = await downloadPreset(0, type)
    if (type === 'return') return result.buf!
    if (io.downloads.length !== 1) throw new Error(`${io.downloads.length} downloads`)
    return io.downloads[0].data
}

// A `.risup` file as `downloadPreset` builds one (`.risupreset` without the RPack layer): the container's
// `{"$encrypted": body}` values become the encrypted msgpack of `body`, everything else is encoded as it is.
async function risupFile(extension: 'risup' | 'risupreset', container: unknown): Promise<{ name: string; data: Uint8Array }> {
    const resolve = async (value: unknown): Promise<unknown> => {
        if (value === null || typeof value !== 'object' || Array.isArray(value)) return value
        if (Object.hasOwn(value, '$encrypted')) return encryptBuffer(encodeMsgpack((value as { $encrypted: unknown }).$encrypted), 'risupreset')
        const resolved: Record<string, unknown> = {}
        for (const [key, item] of Object.entries(value)) resolved[key] = await resolve(item)
        return resolved
    }
    const packed = compressSync(encodeMsgpack(await resolve(container)))
    return { name: `synthetic.${extension}`, data: extension === 'risup' ? await encodeRPack(packed) : packed }
}

async function run(entry: PresetCase): Promise<unknown> {
    ids.next = 0
    DBState.db = { botPresets: [] } as unknown as typeof DBState.db
    try {
        switch (entry.kind) {
            case 'constants': return { ok: JSON.stringify({ template: presetTemplate, nai: prebuiltPresets.NAI }) }
            case 'import': return { ok: await imported({ name: 'synthetic.json', data: new TextEncoder().encode(JSON.stringify(entry.data)) }) }
            case 'export': return { ok: JSON.stringify(JSON.parse(new TextDecoder().decode(await download(entry.preset, 'json')))) }
            case 'chain': {
                const data = await download(entry.preset, entry.via === 'json' ? 'json' : 'return')
                return { ok: await imported({ name: `chain.${entry.via}`, data }) }
            }
            case 'risup': return { ok: await imported(await risupFile(entry.extension, entry.container)) }
        }
    } catch (error) {
        return { error: message(error) }
    }
}

it('maps the preset cases', async () => {
    // The Rust port carries both presets as JSON, so neither may hold a value JSON drops.
    expect(JSON.parse(JSON.stringify(presetTemplate))).toStrictEqual(presetTemplate)
    expect(JSON.parse(JSON.stringify(prebuiltPresets.NAI))).toStrictEqual(prebuiltPresets.NAI)
    serveAppFiles()
    const results: unknown[] = []
    for (const entry of set.cases) results.push(await run(entry))
    const outcomes = results as { ok?: unknown; error?: string }[]
    expect(outcomes.filter(result => typeof result.ok === 'string').length).toBeGreaterThan(50)
    // Every case the port does not decline produced a preset or a file.
    set.cases.forEach((entry, index) => {
        if (!entry.decline) expect(outcomes[index], entry.id).toHaveProperty('ok')
    })
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
