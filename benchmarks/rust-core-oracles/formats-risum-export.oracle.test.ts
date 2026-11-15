import { readFileSync } from 'node:fs'
import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))

import { exportModuleLegacy, type RisuModule } from '../../src/ts/process/modules'
import { forageStorage } from '../../src/ts/globalApi.svelte'
import { decodeRPack } from '../../src/ts/rpack/rpack_js'

type ExportCase = OracleCase & { module: unknown; payloads: Record<string, string>; native: 'ported' | { declined: string } }

// The case file comes from formats-risum-export.cases.mjs and drives the real RISUM writer.
const settle = {
    generator: import.meta.url,
    tsSources: ['src/ts/process/modules.ts', 'src/ts/rpack/rpack_js.js'],
}
const set = readOracleCases<ExportCase>('formats', 'risum-export')
const rpackMap = readFileSync('src/ts/rpack/rpack_map.bin')
const hex = (bytes: Uint8Array) => Buffer.from(bytes).toString('hex')

afterEach(() => { vi.restoreAllMocks() })

// `file` is the whole RISUM file `exportModuleLegacy` returns, and `metadata` its first frame decoded, the text
// `JSON.stringify` wrote, so a difference shows where it starts. The asset store holds the case's payloads, which the
// writer reads by key; the RPack codec loads its map over fetch.
async function run(entry: ExportCase) {
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
        if (String(input).endsWith('rpack_map.bin')) return new Response(rpackMap)
        throw new Error(`unexpected fetch ${String(input)}`)
    })
    for (const [key, payload] of Object.entries(entry.payloads)) await forageStorage.setItem(key, Uint8Array.from(Buffer.from(payload, 'hex')))
    const module = JSON.parse(JSON.stringify(entry.module)) as RisuModule
    const file = await exportModuleLegacy(module, { alertEnd: false, saveData: false })
    const length = new DataView(file.buffer, file.byteOffset).getUint32(2, true)
    const metadata = new TextDecoder().decode(await decodeRPack(file.subarray(6, 6 + length)))
    return { file: hex(file), metadata }
}

it('writes the RISUM export cases', async () => {
    const results: Awaited<ReturnType<typeof run>>[] = []
    for (const entry of set.cases) results.push(await run(entry))
    set.cases.forEach((entry, index) => {
        const metadata = JSON.parse(results[index].metadata) as { module: { assets: unknown[] }; type: string }
        expect(metadata.type, entry.id).toBe('risuModule')
        // Every case writes a file whose asset tuples have their keys blanked.
        expect(metadata.module.assets.every(tuple => Array.isArray(tuple) && tuple[1] === ''), entry.id).toBe(true)
    })
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
