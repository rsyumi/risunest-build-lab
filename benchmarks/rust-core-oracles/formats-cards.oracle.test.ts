import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { Buffer } from 'buffer'
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
// The dialogs are the UI boundary. The low-level access confirmation answers yes and is counted, the export menu picks
// CharX, and an export error `exportCharacterCard` reports instead of throwing is kept.
const dialogs = vi.hoisted(() => ({ confirms: 0, errors: [] as unknown[] }))
vi.mock('../../src/ts/alert', async (original) => ({
    ...(await original<typeof import('../../src/ts/alert')>()),
    alertConfirm: async () => { dialogs.confirms += 1; return true },
    alertCardExport: async () => ({ type: '', type2: 'charx' }),
    alertError: (error: unknown) => { dialogs.errors.push(error) },
    alertNormal: () => {},
}))
// The native CharX route is the Tauri boundary: it receives the projection closure and answers a cancelled picker.
type CharxProjector = (character: unknown) => { card: unknown; module: unknown }
const routes = vi.hoisted(() => ({ projectCharacter: undefined as undefined | CharxProjector }))
vi.mock('../../src/ts/storage/nativeCharacterCharxExportRoute', () => ({
    exportNativeCharacterCharxFromPicker: async (input: { projectCharacter: CharxProjector }) => {
        routes.projectCharacter = input.projectCharacter
        return null
    },
}))
// The card export reads the portrait and asset files, each a 1x1 PNG, and writes the card file; the written bytes are
// kept.
const files = vi.hoisted(() => ({ written: [] as Uint8Array[] }))
vi.mock('../../src/ts/globalApi.svelte', async (original) => ({
    ...(await original<typeof import('../../src/ts/globalApi.svelte')>()),
    readImage: async () => Uint8Array.from(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64')),
    downloadFile: async (_name: string, data: Uint8Array) => { files.written.push(data); return true },
}))

import {
    createBaseV3, exportChar, exportCharacterCard, mapPreparedNativeCharacterCard, type PreparedNativeCharacterCardInput,
} from '../../src/ts/characterCards'
import { decodePreparedNativePngCardMetadata } from '../../src/ts/storage/nativePngCardAdapter'
import { VirtualWriter } from '../../src/ts/globalApi.svelte'
import { PngChunk } from '../../src/ts/pngChunk'
import { DBState } from '../../src/ts/stores.svelte'

type Prepared = {
    card: unknown
    assets?: { token: string; logicalId: string }[]
    portraitLogicalId?: string
    module?: unknown
}
type Chain = { via: 'png-v3' | 'png-v2' | 'charx'; character: unknown; now: number; portraitLogicalId?: string; rejected?: boolean }
type CardCase = OracleCase & { decline?: string } & (
    | ({ kind: 'import' } & Prepared)
    | { kind: 'export' | 'charx'; character: unknown; now: number }
    | { kind: 'export-v2'; character: unknown }
    | ({ kind: 'roundtrip'; now: number } & Prepared)
    | ({ kind: 'chain' } & Chain)
)
type Character = Parameters<typeof createBaseV3>[0]
type Card = { data: { assets?: { uri: string }[] } }

// The case file comes from formats-cards.cases.mjs and drives the real mapping functions.
const settle = {
    generator: import.meta.url,
    tsSources: ['src/ts/characterCards.ts', 'src/ts/onboarding.ts', 'src/ts/storage/database.svelte.ts', 'src/ts/storage/nativePngCardAdapter.ts'],
}
const set = readOracleCases<CardCase>('formats', 'cards')
const message = (error: unknown) => error instanceof Error ? error.message : String(error)
// RCC and inline payloads never occur in the exported cards the chains decode.
const pngDecoder = {
    hash: async (): Promise<string> => { throw new Error('unexpected RCC card') },
    decrypt: async (): Promise<Uint8Array> => { throw new Error('unexpected RCC card') },
    requestPassword: async () => null,
}

afterEach(() => { vi.restoreAllMocks() })

function input(entry: Prepared): PreparedNativeCharacterCardInput {
    return {
        card: structuredClone(entry.card),
        assets: entry.assets ?? [],
        ...(entry.portraitLogicalId === undefined ? {} : { portraitLogicalId: entry.portraitLogicalId }),
        ...('module' in entry ? { module: structuredClone(entry.module) } : {}),
    } as PreparedNativeCharacterCardInput
}

// `exportChar` with the CharX choice hands its projection closure to the native route.
async function charxProjector(): Promise<CharxProjector> {
    const before = DBState.db
    DBState.db = { characters: [{ chaId: 'oracle-charx', name: 'Oracle' }] } as unknown as typeof DBState.db
    try { await exportChar(0) } finally { DBState.db = before }
    if (!routes.projectCharacter) throw new Error('exportChar did not take the native CharX route')
    return routes.projectCharacter
}

// `createBaseV2` runs inside `exportCharacterCard`, whose JSON branch writes the card with `JSON.stringify(card, null, 4)`.
async function exportV2(character: unknown): Promise<unknown> {
    files.written.length = 0
    dialogs.errors.length = 0
    // A missing portrait would make the export fetch the fallback image; `createBaseV2` never reads it.
    const stored = character as { image?: unknown }
    if (!stored.image) stored.image = 'oracle-portrait'
    await exportCharacterCard(character as Character, 'json', { spec: 'v2' })
    if (dialogs.errors.length > 0) throw dialogs.errors[0]
    if (files.written.length !== 1) throw new Error(`the export wrote ${files.written.length} files`)
    return JSON.parse(new TextDecoder().decode(files.written[0]))
}

// The text chunks of the PNG file `exportCharacterCard` writes for a CCv3 card.
async function exportPngV3(character: unknown): Promise<Record<string, string>> {
    dialogs.errors.length = 0
    // A missing portrait would make the export fetch the fallback image; the card does not carry it.
    const stored = character as { image?: unknown }
    if (!stored.image) stored.image = 'oracle-portrait'
    const writer = new VirtualWriter()
    await exportCharacterCard(character as Character, 'png', { spec: 'v3', writer })
    if (dialogs.errors.length > 0) throw dialogs.errors[0]
    const chunks: Record<string, string> = {}
    for await (const chunk of PngChunk.readGenerator(writer.buf.buffer)) {
        if (chunk && 'key' in chunk) chunks[chunk.key] = chunk.value
    }
    return chunks
}

// The asset references the CharX container hands the import: it embeds every asset but remote ones, and its import
// names each `__asset:native-charx-<index>`.
function stageCharxAssets(card: Card) {
    const staged: { token: string; logicalId: string }[] = []
    if (!Array.isArray(card.data.assets)) return staged
    card.data.assets.forEach((asset, index) => {
        if (asset.uri.startsWith('http://') || asset.uri.startsWith('https://')) return
        const [token, logicalId] = [`native-charx-${index}`, `charx-asset-${index}`]
        asset.uri = `__asset:${token}`
        staged.push({ token, logicalId })
    })
    return staged
}

async function chain(entry: Chain, projectCharx: CharxProjector): Promise<unknown> {
    vi.spyOn(Date, 'now').mockReturnValue(entry.now)
    const character = structuredClone(entry.character)
    if (entry.via === 'png-v3') {
        // The native PNG import stages each `chara-ext-asset_:<n>` chunk under the token `<n>`.
        const chunks = await exportPngV3(character)
        const assets = Object.keys(chunks).filter(key => key.startsWith('chara-ext-asset_:'))
            .map(key => key.slice('chara-ext-asset_:'.length)).map(token => ({ token, logicalId: `png-asset-${token}` }))
        const decoded = await decodePreparedNativePngCardMetadata({ ccv3: chunks.ccv3 }, pngDecoder)
        return await mapPreparedNativeCharacterCard({ card: decoded, assets, portraitLogicalId: entry.portraitLogicalId } as unknown as PreparedNativeCharacterCardInput)
    }
    let exported: unknown
    let module: unknown
    if (entry.via === 'charx') ({ card: exported, module } = projectCharx(character))
    else exported = await exportV2(character)
    const card = JSON.parse(JSON.stringify(exported)) as Card
    const assets = entry.via === 'charx' ? stageCharxAssets(card) : []
    if (entry.via === 'charx') {
        // The CharX import reads `trigger` and `regex` (`[]` when absent) and `lorebook` from `module.risum`.
        const stored = JSON.parse(JSON.stringify(module)) as { trigger?: unknown; regex?: unknown; lorebook?: unknown }
        return await mapPreparedNativeCharacterCard({
            card, assets,
            module: { trigger: stored.trigger ?? [], regex: stored.regex ?? [], ...(stored.lorebook === undefined ? {} : { lorebook: stored.lorebook }) },
        } as unknown as PreparedNativeCharacterCardInput)
    }
    // The native PNG import decodes the card chunk before the mapping.
    const chunk = Buffer.from(JSON.stringify(card)).toString('base64')
    const decoded = await decodePreparedNativePngCardMetadata({ chara: chunk }, pngDecoder)
    return await mapPreparedNativeCharacterCard({ card: decoded, assets, portraitLogicalId: entry.portraitLogicalId } as unknown as PreparedNativeCharacterCardInput)
}

// The mapped character as `JSON.stringify` writes it: the property order and the omitted `undefined` the Rust port
// must reproduce. The store keeps `canonicalClone` of it, which this text determines.
async function run(entry: CardCase, projectCharx: CharxProjector): Promise<unknown> {
    ids.next = 0
    dialogs.confirms = 0
    try {
        switch (entry.kind) {
            case 'import': {
                const character = await mapPreparedNativeCharacterCard(input(entry))
                return { ok: character === false ? false : JSON.stringify(character), confirms: dialogs.confirms }
            }
            case 'export': {
                vi.spyOn(Date, 'now').mockReturnValue(entry.now)
                return { ok: JSON.stringify(createBaseV3(structuredClone(entry.character) as Character)) }
            }
            case 'export-v2':
                return { ok: JSON.stringify(await exportV2(structuredClone(entry.character))) }
            case 'charx': {
                vi.spyOn(Date, 'now').mockReturnValue(entry.now)
                return { ok: JSON.stringify(projectCharx(structuredClone(entry.character))) }
            }
            case 'roundtrip': {
                const character = await mapPreparedNativeCharacterCard(input(entry))
                if (character === false) return { ok: false }
                vi.spyOn(Date, 'now').mockReturnValue(entry.now)
                return { ok: JSON.stringify(createBaseV3(character)), confirms: dialogs.confirms }
            }
            case 'chain': {
                const character = await chain(entry, projectCharx)
                return { ok: character === false ? false : JSON.stringify(character), confirms: dialogs.confirms }
            }
        }
    } catch (error) {
        return { error: message(error) }
    }
}

it('maps the card cases', async () => {
    const projectCharx = await charxProjector()
    const results: unknown[] = []
    for (const entry of set.cases) results.push(await run(entry, projectCharx))
    const outcomes = results as { ok?: unknown; error?: string; confirms?: number }[]
    expect(outcomes.filter(result => typeof result.ok === 'string').length).toBeGreaterThan(70)
    expect(outcomes.some(result => 'error' in result)).toBe(true)
    // Every export and chain case without a decline produced a card or character, or the error a `rejected` chain expects.
    set.cases.forEach((entry, index) => {
        if (entry.kind === 'import' || entry.kind === 'roundtrip' || entry.decline) return
        const label = `${entry.id} ${JSON.stringify(outcomes[index]).slice(0, 300)}`
        if (entry.kind === 'chain' && entry.rejected) expect(outcomes[index], label).toHaveProperty('error')
        else expect(typeof outcomes[index].ok, label).toBe('string')
    })
    // A dialog only appears where the Rust port declines, so it never runs a case TS would ask about.
    set.cases.forEach((entry, index) => {
        if ((outcomes[index].confirms ?? 0) > 0) expect(entry.decline, entry.id).toBe('lowLevelAccess')
    })
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
