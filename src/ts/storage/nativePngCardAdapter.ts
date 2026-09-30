import { Buffer } from 'buffer'
import type { PreparedCardContentAssetDescriptor } from './nativeFileJobs'

export interface PreparedNativePngCardMetadata {
    chara?: string
    ccv3?: string
}

export interface PreparedNativePngCardDecodeDependencies {
    hash(bytes: Uint8Array): Promise<string>
    decrypt(bytes: Uint8Array, password: string): Promise<Uint8Array | ArrayBuffer>
    requestPassword(): Promise<string | null>
    onOversizedInlineAsset?(): void
    stageInlineAsset?(bytes: Uint8Array, name: string): Promise<PreparedCardContentAssetDescriptor>
}

export type DecodedPreparedNativePngCard = Record<string, unknown>

export class UnsupportedPreparedNativeCharacterCardError extends TypeError {
    readonly code = 'unsupported-character-card'

    constructor(message = 'Prepared content is not a supported character card') {
        super(message)
        this.name = 'UnsupportedPreparedNativeCharacterCardError'
    }
}

export class InvalidPreparedNativePngCardError extends TypeError {
    readonly code = 'invalid-png-card-metadata'

    constructor(message: string) {
        super(message)
        this.name = 'InvalidPreparedNativePngCardError'
    }
}

function decodeStandardBase64(value: string, label: string): Uint8Array {
    if (!isStandardBase64(value)) {
        throw new InvalidPreparedNativePngCardError(`${label} is not valid standard base64`)
    }
    const decoded = Buffer.from(value, 'base64')
    return new Uint8Array(decoded.buffer, decoded.byteOffset, decoded.byteLength)
}

function isStandardBase64(value: string): boolean {
    if (value.length === 0 || value.length % 4 !== 0) return false
    const padding = value.endsWith('==') ? 2 : value.endsWith('=') ? 1 : 0
    const contentLength = value.length - padding
    for (let index = 0; index < contentLength; index += 1) {
        const code = value.charCodeAt(index)
        const valid =
            (code >= 0x41 && code <= 0x5a)
            || (code >= 0x61 && code <= 0x7a)
            || (code >= 0x30 && code <= 0x39)
            || code === 0x2b
            || code === 0x2f
        if (!valid) return false
    }
    for (let index = contentLength; index < value.length; index += 1) {
        if (value.charCodeAt(index) !== 0x3d) return false
    }
    return padding === 0
        ? contentLength % 4 === 0
        : padding === 1
          ? contentLength % 4 === 3
          : contentLength % 4 === 2
}

function parseJsonObject(bytes: Uint8Array, label: string): DecodedPreparedNativePngCard {
    let value: unknown
    try {
        value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes))
    }
    catch {
        throw new InvalidPreparedNativePngCardError(`${label} is not valid UTF-8 JSON`)
    }
    if (typeof value !== 'object' || value === null || Array.isArray(value)) {
        throw new InvalidPreparedNativePngCardError(`${label} must decode to a JSON object`)
    }
    return value as DecodedPreparedNativePngCard
}

function normalizeV2CharacterVersion(card: DecodedPreparedNativePngCard): void {
    if (card.spec !== 'chara_card_v2') return
    const data = card.data
    if (typeof data !== 'object' || data === null || Array.isArray(data)) return
    const characterVersion = (data as Record<string, unknown>).character_version
    if (typeof characterVersion === 'number') {
        (data as Record<string, unknown>).character_version = characterVersion.toString()
    }
}

async function stageInlinePayloads(
    card: DecodedPreparedNativePngCard,
    dependencies: PreparedNativePngCardDecodeDependencies,
): Promise<void> {
    const data = card.data
    if (typeof data !== 'object' || data === null || Array.isArray(data)) return
    const cardData = data as Record<string, unknown>
    const stage = async (encoded: string, name = '') => {
        if (!dependencies.stageInlineAsset) {
            throw new InvalidPreparedNativePngCardError('Native PNG inline asset staging is unavailable')
        }
        const asset = await dependencies.stageInlineAsset(Buffer.from(encoded, 'base64'), name)
        return `__asset:${asset.token}`
    }
    if (card.spec === 'chara_card_v3' && Array.isArray(cardData.assets)) {
        const retained: unknown[] = []
        for (const value of cardData.assets) {
            if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
                const asset = value as Record<string, unknown>
                if (typeof asset.uri === 'string' && asset.uri.startsWith('data:')) {
                    const encoded = asset.uri.split(',')[1]
                    if (encoded === undefined) {
                        throw new InvalidPreparedNativePngCardError('PNG data URI has no payload')
                    }
                    if (encoded.length >= 50 * 1024 * 1024) {
                        dependencies.onOversizedInlineAsset?.()
                        continue
                    }
                    asset.uri = await stage(encoded)
                }
            }
            retained.push(value)
        }
        cardData.assets = retained
        return
    }
    if (card.spec !== 'chara_card_v2') return
    const extensions = cardData.extensions
    if (typeof extensions !== 'object' || extensions === null || Array.isArray(extensions)) return
    const risuai = (extensions as Record<string, unknown>).risuai
    if (typeof risuai !== 'object' || risuai === null || Array.isArray(risuai)) return
    const risu = risuai as Record<string, unknown>
    for (const field of ['emotions', 'additionalAssets'] as const) {
        const tuples = risu[field]
        if (!Array.isArray(tuples)) continue
        for (const tuple of tuples) {
            if (Array.isArray(tuple) && typeof tuple[1] === 'string' && !tuple[1].startsWith('__asset:')) {
                tuple[1] = await stage(tuple[1], field === 'additionalAssets' && typeof tuple[2] === 'string' ? tuple[2] : '')
            }
        }
    }
    const vits = risu.vits
    if (typeof vits === 'object' && vits !== null && !Array.isArray(vits)) {
        const values = vits as Record<string, unknown>
        for (const [key, value] of Object.entries(values)) {
            if (typeof value === 'string' && !value.startsWith('__asset:')) values[key] = await stage(value)
        }
    }
}

async function decodeRcc(
    encoded: string,
    dependencies: PreparedNativePngCardDecodeDependencies,
): Promise<DecodedPreparedNativePngCard | null> {
    const parts = encoded.split('||')
    if (parts.length !== 5 || parts[0] !== 'rcc' || parts[1] !== 'rccv1') {
        throw new InvalidPreparedNativePngCardError('RCC envelope is invalid')
    }
    const encrypted = decodeStandardBase64(parts[2], 'RCC encrypted payload')
    if (await dependencies.hash(encrypted) !== parts[3]) {
        throw new InvalidPreparedNativePngCardError('RCC encrypted payload hash does not match')
    }
    const envelope = parseJsonObject(
        decodeStandardBase64(parts[4], 'RCC envelope metadata'),
        'RCC envelope metadata',
    )
    let password = 'RISU_NONE'
    if (envelope.usePassword === true) {
        const requested = await dependencies.requestPassword()
        if (!requested) return null
        password = requested
    }
    let decrypted: Uint8Array | ArrayBuffer
    try {
        decrypted = await dependencies.decrypt(encrypted, password)
    }
    catch {
        throw new InvalidPreparedNativePngCardError('RCC payload could not be decrypted')
    }
    return parseJsonObject(
        decrypted instanceof Uint8Array ? decrypted : new Uint8Array(decrypted),
        'RCC payload',
    )
}

export async function decodePreparedNativePngCardMetadata(
    metadata: PreparedNativePngCardMetadata,
    dependencies: PreparedNativePngCardDecodeDependencies,
): Promise<DecodedPreparedNativePngCard | null> {
    const selected = metadata.ccv3 ?? metadata.chara
    if (!selected) {
        throw new InvalidPreparedNativePngCardError('PNG card metadata is missing')
    }
    const card = selected.startsWith('rcc||')
        ? await decodeRcc(selected, dependencies)
        : parseJsonObject(
            decodeStandardBase64(selected, 'PNG card metadata'),
            'PNG card metadata',
        )
    if (!card) return null
    normalizeV2CharacterVersion(card)
    await stageInlinePayloads(card, dependencies)
    return card
}
