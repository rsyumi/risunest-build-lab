import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync } from 'node:fs'
import { unzipSync } from 'fflate'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))
// Fresh record IDs come from the random source; a counter per case keeps the expected files stable.
const ids = vi.hoisted(() => ({ next: 0 }))
vi.mock('uuid', async (original) => {
    const actual = await original<typeof import('uuid')>()
    return { ...actual, v4: () => `00000000-0000-4000-8000-${String(++ids.next).padStart(12, '0')}` }
})
// The asset store and file writers are the I/O boundary. The asset store saves the fallback portrait as the case's key,
// and the export writer keeps what it is given. Image reads go to the real browser storage, seeded per case.
const files = vi.hoisted(() => ({ saved: 'saved-none-image', writers: [] as { buf: { buffer: Uint8Array } }[] }))
const png = Uint8Array.from(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64'))
vi.mock('../../src/ts/globalApi.svelte', async (original) => {
    const actual = await original<typeof import('../../src/ts/globalApi.svelte')>()
    class LocalWriter extends actual.VirtualWriter {
        async init() { files.writers.push(this as never); return true }
    }
    return { ...actual, LocalWriter, saveAsset: async () => files.saved }
})

import { language } from '../../src/lang'
import { exportModule, exportModuleLegacy, importModuleData, type RisuModule } from '../../src/ts/process/modules'
import { convertModuleToCharacter } from '../../src/ts/interchangeability'
import { activatePreparedNativeModuleContent } from '../../src/ts/storage/nativeModuleContentActivation'
import type { PreparedNativeContent, PreparedNativeContentActivationLifecycle } from '../../src/ts/storage/nativeFileJobs'
import { ownerManifestIdentity } from '../../src/ts/storage/ownerManifestCodec'
import { decodeRPack } from '../../src/ts/rpack/rpack_js'
import { DBState } from '../../src/ts/stores.svelte'
import { alertStore } from '../../src/ts/alertQueue'
import { forageStorage } from '../../src/ts/globalApi.svelte'

// The dialogs are the UI boundary, answered where the app shows them: a confirmation answers yes and is counted, an
// error or notice is kept and dismissed.
const dialogs = { confirms: 0, alerts: [] as { kind: string; value: string }[] }
alertStore.subscribe((shown) => {
    if (shown.type === 'ask') {
        dialogs.confirms += 1
        queueMicrotask(() => alertStore.set({ type: 'none', msg: 'yes' }))
    } else if (shown.type === 'error' || shown.type === 'normal') {
        dialogs.alerts.push({ kind: shown.type, value: shown.msg })
        queueMicrotask(() => alertStore.set({ type: 'none', msg: '' }))
    }
})

type Staged = { token: string; logicalId: string }
type ModuleCase = OracleCase & { decline?: string; root?: unknown } & (
    | { kind: 'json'; data: unknown }
    | { kind: 'risum'; metadata: unknown; logicalIds: string[] }
    | { kind: 'charx'; card: unknown; assets?: Staged[]; portraitLogicalId?: string; module?: unknown }
    | { kind: 'to-character' | 'risum-export'; module: unknown }
    | { kind: 'module-charx'; module: unknown; noneImage: string; now: number }
)

// The case file comes from formats-modules.cases.mjs and drives the real mapping functions.
const settle = {
    generator: import.meta.url,
    tsSources: [
        'src/ts/process/modules.ts', 'src/ts/interchangeability.ts', 'src/ts/process/lorebook.svelte.ts',
        'src/ts/storage/nativeModuleContentActivation.ts', 'src/ts/characters.ts', 'src/ts/characterCards.ts',
    ],
}
const set = readOracleCases<ModuleCase>('formats', 'modules')
const message = (error: unknown) => error instanceof Error ? error.message : String(error)
const hash = (seed: number) => seed.toString(16).padStart(2, '0').repeat(32)
const rpackMap = readFileSync('src/ts/rpack/rpack_map.bin')

afterEach(() => { vi.restoreAllMocks() })

// The RPack codec loads its map over fetch, and `exportModule` fetches the fallback portrait; both are files the app
// serves.
function serveAppFiles() {
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
        const url = String(input)
        if (url.endsWith('rpack_map.bin')) return new Response(rpackMap)
        if (url === '/none.webp') return new Response(png)
        throw new Error(`unexpected fetch ${url}`)
    })
}

const lifecycle: PreparedNativeContentActivationLifecycle = {
    async prepareOwnerManifestAndSeal(bytes) {
        return { contentHash: await ownerManifestIdentity(bytes), byteSize: bytes.byteLength, physicalKey: 'oracle', deduplicated: false }
    },
    async sealPreparedContent() {},
    async abortPreparedContent() {},
}

// The prepared content a native import job hands TS, around the case's metadata and stored keys.
function prepared(entry: ModuleCase): PreparedNativeContent {
    const descriptor = (logicalId: string, index: number) => ({
        logicalId, objectHash: hash(index + 1), byteSize: 1, mime: 'image/png', name: `asset-${index}`, ext: 'png',
    })
    if (entry.kind === 'risum') {
        // A logical ID names its content hash, so frames that share one share the payload.
        const assets = entry.logicalIds.map((logicalId, position) => ({ ...descriptor(logicalId, entry.logicalIds.indexOf(logicalId)), position }))
        return {
            casSessionId: 'oracle', format: 'risu-module', metadata: structuredClone(entry.metadata) as Record<string, unknown>, assets,
            ownerHead: assets.length > 0 ? { present: true, manifestHash: hash(255), entryCount: assets.length } : { present: false, manifestHash: null, entryCount: 0 },
        }
    }
    if (entry.kind !== 'charx') throw new Error(`no prepared content for ${entry.kind}`)
    // One descriptor per logical ID, as the job stages each payload once.
    const logicalIds = [...new Set((entry.assets ?? []).map(asset => asset.logicalId))]
    return {
        casSessionId: 'oracle', format: 'charx-card', metadata: structuredClone(entry.card) as Record<string, unknown>,
        assets: (entry.assets ?? []).map(({ token, logicalId }) => ({ ...descriptor(logicalId, logicalIds.indexOf(logicalId)), token, referenceKey: token })),
        ...(entry.portraitLogicalId === undefined ? {} : { portraitLogicalId: entry.portraitLogicalId }),
        ...(entry.module === undefined ? {} : { module: structuredClone(entry.module) as never }),
    }
}

// What the activation appends: the module, or the error it throws.
async function activate(entry: ModuleCase): Promise<unknown> {
    let appended: unknown
    const result = await activatePreparedNativeModuleContent(prepared(entry), lifecycle, {
        confirmLowLevelAccess: async () => { dialogs.confirms += 1; return true },
        createId: () => `00000000-0000-4000-8000-${String(++ids.next).padStart(12, '0')}`,
        append: async (input) => { appended = input.module },
    })
    if (!result) return { ok: false }
    return { ok: JSON.stringify(appended), confirms: dialogs.confirms }
}

// `importModuleData` pushes the module into the database or reports why it did not.
async function importJson(entry: Extract<ModuleCase, { kind: 'json' }>): Promise<unknown> {
    DBState.db = { modules: [] } as unknown as typeof DBState.db
    await importModuleData({ name: 'synthetic.json', data: new TextEncoder().encode(JSON.stringify(entry.data)) })
    const modules = DBState.db.modules as unknown[]
    if (modules.length > 1) throw new Error(`${modules.length} modules pushed`)
    if (modules.length === 1) return { ok: JSON.stringify(modules[0]), confirms: dialogs.confirms }
    const alert = dialogs.alerts.at(-1)
    if (!alert || dialogs.alerts.length !== 1) throw new Error(`${dialogs.alerts.length} alerts without a module`)
    const value = alert.value === language.errors.noData ? 'noData' : message(alert.value)
    return { alert: alert.kind, value }
}

// The metadata of a RISUM file: magic 111, version 0, a little-endian length and the RPack-encoded JSON.
async function risumMetadata(bytes: Uint8Array): Promise<unknown> {
    if (bytes[0] !== 111 || bytes[1] !== 0) throw new Error('not a RISUM file')
    const length = new DataView(bytes.buffer, bytes.byteOffset).getUint32(2, true)
    return JSON.parse(new TextDecoder().decode(await decodeRPack(bytes.subarray(6, 6 + length))))
}

// The CharX file `exportModule` writes, as the card and the `module.risum` metadata. Asset URIs are where the container
// put each payload, so they are blanked.
async function exportCharx(entry: Extract<ModuleCase, { kind: 'module-charx' }>): Promise<unknown> {
    vi.spyOn(Date, 'now').mockReturnValue(entry.now)
    files.saved = entry.noneImage
    files.writers.length = 0
    // Every stored image the export reads holds a 1x1 PNG.
    const module = entry.module as { icon?: unknown; assets?: unknown[][] }
    const stored = [entry.noneImage, module.icon, ...(module.assets ?? []).map(asset => asset?.[1])]
    for (const key of stored) if (typeof key === 'string' && key && !/^https?:/.test(key)) await forageStorage.setItem(key, png)
    await exportModule(structuredClone(entry.module) as RisuModule, { alertEnd: false })
    const errors = dialogs.alerts.filter(alert => alert.kind === 'error')
    if (errors.length > 0) throw errors[0].value
    if (files.writers.length !== 1) throw new Error(`${files.writers.length} writers`)
    const archive = unzipSync(files.writers[0].buf.buffer)
    const card = JSON.parse(new TextDecoder().decode(archive['card.json'])) as { data: { assets?: { uri: string }[] } }
    for (const asset of card.data.assets ?? []) asset.uri = ''
    return { card, risum: await risumMetadata(archive['module.risum']) }
}

async function run(entry: ModuleCase): Promise<unknown> {
    ids.next = 0
    dialogs.confirms = 0
    dialogs.alerts.length = 0
    DBState.db = structuredClone(entry.root ?? {}) as typeof DBState.db
    try {
        switch (entry.kind) {
            case 'json': return await importJson(entry)
            case 'risum':
            case 'charx': return await activate(entry)
            case 'to-character': return { ok: JSON.stringify(convertModuleToCharacter(structuredClone(entry.module) as RisuModule)) }
            case 'risum-export': {
                const bytes = await exportModuleLegacy(structuredClone(entry.module) as RisuModule, { alertEnd: false, saveData: false })
                return { ok: JSON.stringify(await risumMetadata(bytes)) }
            }
            case 'module-charx': return { ok: JSON.stringify(await exportCharx(entry)) }
        }
    } catch (error) {
        return { error: message(error) }
    }
}

it('maps the module cases', async () => {
    serveAppFiles()
    const results: unknown[] = []
    for (const entry of set.cases) results.push(await run(entry))
    const outcomes = results as { ok?: unknown; error?: string; alert?: string; confirms?: number }[]
    expect(outcomes.filter(result => typeof result.ok === 'string').length).toBeGreaterThan(60)
    expect(outcomes.some(result => 'error' in result)).toBe(true)
    expect(outcomes.some(result => 'alert' in result)).toBe(true)
    // Every export case without a decline produced a file.
    set.cases.forEach((entry, index) => {
        if ((entry.kind === 'module-charx' || entry.kind === 'risum-export') && !entry.decline) expect(typeof outcomes[index].ok, entry.id).toBe('string')
    })
    // A dialog only appears where the Rust port declines, so it never runs a case TS would ask about.
    set.cases.forEach((entry, index) => {
        if ((outcomes[index].confirms ?? 0) > 0) expect(entry.decline, entry.id).toBe('lowLevelAccess')
    })
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
