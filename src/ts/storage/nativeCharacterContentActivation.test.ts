import { Buffer } from 'buffer'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { decodePreparedNativePngCardMetadata } from './nativePngCardAdapter'
import { describe, expect, it, vi } from 'vitest'

vi.mock('../characterCards', () => ({
    decodePreparedNativePngCharacterCard: vi.fn(),
    mapPreparedNativeCharacterCard: vi.fn(),
}))
vi.mock('./nativeAssetRepository', () => ({
    createNativeImmutablePayloadCas: vi.fn(),
}))
vi.mock('./persistentDataRuntime.svelte', () => ({
    upsertPersistentCompleteCharacter: vi.fn(),
}))

import type { character } from './database.svelte'
import { decryptBuffer } from '../bufferCrypto'
import type { PreparedImmutablePayload } from './payloadCas'
import {
    activatePreparedNativeCharacterContent,
    UnsupportedPreparedNativeCharacterCardError,
    type NativeCharacterContentActivationDependencies,
} from './nativeCharacterContentActivation'
import type {
    PreparedCardContentAssetDescriptor,
    PreparedNativeCharacterMapping,
    PreparedNativeContent,
    PreparedNativeContentActivationLifecycle,
} from './nativeFileJobs'
import { decodeOwnerManifest, ownerManifestIdentity } from './ownerManifestCodec'
import { runNativePreparedContentRoute } from './nativePreparedContentRoute'
import { canonicalClone } from './saveCoordinatorHelpers'

const firstHash = '11'.repeat(32)
const secondHash = '22'.repeat(32)

const content: PreparedNativeContent = {
    casSessionId: 'content-1',
    format: 'json-card',
    metadata: {
        spec: 'chara_card_v3',
        spec_version: '3.0',
        data: { name: 'Native Card', extensions: {} },
    },
    assets: [
        {
            referenceKey: 'data.assets.0.uri',
            token: 'native-data-0',
            logicalId: `assets/${firstHash}.png`,
            objectHash: firstHash,
            byteSize: 4,
            mime: 'image/png',
            name: 'portrait',
            ext: 'png',
        },
        {
            referenceKey: 'data.assets.1.uri',
            token: 'native-data-1',
            logicalId: `assets/${secondHash}.json`,
            objectHash: secondHash,
            byteSize: 9,
            mime: 'application/json',
            name: 'config',
            ext: 'json',
        },
        {
            referenceKey: 'data.assets.2.uri',
            token: 'native-data-2',
            logicalId: `assets/${secondHash}.json`,
            objectHash: secondHash,
            byteSize: 9,
            mime: 'application/json',
            name: 'config',
            ext: 'json',
        },
    ],
}

function mappedCharacter(): character {
    return {
        type: 'character',
        chaId: 'character-1',
        name: 'Native Card',
        chats: [],
        additionalAssets: [
            ['config', `assets/${secondHash}.json`, 'json'],
            ['config duplicate', `assets/${secondHash}.json`, 'json'],
        ],
    } as character
}

function dependencies(
    overrides: Partial<NativeCharacterContentActivationDependencies> = {},
): NativeCharacterContentActivationDependencies {
    return {
        decodePng: vi.fn(async () => content.metadata as any),
        map: vi.fn(async () => mappedCharacter()),
        upsert: vi.fn(async () => true),
        ...overrides,
    }
}

function lifecycle(
    overrides: Partial<PreparedNativeContentActivationLifecycle> = {},
): PreparedNativeContentActivationLifecycle {
    return {
        prepareOwnerManifestAndSeal: vi.fn(async (bytes): Promise<PreparedImmutablePayload> => ({
            contentHash: await ownerManifestIdentity(bytes),
            byteSize: bytes.byteLength,
            physicalKey: 'unused-by-activation',
            deduplicated: false,
        })),
        ...overrides,
    }
}

function deferred<T>(): {
    promise: Promise<T>
    resolve(value?: T): void
} {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((done) => {
        resolve = done
    })
    return { promise, resolve: (value) => resolve(value as T) }
}

describe('prepared native character content activation', () => {
    it('seals the owner manifest session before the atomic character upsert', async () => {
        const events: string[] = []
        const deps = dependencies({
            upsert: vi.fn(async () => {
                events.push('upsert')
                return true
            }),
        })
        const session = lifecycle({
            prepareOwnerManifestAndSeal: vi.fn(async (bytes) => {
                events.push('finalize')
                return {
                    contentHash: await ownerManifestIdentity(bytes),
                    byteSize: bytes.byteLength,
                    physicalKey: 'unused-by-activation',
                    deduplicated: false,
                }
            }),
        })

        await activatePreparedNativeCharacterContent(content, session, deps)

        expect(events).toEqual(['finalize', 'upsert'])
    })

    it('commits ordinary aliases and the exact ordered owner manifest with the character', async () => {
        const deps = dependencies()
        const session = lifecycle()

        const result = await activatePreparedNativeCharacterContent(content, session, deps)

        expect(result).toEqual({ characterId: 'character-1' })
        expect(deps.map).toHaveBeenCalledWith({
            card: content.metadata,
            assets: content.assets.map(({ token, logicalId }) => ({ token, logicalId })),
        })
        expect(session.prepareOwnerManifestAndSeal).toHaveBeenCalledOnce()
        const manifestBytes = vi.mocked(session.prepareOwnerManifestAndSeal).mock.calls[0][0]
        expect(decodeOwnerManifest(manifestBytes)).toEqual([
            {
                tuple: ['config', `assets/${secondHash}.json`, 'json'],
                payloadHash: Uint8Array.from({ length: 32 }, () => 0x22),
            },
            {
                tuple: ['config duplicate', `assets/${secondHash}.json`, 'json'],
                payloadHash: Uint8Array.from({ length: 32 }, () => 0x22),
            },
        ])
        const expectedManifestHash = await ownerManifestIdentity(manifestBytes)
        expect(deps.upsert).toHaveBeenCalledWith(
            'character-1',
            'native-content-import',
            expect.any(Function),
            {
                assetAliases: [
                    {
                        kind: 'asset',
                        key: `assets/${firstHash}.png`,
                        objectHash: firstHash,
                        size: 4,
                        mime: 'image/png',
                        name: 'portrait',
                        ext: 'png',
                    },
                    {
                        kind: 'asset',
                        key: `assets/${secondHash}.json`,
                        objectHash: secondHash,
                        size: 9,
                        mime: 'application/json',
                        name: 'config',
                        ext: 'json',
                    },
                ],
                assetOwnerHeads: [{
                    owner: {
                        kind: 'character-additional-assets',
                        characterId: 'character-1',
                    },
                    present: true,
                    manifestHash: expectedManifestHash,
                    entryCount: 2,
                }],
            },
        )
        const create = vi.mocked(deps.upsert).mock.calls[0][2]
        expect(await create(null)).toEqual(mappedCharacter())
    })

    it('passes a CharX portrait and module overlay to the mapper while committing only ordinary asset aliases', async () => {
        const charxContent = {
            ...content,
            format: 'charx-card',
            portraitLogicalId: content.assets[0].logicalId,
            module: { trigger: [], regex: [], lorebook: [] },
        } as PreparedNativeContent
        const deps = dependencies()
        const session = lifecycle()

        await activatePreparedNativeCharacterContent(charxContent, session, deps)

        expect(deps.map).toHaveBeenCalledWith({
            card: content.metadata,
            assets: content.assets.map(({ token, logicalId }) => ({ token, logicalId })),
            portraitLogicalId: content.assets[0].logicalId,
            module: { trigger: [], regex: [], lorebook: [] },
        })
        const options = vi.mocked(deps.upsert).mock.calls[0][3]
        expect(options?.assetAliases).toEqual(expect.arrayContaining([
            expect.objectContaining({ kind: 'asset', key: content.assets[0].logicalId }),
        ]))
        expect(options?.assetAliases).not.toEqual(expect.arrayContaining([
            expect.objectContaining({ kind: 'inlay' }),
        ]))
        for (const alias of options?.assetAliases ?? []) {
            expect(Object.keys(alias).sort()).toEqual([
                'ext',
                'key',
                'kind',
                'mime',
                'name',
                'objectHash',
                'size',
            ])
        }
    })

    it('keeps an appended CharX JPEG portrait byte-exact as an ordinary asset alias', async () => {
        const portrait = {
            referenceKey: 'portrait',
            token: 'native-appended-portrait',
            logicalId: `assets/${firstHash}.jpeg`,
            objectHash: firstHash,
            byteSize: 10_485_763,
            mime: 'image/jpeg',
            name: 'original portrait',
            ext: 'jpeg',
        }
        const appendedCharxContent = {
            ...content,
            format: 'appended-charx-jpeg',
            assets: [portrait],
            portraitLogicalId: portrait.logicalId,
        } as PreparedNativeContent
        const character = mappedCharacter()
        character.additionalAssets = []
        const deps = dependencies({ map: vi.fn(async () => character) })
        const session = lifecycle()

        await activatePreparedNativeCharacterContent(appendedCharxContent, session, deps)

        expect(deps.map).toHaveBeenCalledWith({
            card: content.metadata,
            assets: [{ token: portrait.token, logicalId: portrait.logicalId }],
            portraitLogicalId: portrait.logicalId,
        })
        const options = vi.mocked(deps.upsert).mock.calls[0][3]
        expect(options?.assetAliases).toEqual([{
            kind: 'asset',
            key: portrait.logicalId,
            objectHash: portrait.objectHash,
            size: portrait.byteSize,
            mime: portrait.mime,
            name: portrait.name,
            ext: portrait.ext,
        }])
        expect(JSON.stringify(options?.assetAliases)).not.toMatch(/inlay|webp|resize/i)
    })

    it.each(['emotions', 'vits', 'v3'])('adds staged %s PNG assets before mapping and alias activation', async (kind) => {
        const inlineCard = kind === 'v3'
            ? { spec: 'chara_card_v3', spec_version: '3.0', data: { name: 'Inline', extensions: {},
                assets: [{ type: 'icon', name: 'main', uri: 'data:image/png;base64,AQI=' }] } }
            : { spec: 'chara_card_v2', spec_version: '2.0', data: { name: 'Inline', extensions: { risuai: kind === 'vits'
                ? { vits: { model: 'AQI=' } } : { emotions: [['happy', 'AQI=']] } } } }
        const prepared: PreparedNativeContent = {
            casSessionId: 'content-1', format: 'png-card',
            metadata: { chara: Buffer.from(JSON.stringify(inlineCard)).toString('base64') }, assets: [],
        }
        const staged = { token: 'inline-1', referenceKey: 'inline-1', logicalId: `assets/${firstHash}.png`,
            objectHash: firstHash, byteSize: 2, mime: '', name: `${firstHash}.png`, ext: 'png' }
        const events: string[] = []
        const stageInlineAsset = vi.fn(async (bytes: Uint8Array) => {
            expect(bytes).toEqual(Buffer.from('AQI=', 'base64'))
            events.push('stage')
            prepared.assets.push(staged)
            return staged
        })
        const mapped = { ...mappedCharacter(), additionalAssets: [] } as character
        const deps = dependencies({
            decodePng: (metadata, stage) => decodePreparedNativePngCardMetadata(metadata, {
                hash: async () => '', decrypt: async (value) => value, requestPassword: async () => null,
                stageInlineAsset: stage,
            }) as any,
            map: vi.fn(async (input) => {
                events.push('map')
                expect(input.assets).toEqual([{ token: staged.token, logicalId: staged.logicalId }])
                expect(JSON.stringify(input.card)).toContain('__asset:inline-1')
                return mapped
            }),
            upsert: vi.fn(async (_id, _reason, _change, options) => {
                events.push('activate')
                expect(options?.assetAliases).toEqual([expect.objectContaining({ objectHash: firstHash, size: 2 })])
                return true
            }),
        })
        await activatePreparedNativeCharacterContent(prepared, lifecycle({ stageInlineAsset }), deps)
        expect(events).toEqual(['stage', 'map', 'activate'])
    })

    it('decodes and activates PNG metadata using only logical portrait and chunk aliases', async () => {
        const portraitHash = '33'.repeat(32)
        const chunkHash = '44'.repeat(32)
        const decodedCard = {
            spec: 'chara_card_v2',
            spec_version: '2.0',
            data: {
                name: 'PNG Card',
                extensions: {
                    risuai: {
                        additionalAssets: [['chunk', '__asset:007', 'png']],
                    },
                },
            },
        }
        const pngContent = {
            casSessionId: 'content-1',
            format: 'png-card',
            metadata: { chara: 'encoded-v2', ccv3: 'encoded-v3' },
            portraitLogicalId: `assets/${portraitHash}.png`,
            assets: [
                {
                    referenceKey: 'native-png-portrait',
                    token: 'native-png-portrait',
                    logicalId: `assets/${portraitHash}.png`,
                    objectHash: portraitHash,
                    byteSize: 100,
                    mime: 'image/png',
                    name: `${portraitHash}.png`,
                    ext: 'png',
                },
                {
                    referenceKey: '007',
                    token: '007',
                    logicalId: `assets/${chunkHash}.png`,
                    objectHash: chunkHash,
                    byteSize: 4,
                    mime: '',
                    name: `${chunkHash}.png`,
                    ext: 'png',
                },
            ],
        } as PreparedNativeContent
        const character = mappedCharacter()
        character.image = pngContent.portraitLogicalId
        character.additionalAssets = [['chunk', pngContent.assets[1].logicalId, 'png']]
        const deps = dependencies({
            decodePng: vi.fn(async () => decodedCard as any),
            map: vi.fn(async () => character),
        })
        const session = lifecycle()

        await activatePreparedNativeCharacterContent(pngContent, session, deps)

        expect(deps.decodePng).toHaveBeenCalledWith(pngContent.metadata, undefined)
        expect(deps.map).toHaveBeenCalledWith({
            card: decodedCard,
            assets: [
                { token: 'native-png-portrait', logicalId: pngContent.assets[0].logicalId },
                { token: '007', logicalId: pngContent.assets[1].logicalId },
            ],
            portraitLogicalId: pngContent.portraitLogicalId,
        })
        const options = vi.mocked(deps.upsert).mock.calls[0][3]
        expect(options?.assetAliases).toEqual([
            expect.objectContaining({
                kind: 'asset',
                key: pngContent.assets[0].logicalId,
                mime: 'image/png',
            }),
            expect.objectContaining({
                kind: 'asset',
                key: pngContent.assets[1].logicalId,
                mime: '',
            }),
        ])
        expect(options?.assetAliases).not.toEqual(expect.arrayContaining([
            expect.objectContaining({ kind: 'inlay' }),
        ]))
        const manifestBytes = vi.mocked(session.prepareOwnerManifestAndSeal).mock.calls[0][0]
        expect(decodeOwnerManifest(manifestBytes)).toEqual([{
            tuple: ['chunk', pngContent.assets[1].logicalId, 'png'],
            payloadHash: Uint8Array.from({ length: 32 }, () => 0x44),
        }])
        expect(JSON.stringify(vi.mocked(deps.map).mock.calls)).not.toMatch(/payload|staged|path|Uint8Array/i)
    })

    it('declines a cancelled PNG password prompt before finalizing', async () => {
        const deps = dependencies({ decodePng: vi.fn(async () => null) })
        const session = lifecycle()

        await expect(activatePreparedNativeCharacterContent({
            ...content,
            format: 'png-card',
            metadata: { chara: 'rcc-envelope' },
            portraitLogicalId: content.assets[0].logicalId,
        }, session, deps)).resolves.toBeNull()

        expect(deps.map).not.toHaveBeenCalled()
        expect(session.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('deduplicates a PNG portrait and opaque chunk with the same exact bytes', async () => {
        const sharedHash = '55'.repeat(32)
        const logicalId = `assets/${sharedHash}.png`
        const pngContent = {
            ...content,
            format: 'png-card',
            metadata: { ccv3: 'encoded-card' },
            portraitLogicalId: logicalId,
            assets: [
                {
                    referenceKey: 'native-png-portrait',
                    token: 'native-png-portrait',
                    logicalId,
                    objectHash: sharedHash,
                    byteSize: 100,
                    mime: 'image/png',
                    name: `${sharedHash}.png`,
                    ext: 'png',
                },
                {
                    referenceKey: 'same',
                    token: 'same',
                    logicalId,
                    objectHash: sharedHash,
                    byteSize: 100,
                    mime: '',
                    name: `${sharedHash}.png`,
                    ext: 'png',
                },
            ],
        } as PreparedNativeContent
        const character = mappedCharacter()
        character.additionalAssets = [['same', logicalId, 'png']]
        const deps = dependencies({
            decodePng: vi.fn(async () => content.metadata as any),
            map: vi.fn(async () => character),
        })

        await activatePreparedNativeCharacterContent(pngContent, lifecycle(), deps)

        expect(vi.mocked(deps.upsert).mock.calls[0][3]?.assetAliases).toEqual([{
            kind: 'asset',
            key: logicalId,
            objectHash: sharedHash,
            size: 100,
            mime: 'image/png',
            name: `${sharedHash}.png`,
            ext: 'png',
        }])
    })

    it('rejects unsupported PNG inline payloads before finalizing', async () => {
        const deps = dependencies({
            decodePng: vi.fn(async () => {
                throw new UnsupportedPreparedNativeCharacterCardError('inline payload')
            }),
        })
        const session = lifecycle()

        await expect(activatePreparedNativeCharacterContent({
            ...content,
            format: 'png-card',
            metadata: { ccv3: 'encoded-inline-card' },
            portraitLogicalId: content.assets[0].logicalId,
        }, session, deps)).rejects.toBeInstanceOf(UnsupportedPreparedNativeCharacterCardError)

        expect(deps.map).not.toHaveBeenCalled()
        expect(session.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('cancels the retained route when abort arrives during PNG metadata decoding', async () => {
        const controller = new AbortController()
        const pending = deferred<any>()
        const decodeStarted = deferred<void>()
        const deps = dependencies({
            decodePng: vi.fn(async () => {
                decodeStarted.resolve()
                return pending.promise
            }),
        })
        const session = lifecycle()
        const receipt = {
            jobId: 'content-1',
            content: {
                ...content,
                format: 'png-card',
                metadata: { chara: 'rcc-envelope' },
                portraitLogicalId: content.assets[0].logicalId,
            } as PreparedNativeContent,
            warningCodes: [],
            ...session,
            confirmActivated: vi.fn(async () => undefined),
            cancel: vi.fn(async () => undefined),
        }

        const result = runNativePreparedContentRoute(
            { type: 'desktopPath', path: 'C:\\chosen\\card.png' },
            'card.png',
            {
                prepare: vi.fn(async () => receipt),
                map: vi.fn(async (prepared) => prepared),
                activate: (prepared, activeReceipt, signal) =>
                    activatePreparedNativeCharacterContent(prepared, activeReceipt, deps, signal),
            },
            { signal: controller.signal },
        )
        await decodeStarted.promise
        controller.abort()
        pending.resolve(content.metadata)

        await expect(result).rejects.toMatchObject({ name: 'AbortError' })
        expect(receipt.cancel).toHaveBeenCalledOnce()
        expect(session.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(deps.map).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('cancels the retained route when abort arrives during semantic mapping', async () => {
        const controller = new AbortController()
        const pending = deferred<character | false>()
        const mapStarted = deferred<void>()
        const deps = dependencies({
            decodePng: vi.fn(async () => content.metadata as any),
            map: vi.fn(async () => {
                mapStarted.resolve()
                return pending.promise
            }),
        })
        const session = lifecycle()
        const receipt = {
            jobId: 'content-1',
            content: {
                ...content,
                format: 'png-card',
                metadata: { ccv3: 'encoded-card' },
                portraitLogicalId: content.assets[0].logicalId,
            } as PreparedNativeContent,
            warningCodes: [],
            ...session,
            confirmActivated: vi.fn(async () => undefined),
            cancel: vi.fn(async () => undefined),
        }

        const result = runNativePreparedContentRoute(
            { type: 'desktopPath', path: 'C:\\chosen\\card.png' },
            'card.png',
            {
                prepare: vi.fn(async () => receipt),
                map: vi.fn(async (prepared) => prepared),
                activate: (prepared, activeReceipt, signal) =>
                    activatePreparedNativeCharacterContent(prepared, activeReceipt, deps, signal),
            },
            { signal: controller.signal },
        )
        await mapStarted.promise
        controller.abort()
        pending.resolve(mappedCharacter())

        await expect(result).rejects.toMatchObject({ name: 'AbortError' })
        expect(receipt.cancel).toHaveBeenCalledOnce()
        expect(session.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('finishes atomic activation when abort arrives after owner-manifest sealing begins', async () => {
        const controller = new AbortController()
        const sealStarted = deferred<void>()
        const allowSeal = deferred<void>()
        const deps = dependencies()
        const session = lifecycle({
            prepareOwnerManifestAndSeal: vi.fn(async (bytes) => {
                sealStarted.resolve()
                await allowSeal.promise
                return {
                    contentHash: await ownerManifestIdentity(bytes),
                    byteSize: bytes.byteLength,
                    physicalKey: 'sealed-owner-manifest',
                    deduplicated: false,
                }
            }),
        })

        const result = activatePreparedNativeCharacterContent(
            content,
            session,
            deps,
            controller.signal,
        )
        await sealStarted.promise
        controller.abort()
        allowSeal.resolve()

        await expect(result).resolves.toEqual({ characterId: 'character-1' })
        expect(session.prepareOwnerManifestAndSeal).toHaveBeenCalledOnce()
        expect(deps.upsert).toHaveBeenCalledOnce()
    })

    it('returns a normal declined outcome without preparing or publishing anything', async () => {
        const deps = dependencies({ map: vi.fn(async (): Promise<false> => false) })
        const session = lifecycle()

        await expect(activatePreparedNativeCharacterContent(content, session, deps)).resolves.toBeNull()

        expect(session.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('classifies off-spec JSON before calling the character card mapper', async () => {
        const deps = dependencies()
        const session = lifecycle()

        await expect(activatePreparedNativeCharacterContent({
            ...content,
            metadata: { name: 'Legacy Tavern Card' },
        }, session, deps)).rejects.toBeInstanceOf(UnsupportedPreparedNativeCharacterCardError)

        expect(deps.map).not.toHaveBeenCalled()
        expect(session.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('keeps v2 JSON on the compatibility importer, even with a native mapping, until native v2 payload extraction exists', async () => {
        const deps = dependencies()
        const session = lifecycle()

        await expect(activatePreparedNativeCharacterContent({
            ...content,
            metadata: {
                spec: 'chara_card_v2',
                spec_version: '2.0',
                data: { extensions: {} },
            },
            nativeMapping: { outcome: 'mapped', character: mappedCharacter() as unknown as Record<string, unknown> },
        }, session, deps)).rejects.toBeInstanceOf(UnsupportedPreparedNativeCharacterCardError)

        expect(deps.map).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('commits a present empty additional-assets manifest', async () => {
        const character = mappedCharacter()
        character.additionalAssets = []
        const deps = dependencies({ map: vi.fn(async () => character) })
        const session = lifecycle()

        await activatePreparedNativeCharacterContent(content, session, deps)

        const manifestBytes = vi.mocked(session.prepareOwnerManifestAndSeal).mock.calls[0][0]
        expect(decodeOwnerManifest(manifestBytes)).toEqual([])
        expect(deps.upsert).toHaveBeenCalledWith(
            'character-1',
            'native-content-import',
            expect.any(Function),
            expect.objectContaining({
                assetOwnerHeads: [expect.objectContaining({
                    present: true,
                    entryCount: 0,
                })],
            }),
        )
    })

    it('fails closed when an additional asset does not have a prepared ordinary alias', async () => {
        const character = mappedCharacter()
        character.additionalAssets = [['missing', 'assets/missing.bin', 'bin']]
        const deps = dependencies({ map: vi.fn(async () => character) })
        const session = lifecycle()

        await expect(activatePreparedNativeCharacterContent(content, session, deps))
            .rejects.toThrow(/missing prepared asset alias/i)

        expect(session.prepareOwnerManifestAndSeal).not.toHaveBeenCalled()
        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('rejects a manifest CAS identity mismatch before the database commit', async () => {
        const deps = dependencies()
        const session = lifecycle({
            prepareOwnerManifestAndSeal: vi.fn(async (bytes) => ({
                contentHash: 'ff'.repeat(32),
                byteSize: bytes.byteLength,
                physicalKey: 'wrong-object',
                deduplicated: false,
            })),
        })

        await expect(activatePreparedNativeCharacterContent(content, session, deps))
            .rejects.toThrow(/manifest CAS identity mismatch/i)

        expect(deps.upsert).not.toHaveBeenCalled()
    })

    it('leaves activation failure visible to the retained native job coordinator', async () => {
        const deps = dependencies({
            upsert: vi.fn(async () => {
                throw new Error('revision conflict')
            }),
        })
        const session = lifecycle()

        await expect(activatePreparedNativeCharacterContent(content, session, deps))
            .rejects.toThrow('revision conflict')

        expect(session.prepareOwnerManifestAndSeal).toHaveBeenCalledOnce()
    })
})

// The card receipts the import wiring oracle imports as characters, with what the TS activation stored for each,
// recorded by benchmarks/rust-core-oracles/formats-wiring.oracle.test.ts. `native` is the mapping the Rust receipt
// carries, and `staged` the PNG inline payloads the TS decode staged.
type WiringCase = {
    id: string
    receipt: PreparedNativeContent
    native: 'mapped' | { reason: 'unsupported' | 'invalid'; name: string }
    result: { mapped?: string; stored?: string; manifest?: string; staged?: PreparedCardContentAssetDescriptor[]; error?: string }
}
const fixture = (name: string) => JSON.parse(readFileSync(resolve(process.cwd(), `tests/fixtures/rust-core/formats/${name}.json`), 'utf8'))
const wiringResults = new Map((fixture('wiring.expected').cases as { id: string; result: WiringCase['result'] }[]).map(({ id, result }) => [id, result]))
const wiringCases = (fixture('wiring.cases').cases as (Omit<WiringCase, 'result'> & { destination?: 'module' })[])
    .filter((entry) => entry.receipt.format !== 'risu-module' && entry.destination !== 'module')
    .map((entry) => ({ ...entry, result: wiringResults.get(entry.id)! }))
const hex = (bytes: Uint8Array) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')

// The PNG decode the production activation runs, with the RCC password prompt declined.
const decodePng: NativeCharacterContentActivationDependencies['decodePng'] = async (metadata, stageInlineAsset) =>
    await decodePreparedNativePngCardMetadata(metadata, {
        stageInlineAsset,
        hash: async (bytes) => createHash('sha256').update(bytes).digest('hex'),
        decrypt: (bytes, password) => decryptBuffer(bytes, password),
        requestPassword: async () => null,
    }) as Awaited<ReturnType<NativeCharacterContentActivationDependencies['decodePng']>>

// The record the upsert keeps and the owner manifest the activation seals, as the oracle records them. An inline
// payload the decode stages becomes the asset the oracle staged for it.
async function activateReceipt(
    entry: WiringCase,
    nativeMapping: PreparedNativeCharacterMapping | undefined,
    map: NativeCharacterContentActivationDependencies['map'],
): Promise<{ stored?: string; manifest?: string }> {
    const receipt: PreparedNativeContent = { ...structuredClone(entry.receipt), ...(nativeMapping === undefined ? {} : { nativeMapping }) }
    const staged = structuredClone(entry.result.staged ?? [])
    const kept: { stored?: string; manifest?: string } = {}
    const session = lifecycle({
        prepareOwnerManifestAndSeal: vi.fn(async (bytes): Promise<PreparedImmutablePayload> => {
            kept.manifest = hex(bytes)
            return {
                contentHash: await ownerManifestIdentity(bytes),
                byteSize: bytes.byteLength,
                physicalKey: 'unused-by-activation',
                deduplicated: false,
            }
        }),
        stageInlineAsset: vi.fn(async () => {
            const asset = staged.shift()
            if (!asset) throw new Error('the oracle staged no further asset')
            receipt.assets.push(asset)
            return asset
        }),
    })
    await activatePreparedNativeCharacterContent(receipt, session, dependencies({
        decodePng,
        map,
        upsert: vi.fn(async (_id, _reason, createOrMutate) => {
            kept.stored = JSON.stringify(canonicalClone(await createOrMutate(undefined)))
            return true
        }),
    }))
    return kept
}

describe('native card mapping', () => {
    const named = <Entry extends WiringCase>(entries: Entry[]) => entries.map((entry) => [entry.id, entry] as const)
    const mapped = wiringCases.filter((entry) => entry.native === 'mapped')
    const declined = wiringCases.flatMap((entry) => entry.native === 'mapped' ? [] : [{ ...entry, native: entry.native }])
    const declinedAndStored = declined.filter((entry) => entry.result.stored !== undefined)
    const declinedAndFailed = declined.filter((entry) => entry.result.error !== undefined)
    // What the TS mapping of the entry's card returned or threw, as the oracle recorded it.
    const tsMapping = (entry: WiringCase) => vi.fn(async () => {
        if (entry.result.error !== undefined) throw new Error(entry.result.error)
        return JSON.parse(entry.result.mapped!) as character
    })
    const tsMappingRuns = async (): Promise<never> => {
        throw new Error('the TS mapping ran')
    }
    const recorded = (entry: WiringCase) => ({ stored: entry.result.stored, manifest: entry.result.manifest })

    it('replays every wiring receipt', () => {
        expect([mapped.length, declinedAndStored.length, declinedAndFailed.length]).toEqual([12, 6, 13])
    })

    // With the Rust receipt test, which proves the native mapping is the text the TS mapping stringifies to, this is
    // the same card through both paths.
    it.each(named(mapped))('stores the native mapping of %s exactly as the TS path stores its own', async (_id, entry) => {
        const character = JSON.parse(entry.result.mapped!) as Record<string, unknown>
        await expect(activateReceipt(entry, { outcome: 'mapped', character }, tsMappingRuns)).resolves.toEqual(recorded(entry))
    })

    it.each(named(declinedAndStored))('maps %s in TS where the native mapping declines it', async (_id, entry) => {
        await expect(activateReceipt(entry, { outcome: 'declined', ...entry.native }, tsMapping(entry))).resolves.toEqual(recorded(entry))
    })

    // A card the native mapping finds invalid is mapped in TS too, which is the reference: it stores what TS maps.
    it.each([
        ['limit', 'maxOwnedBytes'],
        ['cancelled', ''],
        ['host', 'stack'],
        ['failed', 'panic'],
        ...['card', 'dataUri', 'missingAsset', 'ccdefaultPortrait', 'duplicateAssetToken', 'assetToken', 'portraitLogicalId', 'assetReference']
            .map((name) => ['invalid', name] as const),
    ] as const)('maps the card in TS where the native mapping declines it as %s %s', async (reason, name) => {
        const entry = mapped[0]
        await expect(activateReceipt(entry, { outcome: 'declined', reason, name }, tsMapping(entry))).resolves.toEqual(recorded(entry))
    })

    it.each(named(declinedAndFailed))('rejects %s with the exact TS error where the native mapping declines it', async (_id, entry) => {
        await expect(activateReceipt(entry, { outcome: 'declined', ...entry.native }, tsMapping(entry)))
            .rejects.toHaveProperty('message', entry.result.error)
    })

    it('maps a PNG card in TS once its decode staged an inline payload the native mapping did not read', async () => {
        const entry = wiringCases.find(({ id }) => id === 'png-v3-inline-data-uri')!
        const elsewhere = JSON.parse(mapped[0].result.mapped!) as Record<string, unknown>
        await expect(activateReceipt(entry, { outcome: 'mapped', character: elsewhere }, tsMapping(entry))).resolves.toEqual(recorded(entry))
    })
})
