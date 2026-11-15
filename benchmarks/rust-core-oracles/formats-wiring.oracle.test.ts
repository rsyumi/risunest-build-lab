import { createHash } from 'node:crypto'
import { expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
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
// The low-level access confirmation is the UI boundary: it answers yes and is counted.
const dialogs = vi.hoisted(() => ({ confirms: 0 }))
vi.mock('../../src/ts/alert', async (original) => ({
    ...(await original<typeof import('../../src/ts/alert')>()),
    alertConfirm: async () => { dialogs.confirms += 1; return true },
}))
// The module activation maps a card through the production mapping, which is watched for the character it returns and
// the IDs it draws.
const seen = vi.hoisted(() => ({ character: undefined as string | false | undefined, draws: 0 }))
vi.mock('../../src/ts/characterCards', async (original) => {
    const actual = await original<typeof import('../../src/ts/characterCards')>()
    return {
        ...actual,
        mapPreparedNativeCharacterCard: async (...input: Parameters<typeof actual.mapPreparedNativeCharacterCard>) => {
            const character = await actual.mapPreparedNativeCharacterCard(...input)
            seen.character = character === false ? false : JSON.stringify(character)
            seen.draws = ids.next
            return character
        },
    }
})

import { v4 } from 'uuid'
import { decodePreparedNativePngCharacterCard, mapPreparedNativeCharacterCard } from '../../src/ts/characterCards'
import { activatePreparedNativeCharacterContent } from '../../src/ts/storage/nativeCharacterContentActivation'
import { activatePreparedNativeModuleContent } from '../../src/ts/storage/nativeModuleContentActivation'
import type {
    PreparedCardContentAssetDescriptor,
    PreparedNativeCharacterMapping,
    PreparedNativeContent,
    PreparedNativeContentActivationLifecycle,
    PreparedNativeModuleMapping,
} from '../../src/ts/storage/nativeFileJobs'
import { ownerManifestIdentity } from '../../src/ts/storage/ownerManifestCodec'
import { canonicalClone } from '../../src/ts/storage/saveCoordinatorHelpers'

type WiringCase = OracleCase & {
    receipt: unknown
    destination?: 'module'
    native: 'mapped' | { reason: string; name: string }
    nativeModule?: 'mapped' | { reason: string; name: string }
    ts?: 'stored' | 'error'
}

// The case file comes from formats-wiring.cases.mjs and drives the real activation with the TS mapping.
const settle = {
    generator: import.meta.url,
    tsSources: [
        'src/ts/storage/nativeCharacterContentActivation.ts', 'src/ts/characterCards.ts', 'src/ts/storage/nativePngCardAdapter.ts',
        'src/ts/storage/nativeModuleContentActivation.ts', 'src/ts/storage/saveCoordinatorHelpers.ts', 'src/ts/interchangeability.ts',
    ],
}
const set = readOracleCases<WiringCase>('formats', 'wiring')
const message = (error: unknown) => error instanceof Error ? error.message : String(error)
const hex = (bytes: Uint8Array) => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')
const isModule = (entry: WiringCase) => (entry.receipt as PreparedNativeContent).format === 'risu-module'
const toModule = (entry: WiringCase) => isModule(entry) || entry.destination === 'module'

// `mapped` is the TS mapping as `JSON.stringify` writes it, which the Rust receipt must carry verbatim: the character,
// or the module the RISUM activation appends, recorded only where the TS steps ran; `module` is the module converted
// from a card imported as one, recorded only where TS converted it. `stored` is the record the activation keeps
// (`canonicalClone` of the mapped character or module), `manifest` the additional asset owner manifest a card
// activation seals, and `aliases` and `ownerHead` what a module activation appends beside the module. A PNG inline
// payload is staged as the native job stages it, a new asset under a fresh token added to the receipt's assets;
// `staged` lists them. `nativeMapping` and `nativeModuleMapping` are added to the receipt as the Rust receipt carries
// them, and `draws` starts the ID counter where the TS mapping a native character replaces would have left it.
type Outcome = {
    mapped?: string | false
    module?: string
    stored?: string
    manifest?: string
    staged?: PreparedCardContentAssetDescriptor[]
    aliases?: unknown
    ownerHead?: unknown
    error?: string
    confirms: number
}
async function run(
    entry: WiringCase,
    nativeMapping?: PreparedNativeCharacterMapping | PreparedNativeModuleMapping,
    nativeModuleMapping?: PreparedNativeModuleMapping,
    draws = 0,
): Promise<Outcome> {
    ids.next = draws
    dialogs.confirms = 0
    seen.character = undefined
    seen.draws = 0
    const recorded: Omit<Outcome, 'error' | 'confirms'> = {}
    const receipt = {
        ...structuredClone(entry.receipt) as PreparedNativeContent,
        ...(nativeMapping === undefined ? {} : { nativeMapping }),
        ...(nativeModuleMapping === undefined ? {} : { nativeModuleMapping }),
    }
    const lifecycle: PreparedNativeContentActivationLifecycle = {
        prepareOwnerManifestAndSeal: async (bytes) => {
            recorded.manifest = hex(bytes)
            return { contentHash: await ownerManifestIdentity(bytes), byteSize: bytes.byteLength, physicalKey: 'oracle', deduplicated: false }
        },
        stageInlineAsset: async (bytes, name) => {
            const objectHash = createHash('sha256').update(bytes).digest('hex')
            const ext = name === '' ? 'png' : name.split('.').pop()!
            recorded.staged ??= []
            const token = `native-inline-00000000-0000-4000-8000-${String(recorded.staged.length + 1).padStart(12, '0')}`
            const asset: PreparedCardContentAssetDescriptor = {
                referenceKey: token, token, logicalId: `assets/${objectHash}.${ext}`, objectHash, byteSize: bytes.byteLength,
                mime: '', name: name === '' ? `${objectHash}.${ext}` : name, ext,
            }
            recorded.staged.push(asset)
            receipt.assets.push(asset)
            return structuredClone(asset)
        },
        sealPreparedContent: async () => {},
        abortPreparedContent: async () => {},
    }
    try {
        if (toModule(entry)) {
            // The TS steps draw the module id, so a module appended without a draw is the native one.
            let drawn = false
            await activatePreparedNativeModuleContent(receipt, lifecycle, {
                confirmLowLevelAccess: async () => { dialogs.confirms += 1; return true },
                createId: () => { drawn = true; return v4() },
                append: async (input) => {
                    if (drawn) recorded[isModule(entry) ? 'mapped' : 'module'] = JSON.stringify(input.module)
                    recorded.stored = JSON.stringify(canonicalClone(input.module))
                    recorded.aliases = canonicalClone(input.assetAliases)
                    recorded.ownerHead = canonicalClone(input.ownerHead)
                },
            })
            if (!isModule(entry) && seen.character !== undefined) recorded.mapped = seen.character
        } else {
            await activatePreparedNativeCharacterContent(receipt, lifecycle, {
                decodePng: decodePreparedNativePngCharacterCard,
                map: async (input) => {
                    const character = await mapPreparedNativeCharacterCard(input)
                    recorded.mapped = character === false ? false : JSON.stringify(character)
                    return character
                },
                upsert: async (_id, _reason, createOrMutate) => {
                    recorded.stored = JSON.stringify(canonicalClone(await createOrMutate(undefined)))
                    return true
                },
            })
        }
        return { ...recorded, confirms: dialogs.confirms }
    } catch (error) {
        return { error: message(error), confirms: dialogs.confirms }
    }
}
const decline = (native: Exclude<WiringCase['native'], 'mapped'>) => ({ outcome: 'declined', ...native }) as PreparedNativeModuleMapping

it('activates the wiring receipts through the TS mapping', async () => {
    const outcomes: Outcome[] = []
    for (const entry of set.cases) outcomes.push(await run(entry))
    set.cases.forEach((entry, index) => {
        const outcome = outcomes[index]
        // A receipt the Rust port maps was stored by the TS path, and so was one it declines, unless the content is invalid.
        const ts = entry.ts ?? (entry.native === 'mapped' || entry.native.reason !== 'invalid' ? 'stored' : 'error')
        if (entry.native === 'mapped') expect(ts, entry.id).toBe('stored')
        if (entry.destination === 'module') {
            expect(entry.nativeModule, entry.id).toBeDefined()
            if (entry.native !== 'mapped') expect(entry.nativeModule, entry.id).toEqual(entry.native)
        }
        if (ts === 'stored') {
            expect(typeof outcome.mapped, entry.id).toBe('string')
            expect(typeof outcome.stored, entry.id).toBe('string')
            if (isModule(entry)) expect(outcome.ownerHead, entry.id).toEqual((entry.receipt as PreparedNativeContent).ownerHead)
            else expect(typeof outcome.manifest, entry.id).toBe('string')
            if (entry.destination === 'module') expect(JSON.parse(outcome.module!).id, entry.id).toBe(JSON.parse(outcome.stored!).id)
        } else {
            expect(outcome, entry.id).toHaveProperty('error')
        }
        // A dialog only appears where the Rust port declines.
        if (outcome.confirms > 0) expect(entry.native, entry.id).toEqual({ reason: 'unsupported', name: 'lowLevelAccess' })
    })
    expect(settleOracle(set, outcomes, settle)).toMatch(/written|matched/)
})

// With the mappings the Rust receipt carries (the TS character or module where it maps, which the app's replay proves
// it to be, or its decline), the activation stores the same record or throws the same error as without them.
it('ends each wiring receipt with its native mapping where the TS mapping alone ends it', async () => {
    for (const entry of set.cases) {
        const alone = await run(entry)
        const draws = seen.draws
        const parsed = (text: string | false | undefined) => JSON.parse(text as string) as Record<string, unknown>
        const nativeMapping: PreparedNativeCharacterMapping | PreparedNativeModuleMapping = entry.native === 'mapped'
            ? isModule(entry) ? { outcome: 'mapped', module: parsed(alone.mapped) } : { outcome: 'mapped', character: parsed(alone.mapped) }
            : decline(entry.native)
        const nativeModuleMapping = entry.nativeModule === undefined ? undefined
            : entry.nativeModule === 'mapped' ? { outcome: 'mapped', module: parsed(alone.module) } as const : decline(entry.nativeModule)
        const composed = await run(entry, nativeMapping, nativeModuleMapping, entry.native === 'mapped' ? draws : 0)
        const end = ({ stored, manifest, staged, aliases, ownerHead, error }: Outcome) => ({ stored, manifest, staged, aliases, ownerHead, error })
        expect(end(composed), entry.id).toEqual(end(alone))
        // Only a decline runs the TS mapping, and with it the confirmation; only a module decline runs the conversion.
        expect(composed.mapped, entry.id).toEqual(entry.native === 'mapped' ? undefined : alone.mapped)
        const converted = entry.native === 'mapped' && entry.nativeModule === 'mapped'
        expect(composed.module, entry.id).toEqual(converted ? undefined : alone.module)
        expect(composed.confirms, entry.id).toBe(entry.native === 'mapped' ? 0 : alone.confirms)
    }
})
