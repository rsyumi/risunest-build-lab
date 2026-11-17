import { language } from 'src/lang'

export type ImageFormat = {
    ext: 'png' | 'jpg' | 'gif' | 'webp' | 'avif'
    mime: string
    aliases: readonly string[]
}

export const imageFormats: readonly ImageFormat[] = [
    { ext: 'png', mime: 'image/png', aliases: ['png', 'apng'] },
    { ext: 'jpg', mime: 'image/jpeg', aliases: ['jpg', 'jpeg', 'jfif', 'jpe', 'pjpeg', 'pjp'] },
    { ext: 'gif', mime: 'image/gif', aliases: ['gif'] },
    { ext: 'webp', mime: 'image/webp', aliases: ['webp'] },
    { ext: 'avif', mime: 'image/avif', aliases: ['avif'] },
]

export const imageAliases = imageFormats.flatMap(format => format.aliases)

const text = new TextDecoder()
const ascii = (data: Uint8Array, start: number, end: number) => text.decode(data.subarray(start, end))

export function imageFormatForName(name: string): ImageFormat | undefined {
    const extension = name.includes('.') ? name.split('.').at(-1)?.toLowerCase() : ''
    return imageFormats.find(format => format.aliases.includes(extension))
}

export function stripImageExtension(name: string): string {
    return imageFormatForName(name) ? name.slice(0, name.lastIndexOf('.')) : name
}

export function isGifImage(data: Uint8Array): boolean {
    return ['GIF87a', 'GIF89a'].includes(ascii(data, 0, 6))
}

export function hasAvifBrand(data: Uint8Array): boolean {
    const view = new DataView(data.buffer, data.byteOffset, data.byteLength)
    for (let offset = 0; offset + 8 <= data.byteLength;) {
        const size32 = view.getUint32(offset)
        const type = ascii(data, offset + 4, offset + 8)
        let headerSize = 8
        let boxSize = size32
        if (size32 === 1) {
            if (offset + 16 > data.byteLength) return false
            boxSize = view.getUint32(offset + 8) * 0x1_0000_0000 + view.getUint32(offset + 12)
            headerSize = 16
        } else if (size32 === 0) {
            boxSize = data.byteLength - offset
        }
        if (!Number.isSafeInteger(boxSize) || boxSize < headerSize) return false
        if (type === 'ftyp') {
            const brandsStart = offset + headerSize
            // Recognize a truncated ftyp too, so validation reports a damaged image.
            const end = Math.min(offset + boxSize, data.byteLength)
            for (let brandOffset = brandsStart; brandOffset + 4 <= end; brandOffset += brandOffset === brandsStart ? 8 : 4) {
                if (['avif', 'avis'].includes(ascii(data, brandOffset, brandOffset + 4))) return true
            }
            return false
        }
        if (offset + boxSize > data.byteLength) return false
        offset += boxSize
    }
    return false
}

export function imageFormatFromBytes(data: Uint8Array): ImageFormat | null {
    if (data.length >= 8 && [137, 80, 78, 71, 13, 10, 26, 10].every((v, i) => data[i] === v)) return imageFormats[0]
    if (data.length >= 3 && data[0] === 255 && data[1] === 216 && data[2] === 255) return imageFormats[1]
    if (isGifImage(data)) return imageFormats[2]
    if (ascii(data, 0, 4) === 'RIFF' && ascii(data, 8, 12) === 'WEBP') return imageFormats[3]
    if (hasAvifBrand(data)) return imageFormats[4]
    return null
}

export class ImageFormatError extends Error {
    constructor(readonly reason: 'unsupported' | 'damaged') {
        super(language.risuNest.imageFormats[reason])
        this.name = 'ImageFormatError'
    }
}

function hasImageHeader(data: Uint8Array, format: ImageFormat): boolean {
    const view = new DataView(data.buffer, data.byteOffset, data.byteLength)
    switch (format.ext) {
        case 'png':
            return data.length >= 33 && view.getUint32(8) === 13 && ascii(data, 12, 16) === 'IHDR'
                && view.getUint32(16) > 0 && view.getUint32(20) > 0
                && ({ 0: [1, 2, 4, 8, 16], 2: [8, 16], 3: [1, 2, 4, 8], 4: [8, 16], 6: [8, 16] }[data[25]]?.includes(data[24]) ?? false)
                && data[26] === 0 && data[27] === 0 && data[28] <= 1
        case 'gif':
            return data.length >= 13 && view.getUint16(6, true) > 0 && view.getUint16(8, true) > 0
        case 'webp': {
            if (data.length < 20 || view.getUint32(4, true) + 8 > data.length) return false
            const kind = ascii(data, 12, 16)
            const size = view.getUint32(16, true)
            return size + (size % 2) + 20 <= view.getUint32(4, true) + 8
                && ((kind === 'VP8 ' && size >= 10) || (kind === 'VP8L' && size >= 5) || (kind === 'VP8X' && size === 10))
        }
        case 'jpg': {
            for (let offset = 2; offset + 4 <= data.length;) {
                if (data[offset++] !== 255) return false
                while (data[offset] === 255) offset++
                const marker = data[offset++]
                if (offset + 2 > data.length || marker === 0xda || marker === 0xd9) return false
                const length = view.getUint16(offset)
                if (length < 2 || offset + length > data.length) return false
                if (marker >= 0xc0 && marker <= 0xcf && ![0xc4, 0xc8, 0xcc].includes(marker)) {
                    return length >= 8 && view.getUint16(offset + 3) > 0 && view.getUint16(offset + 5) > 0
                }
                offset += length
            }
            return false
        }
        case 'avif': {
            for (let offset = 0; offset + 8 <= data.length;) {
                const size32 = view.getUint32(offset)
                const header = size32 === 1 ? 16 : 8
                if (offset + header > data.length) return false
                const size = size32 === 1 ? view.getUint32(offset + 8) * 0x1_0000_0000 + view.getUint32(offset + 12)
                    : size32 === 0 ? data.length - offset : size32
                if (!Number.isSafeInteger(size) || size < header || offset + size > data.length) return false
                if (ascii(data, offset + 4, offset + 8) === 'ftyp') return size >= header + 8 && (size - header) % 4 === 0
                offset += size
            }
            return false
        }
    }
}

export function identifyImage(data: Uint8Array): ImageFormat | null {
    const format = imageFormatFromBytes(data)
    if (format && !hasImageHeader(data, format)) throw new ImageFormatError('damaged')
    return format
}

export function requireImage(data: Uint8Array): ImageFormat {
    const format = identifyImage(data)
    if (!format) throw new ImageFormatError('unsupported')
    return format
}

export function selectedImageFormat(data: Uint8Array, name: string): ImageFormat | null {
    const format = identifyImage(data)
    if (!format && imageFormatForName(name)) throw new ImageFormatError('unsupported')
    return format
}
