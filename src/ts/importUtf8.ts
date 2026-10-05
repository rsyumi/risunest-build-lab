const nativeDecoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })
const CODE_UNIT_CHUNK = 4096

// Buffer's browser decoder replaces each invalid byte, unlike TextDecoder's
// replacement of an entire incomplete sequence. Keep that import contract.
export function decodeImportUtf8(bytes: Uint8Array): string {
    try {
        return nativeDecoder.decode(bytes)
    } catch (error) {
        if (!(error instanceof TypeError)) throw error
    }

    const parts: string[] = []
    const units: number[] = []
    for (let offset = 0; offset < bytes.length;) {
        const first = bytes[offset]
        let width = first > 0xef ? 4 : first > 0xdf ? 3 : first > 0xbf ? 2 : 1
        let point: number | null = null
        if (offset + width <= bytes.length) {
            const second = bytes[offset + 1]
            const third = bytes[offset + 2]
            const fourth = bytes[offset + 3]
            if (width === 1 && first < 0x80) {
                point = first
            } else if (width === 2 && (second & 0xc0) === 0x80) {
                const candidate = ((first & 0x1f) << 6) | (second & 0x3f)
                if (candidate > 0x7f) point = candidate
            } else if (width === 3 && (second & 0xc0) === 0x80 && (third & 0xc0) === 0x80) {
                const candidate = ((first & 0x0f) << 12) | ((second & 0x3f) << 6) | (third & 0x3f)
                if (candidate > 0x7ff && (candidate < 0xd800 || candidate > 0xdfff)) point = candidate
            } else if (width === 4 && (second & 0xc0) === 0x80 && (third & 0xc0) === 0x80 && (fourth & 0xc0) === 0x80) {
                const candidate = ((first & 0x0f) << 18) | ((second & 0x3f) << 12) | ((third & 0x3f) << 6) | (fourth & 0x3f)
                if (candidate > 0xffff && candidate < 0x110000) point = candidate
            }
        }
        if (point === null) {
            point = 0xfffd
            width = 1
        } else if (point > 0xffff) {
            point -= 0x10000
            units.push(((point >>> 10) & 0x3ff) | 0xd800)
            point = 0xdc00 | (point & 0x3ff)
        }
        units.push(point)
        if (units.length >= CODE_UNIT_CHUNK) {
            parts.push(String.fromCharCode(...units))
            units.length = 0
        }
        offset += width
    }
    if (units.length) parts.push(String.fromCharCode(...units))
    return parts.join('')
}

export interface ParsedImportJson {
    value: any
}

export function parseImportJson(bytes: Uint8Array): ParsedImportJson {
    return { value: JSON.parse(decodeImportUtf8(bytes)) }
}
