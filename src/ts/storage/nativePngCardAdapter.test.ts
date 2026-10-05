import { Buffer } from 'buffer'
import { describe, expect, it, vi } from 'vitest'

import {
    decodePreparedNativePngCardMetadata,
    InvalidPreparedNativePngCardError,
} from './nativePngCardAdapter'

function encoded(value: unknown): string {
    return Buffer.from(JSON.stringify(value), 'utf8').toString('base64')
}

/**
 * Encodes `value` with its one `marker` string replaced by `count` ASCII 'A's. The
 * repeated run is spliced in as whole base64 quanta ('AAA' is 'QUFB') instead of
 * passing tens of MiB through the byte-wise encoder.
 */
function encodedWithRepeatedA(value: unknown, marker: string, count: number): string {
    const parts = JSON.stringify(value).split(marker)
    if (parts.length !== 2 || /[^\x00-\x7f]/.test(parts[0])) throw new Error('Expected one marker after ASCII JSON')
    const [head, tail] = parts
    const lead = (3 - head.length % 3) % 3
    const quanta = count < lead ? 0 : Math.floor((count - lead) / 3)
    if (quanta === 0) return Buffer.from(head + 'A'.repeat(count) + tail, 'utf8').toString('base64')
    return Buffer.from(head + 'A'.repeat(lead), 'utf8').toString('base64')
        + 'QUFB'.repeat(quanta)
        + Buffer.from('A'.repeat(count - lead - quanta * 3) + tail, 'utf8').toString('base64')
}

function v2(name = 'V2'): Record<string, unknown> {
    return {
        spec: 'chara_card_v2',
        spec_version: '2.0',
        data: {
            name,
            character_version: 7,
            extensions: {},
        },
    }
}

function v3(name = 'V3'): Record<string, unknown> {
    return {
        spec: 'chara_card_v3',
        spec_version: '3.0',
        data: { name, extensions: {} },
    }
}

const unusedRccDependencies = {
    hash: vi.fn(async () => 'unused'),
    decrypt: vi.fn(async () => new Uint8Array()),
    requestPassword: vi.fn(async () => null),
}

describe('prepared native PNG card metadata adapter', () => {
    it('selects ccv3 over stale chara and returns no encoded payload', async () => {
        const result = await decodePreparedNativePngCardMetadata({
            chara: 'not-valid-base64',
            ccv3: encoded(v3()),
        }, unusedRccDependencies)

        expect(result).toEqual(v3())
        expect(JSON.stringify(result)).not.toContain(encoded(v3()))
    })

    it('decodes native PNG metadata above the legacy five MiB ceiling', async () => {
        const card = v3('Large native card') as any
        card.data.description = 'x'.repeat(6 * 1024 * 1024)

        const result = await decodePreparedNativePngCardMetadata({
            ccv3: encoded(card),
        }, unusedRccDependencies)

        expect((result?.data as any).description).toHaveLength(6 * 1024 * 1024)
    })

    it('decodes v2 and normalizes a numeric character_version', async () => {
        const result = await decodePreparedNativePngCardMetadata({
            chara: encoded(v2()),
        }, unusedRccDependencies)

        expect(result).toMatchObject({
            spec: 'chara_card_v2',
            data: { character_version: '7' },
        })
    })

    it('keeps an off-spec Tavern card available to the compatibility mapper', async () => {
        const card = {
            name: 'Tavern',
            description: 'Description',
            first_mes: 'Hello',
        }

        await expect(decodePreparedNativePngCardMetadata({
            chara: encoded(card),
        }, unusedRccDependencies)).resolves.toEqual(card)
    })

    it('verifies and decrypts an RCC card with the requested password', async () => {
        const encrypted = Uint8Array.of(1, 2, 3)
        const metadata = encoded({ usePassword: true })
        const dependencies = {
            hash: vi.fn(async () => 'verified-hash'),
            decrypt: vi.fn(async () => Buffer.from(JSON.stringify(v2('Encrypted')), 'utf8')),
            requestPassword: vi.fn(async () => 'secret'),
        }

        const result = await decodePreparedNativePngCardMetadata({
            chara: `rcc||rccv1||${Buffer.from(encrypted).toString('base64')}||verified-hash||${metadata}`,
        }, dependencies)

        expect(result).toMatchObject({ data: { name: 'Encrypted' } })
        expect(dependencies.hash).toHaveBeenCalledWith(encrypted)
        expect(dependencies.requestPassword).toHaveBeenCalledOnce()
        expect(dependencies.decrypt).toHaveBeenCalledWith(encrypted, 'secret')
    })

    it('uses the legacy RISU_NONE key for an RCC card without a password', async () => {
        const encrypted = Uint8Array.of(4, 5, 6)
        const dependencies = {
            hash: vi.fn(async () => 'verified-hash'),
            decrypt: vi.fn(async () => Buffer.from(JSON.stringify(v3('No Password')), 'utf8')),
            requestPassword: vi.fn(async () => 'must-not-be-used'),
        }

        await decodePreparedNativePngCardMetadata({
            chara: `rcc||rccv1||${Buffer.from(encrypted).toString('base64')}||verified-hash||${encoded({ usePassword: false })}`,
        }, dependencies)

        expect(dependencies.requestPassword).not.toHaveBeenCalled()
        expect(dependencies.decrypt).toHaveBeenCalledWith(encrypted, 'RISU_NONE')
    })

    it('returns null when the password prompt is cancelled', async () => {
        const encrypted = Uint8Array.of(7)
        const dependencies = {
            hash: vi.fn(async () => 'verified-hash'),
            decrypt: vi.fn(async () => Buffer.from('{}')),
            requestPassword: vi.fn(async () => null),
        }

        await expect(decodePreparedNativePngCardMetadata({
            chara: `rcc||rccv1||${Buffer.from(encrypted).toString('base64')}||verified-hash||${encoded({ usePassword: true })}`,
        }, dependencies)).resolves.toBeNull()
        expect(dependencies.decrypt).not.toHaveBeenCalled()
    })

    it('rejects a mismatched RCC ciphertext hash', async () => {
        const encrypted = Uint8Array.of(8)
        const dependencies = {
            hash: vi.fn(async () => 'actual-hash'),
            decrypt: vi.fn(async () => Buffer.from('{}')),
            requestPassword: vi.fn(async () => null),
        }

        await expect(decodePreparedNativePngCardMetadata({
            chara: `rcc||rccv1||${Buffer.from(encrypted).toString('base64')}||expected-hash||${encoded({})}`,
        }, dependencies)).rejects.toBeInstanceOf(InvalidPreparedNativePngCardError)
        expect(dependencies.decrypt).not.toHaveBeenCalled()
    })

    it('stages v2 inline emotions, additional assets and VITS using legacy base64 decoding', async () => {
        const inline = v2() as any
        inline.data.extensions.risuai = {
            emotions: [['happy', 'AQI- _==']],
            additionalAssets: [['audio', 'AwQ=', 'clip.wav']],
            vits: { model: 'BQY=' },
        }
        const staged: Uint8Array[] = []
        const stageInlineAsset = vi.fn(async (bytes: Uint8Array, name: string) => {
            staged.push(bytes)
            return { token: `inline-${staged.length}`, referenceKey: `inline-${staged.length}`,
                logicalId: `assets/${staged.length}.png`, objectHash: 'a'.repeat(64),
                byteSize: bytes.length, mime: '', name: name || 'inline.png', ext: 'png' }
        })
        const result = await decodePreparedNativePngCardMetadata({ chara: encoded(inline) }, {
            ...unusedRccDependencies, stageInlineAsset,
        }) as any
        expect(staged).toEqual(['AQI- _==', 'AwQ=', 'BQY='].map((value) => Buffer.from(value, 'base64')))
        expect(stageInlineAsset.mock.calls.map(([, name]) => name)).toEqual(['', 'clip.wav', ''])
        expect(result.data.extensions.risuai).toEqual({
            emotions: [['happy', '__asset:inline-1']],
            additionalAssets: [['audio', '__asset:inline-2', 'clip.wav']],
            vits: { model: '__asset:inline-3' },
        })
    })

    it('stages v3 data URI bytes while retaining native and remote references', async () => {
        const inline = v3() as any
        inline.data.assets = [
            { type: 'icon', name: 'main', uri: 'data:image/png;base64,AQI=,ignored' },
            { type: 'emotion', name: 'happy', uri: '__asset:existing' },
            { type: 'icon', name: 'other', uri: 'ccdefault:' },
        ]
        const stageInlineAsset = vi.fn(async (bytes: Uint8Array) => ({
            token: 'inline', referenceKey: 'inline', logicalId: 'assets/inline.png',
            objectHash: 'a'.repeat(64), byteSize: bytes.length, mime: '', name: 'inline.png', ext: 'png',
        }))
        const result = await decodePreparedNativePngCardMetadata({ ccv3: encoded(inline) }, {
            ...unusedRccDependencies, stageInlineAsset,
        }) as any
        expect(stageInlineAsset).toHaveBeenCalledExactlyOnceWith(Buffer.from('AQI=', 'base64'), '')
        expect(result.data.assets.map((asset: any) => asset.uri)).toEqual(['__asset:inline', '__asset:existing', 'ccdefault:'])
    })

    it('builds a spliced repeated payload identical to its whole encoding', () => {
        for (const name of ['V3', 'V3x', 'V3xy']) {
            for (const count of [0, 1, 2, 3, 4, 5, 6, 7, 64]) {
                const whole = v3(name) as any
                whole.data.assets = [{ uri: `data:application/octet-stream;base64,${'A'.repeat(count)}` }]
                const marked = v3(name) as any
                marked.data.assets = [{ uri: 'data:application/octet-stream;base64,<repeat>' }]
                expect(encodedWithRepeatedA(marked, '<repeat>', count)).toBe(encoded(whole))
            }
        }
    })

    it('skips an encoded data URI at the legacy fifty MiB boundary', async () => {
        const inline = v3() as any
        inline.data.assets = [{ uri: 'data:application/octet-stream;base64,<repeat>' }]
        const stageInlineAsset = vi.fn()
        const onOversizedInlineAsset = vi.fn()
        const ccv3 = encodedWithRepeatedA(inline, '<repeat>', 50 * 1024 * 1024)
        const result = await decodePreparedNativePngCardMetadata({ ccv3 }, {
            ...unusedRccDependencies, stageInlineAsset, onOversizedInlineAsset,
        }) as any
        expect(result.data.assets).toEqual([])
        expect(stageInlineAsset).not.toHaveBeenCalled()
        expect(onOversizedInlineAsset).toHaveBeenCalledOnce()
    })

    it('propagates inline staging failure instead of selecting a whole-file fallback', async () => {
        const inline = v3() as any
        inline.data.assets = [{ uri: 'data:image/png;base64,AA==' }]
        const failure = new Error('cancelled staging')
        await expect(decodePreparedNativePngCardMetadata({ ccv3: encoded(inline) }, {
            ...unusedRccDependencies, stageInlineAsset: async () => { throw failure },
        })).rejects.toBe(failure)
    })
})
