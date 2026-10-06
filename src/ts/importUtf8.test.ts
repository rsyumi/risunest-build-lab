import { createRequire } from 'node:module'
import { describe, expect, it, vi } from 'vitest'
import { decodeImportUtf8, parseImportJson } from './importUtf8'

// The package decoder, not Node's native Buffer, defines browser imports.
const BrowserBuffer = createRequire(import.meta.url)('buffer/').Buffer as typeof Buffer
const expected = (bytes: Uint8Array) => BrowserBuffer.from(bytes).toString('utf8')

describe('bounded import UTF-8 decoding', () => {
    it('uses native decoding without a code-unit array for large valid text', () => {
        const text = 'Synthetic 한글 é 😀\uFEFF'.repeat(100_000)
        const bytes = new TextEncoder().encode(text)
        const codeUnits = vi.spyOn(String, 'fromCharCode')
        try {
            expect(decodeImportUtf8(bytes)).toBe(text)
            expect(codeUnits).not.toHaveBeenCalled()
        } finally {
            codeUnits.mockRestore()
        }
    })

    it('matches browser Buffer for every one-byte and two-byte input', () => {
        for (let first = 0; first < 256; first++) {
            expect(decodeImportUtf8(Uint8Array.of(first))).toBe(expected(Uint8Array.of(first)))
            for (let second = 0; second < 256; second++) {
                const bytes = Uint8Array.of(first, second)
                if (decodeImportUtf8(bytes) !== expected(bytes)) {
                    throw new Error(`UTF-8 mismatch at ${first}, ${second}`)
                }
            }
        }
    })

    it.each([
        [0xe2, 0x82], [0xf0, 0x9f, 0x98], [0xe2, 0x82, 0x41],
        [0xed, 0xa0, 0x80], [0xe0, 0x80, 0x80], [0xf0, 0x80, 0x80, 0x80],
        [0xf4, 0x90, 0x80, 0x80], [0xf8, 0x90, 0x80, 0x80],
        [0xef, 0xbb, 0xbf], [0xf0, 0x9f, 0x98, 0x80],
    ])('preserves replacement and complete sequences across flush boundaries: %j', (...sequence) => {
        for (let padding = 4093; padding <= 4097; padding++) {
            const bytes = new Uint8Array(padding + sequence.length + 1).fill(0x61)
            bytes[0] = 0xff
            bytes.set(sequence, padding)
            expect(decodeImportUtf8(bytes)).toBe(expected(bytes))
            expect(decodeImportUtf8(bytes.subarray(0, -1))).toBe(expected(bytes.subarray(0, -1)))
        }
    })

    it('bounds the fallback code-unit scratch space independently of input length', () => {
        const bytes = new Uint8Array(100_000).fill(0x80)
        const codeUnits = vi.spyOn(String, 'fromCharCode')
        try {
            const text = decodeImportUtf8(bytes)
            expect(text).toBe('\uFFFD'.repeat(bytes.length))
            expect(codeUnits.mock.calls.length).toBeGreaterThan(1)
            expect(Math.max(...codeUnits.mock.calls.map(call => call.length))).toBeLessThanOrEqual(4097)
        } finally {
            codeUnits.mockRestore()
        }
    })

    it('preserves BOM and exact subarray offsets, including JSON rejection', () => {
        const bytes = new TextEncoder().encode('prefix\uFEFF{"name":"한글 😀"}suffix')
        const view = bytes.subarray(6, bytes.length - 6)
        expect(decodeImportUtf8(view)).toBe(expected(view))
        expect(decodeImportUtf8(view).charCodeAt(0)).toBe(0xfeff)
        expect(() => parseImportJson(view)).toThrow(SyntaxError)
        expect(parseImportJson(view.subarray(3)).value).toEqual({ name: '한글 😀' })
    })
})
