import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it, vi } from 'vitest'
vi.mock('../characterCards', () => ({
    decodePreparedNativePngCharacterCard: vi.fn(),
    mapPreparedNativeCharacterCard: vi.fn(),
}))
vi.mock('../characters', () => ({ createBlankChar: vi.fn() }))
vi.mock('../alert', () => ({ alertConfirm: vi.fn() }))
vi.mock('./persistentDataRuntime.svelte', () => ({
    upsertPersistentCompleteCharacter: vi.fn(),
    appendPersistentRootModule: vi.fn(),
}))
import { decodePreparedNativePngCharacterCard, mapPreparedNativeCharacterCard } from '../characterCards'
import { decryptBuffer } from '../bufferCrypto'
import { upsertPersistentCompleteCharacter } from './persistentDataRuntime.svelte'
import { activatePreparedNativeModuleContent, type PreparedRootModuleAppend } from './nativeModuleContentActivation'
import { decodePreparedNativePngCardMetadata } from './nativePngCardAdapter'
import {
    decodeOwnerManifest,
    ownerManifestIdentity,
} from './ownerManifestCodec'
import { canonicalClone } from './saveCoordinatorHelpers'
import type { character } from './database.svelte'
import type {
    PreparedCardContentAssetDescriptor,
    PreparedNativeCharacterMapping,
    PreparedNativeContent,
    PreparedNativeModuleMapping,
} from './nativeFileJobs'

it('converts CharX to a module with its owner manifest, without committing an intermediate character', async () => {
    const hash = 'ab'.repeat(32)
    const key = `assets/${hash}.png`
    const character = {
        chaId: 'temporary',
        name: 'synthetic',
        additionalAssets: [
            ['first', key, 'png'],
            ['second', key, 'png'],
        ],
        globalLore: [{ content: 'synthetic lore' }],
    } as character
    vi.mocked(mapPreparedNativeCharacterCard).mockResolvedValue(character)
    const content: PreparedNativeContent = {
        casSessionId: 'job',
        format: 'charx-card',
        metadata: {
            spec: 'chara_card_v3',
            spec_version: '3.0',
            data: { extensions: {} },
        },
        assets: [
            {
                token: 'asset',
                referenceKey: 'asset',
                logicalId: key,
                objectHash: hash,
                byteSize: 4,
                ext: 'png',
                mime: 'image/png',
                name: 'first',
            },
        ],
    }
    let manifest!: Uint8Array
    const append = vi.fn()
    const result = await activatePreparedNativeModuleContent(
        content,
        {
            prepareOwnerManifestAndSeal: async (bytes) => {
                manifest = bytes
                return {
                    contentHash: await ownerManifestIdentity(bytes),
                    byteSize: bytes.length,
                    physicalKey: 'synthetic',
                    deduplicated: false,
                }
            },
        },
        { createId: () => 'module-id', confirmLowLevelAccess: vi.fn(), append },
    )
    expect(result).toEqual({ moduleId: 'module-id' })
    expect(upsertPersistentCompleteCharacter).not.toHaveBeenCalled()
    expect(append).toHaveBeenCalledOnce()
    const input = append.mock.calls[0][0]
    expect(input.module.assets).toEqual(character.additionalAssets)
    expect(input.module.lorebook).toEqual(character.globalLore)
    expect(input.assetAliases).toHaveLength(1)
    expect(input.ownerHead).toEqual({
        present: true,
        entryCount: 2,
        manifestHash: await ownerManifestIdentity(manifest),
    })
    expect(decodeOwnerManifest(manifest)).toHaveLength(2)
})

// The cards imported as modules in the import wiring oracle, with what the TS activation appended for each, recorded by
// benchmarks/rust-core-oracles/formats-wiring.oracle.test.ts. `native` and `nativeModule` are the character and module
// mappings the Rust receipt carries.
type Decline = { reason: 'unsupported' | 'invalid'; name: string }
type WiringCase = {
    id: string
    receipt: PreparedNativeContent
    destination?: 'module'
    native: 'mapped' | Decline
    nativeModule?: 'mapped' | Decline
    result: {
        mapped?: string; module?: string; stored?: string; manifest?: string; aliases?: unknown; ownerHead?: unknown
        staged?: PreparedCardContentAssetDescriptor[]; error?: string
    }
}
const fixture = (name: string) => JSON.parse(readFileSync(resolve(process.cwd(), `tests/fixtures/rust-core/formats/${name}.json`), 'utf8'))
const wiringResults = new Map((fixture('wiring.expected').cases as { id: string; result: WiringCase['result'] }[]).map(({ id, result }) => [id, result]))
const wiringCases = (fixture('wiring.cases').cases as Omit<WiringCase, 'result'>[])
    .filter((entry) => entry.destination === 'module')
    .map((entry) => ({ ...entry, result: wiringResults.get(entry.id)! }))
const hex = (bytes: Uint8Array) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')

// The module the append keeps with its aliases and owner head, and the owner manifest, as the oracle records them. The
// TS mapping answers what the oracle recorded for it, the PNG decode is the production one, and a staged inline payload
// becomes the asset the oracle staged. The module id is the oracle's last draw.
async function activateCardModule(
    entry: WiringCase,
    nativeMapping: PreparedNativeCharacterMapping,
    nativeModuleMapping: PreparedNativeModuleMapping,
    map: () => Promise<character>,
) {
    vi.mocked(mapPreparedNativeCharacterCard).mockReset().mockImplementation(map)
    vi.mocked(decodePreparedNativePngCharacterCard).mockReset().mockImplementation(async (metadata, stageInlineAsset) =>
        await decodePreparedNativePngCardMetadata(metadata, {
            stageInlineAsset,
            hash: async (bytes) => createHash('sha256').update(bytes).digest('hex'),
            decrypt: (bytes, password) => decryptBuffer(bytes, password),
            requestPassword: async () => null,
        }) as Awaited<ReturnType<typeof decodePreparedNativePngCharacterCard>>)
    const receipt: PreparedNativeContent = { ...structuredClone(entry.receipt), nativeMapping, nativeModuleMapping }
    const staged = structuredClone(entry.result.staged ?? [])
    const kept: Pick<WiringCase['result'], 'stored' | 'manifest' | 'aliases' | 'ownerHead'> = {}
    const dependencies = {
        confirmLowLevelAccess: vi.fn(),
        createId: vi.fn(() => JSON.parse(entry.result.module!).id as string),
        append: vi.fn(async (input: PreparedRootModuleAppend) => {
            kept.stored = JSON.stringify(canonicalClone(input.module))
            kept.aliases = canonicalClone(input.assetAliases)
            kept.ownerHead = canonicalClone(input.ownerHead)
        }),
    }
    const result = await activatePreparedNativeModuleContent(receipt, {
        prepareOwnerManifestAndSeal: async (bytes) => {
            kept.manifest = hex(bytes)
            return { contentHash: await ownerManifestIdentity(bytes), byteSize: bytes.byteLength, physicalKey: 'synthetic', deduplicated: false }
        },
        stageInlineAsset: async () => {
            const asset = staged.shift()
            if (!asset) throw new Error('the oracle staged no further asset')
            receipt.assets.push(asset)
            return asset
        },
    }, dependencies)
    return { result, kept, dependencies }
}

describe('native card module mapping', () => {
    const named = <Entry extends WiringCase>(entries: Entry[]) => entries.map((entry) => [entry.id, entry] as const)
    const decline = (native: Decline) => ({ outcome: 'declined', ...native }) as const
    const nativeCharacter = (entry: WiringCase) => ({ outcome: 'mapped', character: JSON.parse(entry.result.mapped!) }) as const
    const nativeModule = (entry: WiringCase) => ({ outcome: 'mapped', module: JSON.parse(entry.result.module!) }) as const
    const converted = wiringCases.filter((entry) => entry.native === 'mapped' && entry.nativeModule === 'mapped')
    const conversionDeclined = wiringCases.flatMap((entry) =>
        entry.native === 'mapped' && entry.nativeModule !== 'mapped' ? [{ ...entry, nativeModule: entry.nativeModule! }] : [])
    const declined = wiringCases.flatMap((entry) => entry.native === 'mapped' ? [] : [{ ...entry, native: entry.native }])
    const declinedAndStored = declined.filter((entry) => entry.result.stored !== undefined)
    const declinedAndFailed = declined.filter((entry) => entry.result.error !== undefined)
    const tsMapping = (entry: WiringCase) => async () => {
        if (entry.result.error !== undefined) throw new Error(entry.result.error)
        return JSON.parse(entry.result.mapped!) as character
    }
    const tsMappingRuns = async (): Promise<never> => {
        throw new Error('the TS mapping ran')
    }
    const recorded = ({ result }: WiringCase) => ({
        stored: result.stored, manifest: result.manifest, aliases: result.aliases, ownerHead: result.ownerHead,
    })

    it('replays every card wiring receipt imported as a module', () => {
        expect([converted.length, conversionDeclined.length, declinedAndStored.length, declinedAndFailed.length]).toEqual([8, 1, 3, 1])
    })

    // With the Rust receipt test, which proves both native mappings are the texts the TS mapping and conversion
    // stringify to, this is the same module through both paths.
    it.each(named(converted))('appends the native module of %s exactly as the TS conversion appends its own', async (_id, entry) => {
        const { result, kept, dependencies } = await activateCardModule(entry, nativeCharacter(entry), nativeModule(entry), tsMappingRuns)

        expect(result).toEqual({ moduleId: JSON.parse(entry.result.module!).id })
        expect(kept).toEqual(recorded(entry))
        expect(dependencies.createId).not.toHaveBeenCalled()
    })

    it.each(named(conversionDeclined))('converts the native character of %s in TS where the module mapping declines', async (_id, entry) => {
        const { kept, dependencies } = await activateCardModule(entry, nativeCharacter(entry), decline(entry.nativeModule), tsMappingRuns)

        expect(kept).toEqual(recorded(entry))
        expect(dependencies.createId).toHaveBeenCalledOnce()
    })

    it.each([
        ['limit', 'cardBytes'],
        ['cancelled', ''],
        ['host', 'stack'],
        ['failed', 'module'],
    ] as const)('converts the native character in TS where the module mapping declines as %s %s', async (reason, name) => {
        const entry = converted[0]
        const { kept, dependencies } = await activateCardModule(entry, nativeCharacter(entry), { outcome: 'declined', reason, name }, tsMappingRuns)

        expect(kept).toEqual(recorded(entry))
        expect(dependencies.createId).toHaveBeenCalledOnce()
    })

    it.each(named(declinedAndStored))('maps and converts %s in TS where the native mapping declines it', async (_id, entry) => {
        const { kept, dependencies } = await activateCardModule(entry, decline(entry.native), decline(entry.nativeModule as Decline), tsMapping(entry))

        expect(kept).toEqual(recorded(entry))
        expect(dependencies.createId).toHaveBeenCalledOnce()
    })

    it.each(named(declinedAndFailed))('rejects %s with the exact TS error where the native mapping declines it', async (_id, entry) => {
        await expect(activateCardModule(entry, decline(entry.native), decline(entry.nativeModule as Decline), tsMapping(entry)))
            .rejects.toHaveProperty('message', entry.result.error)
    })

    // The module was converted from the native character, which the activation does not use for a PNG card whose
    // decode staged an inline payload.
    it('converts in TS the character TS mapped for a PNG card whose decode staged an inline payload', async () => {
        const entry = wiringCases.find(({ id }) => id === 'module-png-inline-data-uri')!
        const { kept, dependencies } = await activateCardModule(entry, nativeCharacter(converted[0]), nativeModule(converted[0]), tsMapping(entry))

        expect(kept).toEqual(recorded(entry))
        expect(dependencies.createId).toHaveBeenCalledOnce()
    })
})
