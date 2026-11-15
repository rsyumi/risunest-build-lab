import { expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))
// Fresh record IDs come from the random source; a counter per case keeps the expected files stable.
const ids = vi.hoisted(() => ({ next: 0 }))
const nextId = () => `00000000-0000-4000-8000-${String(++ids.next).padStart(12, '0')}`
vi.mock('uuid', async (original) => {
    const actual = await original<typeof import('uuid')>()
    return { ...actual, v4: () => nextId() }
})

import { prepareExternalDatabaseImport } from '../../src/ts/storage/upstreamImport'
import { prepareUpstreamImport } from '../../src/ts/storage/importedIdentity'
import { assignIds, checkCharOrder, prepareDatabaseForPersistence } from '../../src/ts/storage/databasePreparation'
import { personaMirrorMap, presetMirrorMap, protectedPresetGroups } from '../../src/ts/storage/effectiveIdentityState'
import { importRisuSaveFromPicker, type RisuSaveFileRouteDependencies } from '../../src/ts/storage/risuSaveFileRoute'
import { decodeRisuSave, encodeRisuSaveLegacy } from '../../src/ts/storage/risuSave'
import type { Database } from '../../src/ts/storage/database.svelte'

type RisuSaveCase = OracleCase & { decline?: string } & (
    | { kind: 'constants' }
    | { kind: 'external' | 'upstream' | 'route' | 'assign-ids' | 'char-order'; database: unknown }
    | { kind: 'chain'; database: unknown; now: number }
)

// The case file comes from formats-risusave.cases.mjs and drives the real import steps.
const settle = {
    generator: import.meta.url,
    tsSources: [
        'src/ts/storage/upstreamImport.ts', 'src/ts/storage/importedIdentity.ts', 'src/ts/storage/effectiveIdentityState.ts',
        'src/ts/storage/databasePreparation.ts', 'src/ts/storage/risuSaveFileRoute.ts', 'src/ts/storage/risuSave.ts',
        'src/ts/storage/database.svelte.ts', 'src/ts/storage/saveCoordinatorHelpers.ts', 'src/ts/process/coldPayloadExpansion.ts',
    ],
}
const set = readOracleCases<RisuSaveCase>('formats', 'risusave')
const message = (error: unknown) => error instanceof Error ? error.message : String(error)

// The database the web `.risudat` import hands the persistent store, for `database` written as a compressed legacy
// save. The persistent runtime and the file picker are the boundary; the decoder and the import steps are the app's.
// With `now`, the stored database is the one the web runtime's `replacePersistentDatabase` prepares from it
// (`persistentDataRuntime.ts`: `prepareDatabase(database)`, which the app wires to `prepareDatabaseForPersistence`),
// with `Date.now()` at `now`.
async function imported(database: unknown, now?: number): Promise<string> {
    const bytes = encodeRisuSaveLegacy(database, 'compression')
    let stored: string | undefined
    const dependencies = {
        platform: () => 'web',
        runtime: () => ({
            replacePersistentDatabase: async (candidate: Database) => {
                if (now === undefined) {
                    stored = JSON.stringify(candidate)
                } else {
                    const clock = vi.spyOn(Date, 'now').mockReturnValue(now)
                    try {
                        stored = JSON.stringify(await prepareDatabaseForPersistence(candidate))
                    } finally {
                        clock.mockRestore()
                    }
                }
                return { revision: 1, projection: 'applied' }
            },
        }),
        chooseWebImport: async () => [{ name: 'synthetic.risudat', size: bytes.byteLength, arrayBuffer: async () => bytes.slice().buffer }],
        decodeRisuSave: async (data: Uint8Array) => {
            const decoded = await decodeRisuSave(data)
            // The Rust replay starts from the case's database, so the codec must hand the steps exactly that.
            if (JSON.stringify(decoded) !== JSON.stringify(database)) throw new Error('the decoded save differs from the case database')
            return decoded
        },
        reloadPlugins: () => {},
    } as unknown as RisuSaveFileRouteDependencies
    const result = await importRisuSaveFromPicker({}, dependencies)
    if (result?.mode !== 'web' || stored === undefined) throw new Error('the route stored no database')
    return stored
}

async function run(entry: RisuSaveCase): Promise<unknown> {
    ids.next = 0
    try {
        if (entry.kind === 'constants') return { ok: JSON.stringify({ presetMirrorMap, personaMirrorMap, protectedPresetGroups }) }
        const database = structuredClone(entry.database) as Database
        switch (entry.kind) {
            case 'external': return { ok: JSON.stringify(prepareExternalDatabaseImport(database)) }
            case 'upstream': return { ok: JSON.stringify(prepareUpstreamImport(database)) }
            case 'route': return { ok: await imported(entry.database) }
            case 'chain': return { ok: await imported(entry.database, entry.now) }
            case 'assign-ids': return { ok: JSON.stringify(assignIds(database, nextId)) }
            case 'char-order': return { ok: JSON.stringify(checkCharOrder(database)) }
        }
    } catch (error) {
        return { error: message(error) }
    }
}

it('maps the RisuSave import cases', async () => {
    const results: unknown[] = []
    for (const entry of set.cases) results.push(await run(entry))
    const outcomes = results as { ok?: unknown; error?: string }[]
    expect(outcomes.filter(result => typeof result.ok === 'string').length).toBeGreaterThan(50)
    // TS raises no error on purpose in these steps, so every case the port does not decline produced a database.
    set.cases.forEach((entry, index) => {
        if (!entry.decline) expect(outcomes[index], entry.id).toHaveProperty('ok')
    })
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
