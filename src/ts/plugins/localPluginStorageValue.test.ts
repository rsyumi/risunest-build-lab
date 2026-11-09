// @vitest-environment node
import { describe, expect, it } from 'vitest'
import {
    STRUCTURE_MARKER, decodeStructuredLocalPluginValue, encodeStructuredLocalPluginValue,
    isPlainLocalPluginValue, isStructureText, parseStructureText, structureParts, structureText,
} from './localPluginStorageValue'

const fakeHash = (index: number) => index.toString(16).padStart(64, '0')

async function roundTrip<T>(value: T, partBytes = 7): Promise<{ value: T; parts: Uint8Array[]; text: string }> {
    const encoded = encodeStructuredLocalPluginValue(value)
    const parts: Uint8Array[] = []
    for await (const part of structureParts(encoded, partBytes)) parts.push(part.slice())
    const text = structureText(encoded, parts.map((_, index) => fakeHash(index)), partBytes)
    const structure = parseStructureText(text, partBytes)
    const decoded = await decodeStructuredLocalPluginValue<T>(structure, async index => parts[index], partBytes)
    return { value: decoded, parts, text }
}

describe('plain local plugin values', () => {
    it('keeps only values JSON reproduces exactly as plain', () => {
        for (const value of [null, true, 'text \ud800', 0, 1.5, [1, [2]], { a: { b: [] } }, Object.create(null)]) {
            expect(isPlainLocalPluginValue(value), JSON.stringify(value)).toBe(true)
        }
        const shared = { x: 1 }
        const holes = [1, , 3]
        const extra = Object.assign([1], { label: 'kept by structured clone' })
        for (const value of [undefined, -0, NaN, Infinity, 1n, [undefined], { a: undefined }, holes, extra,
            [shared, shared], new Date(0), new Map(), new Uint8Array(1), new Blob([]), Object(1), /x/]) {
            expect(isPlainLocalPluginValue(value)).toBe(false)
        }
    })
})

describe('structured local plugin values', () => {
    it('round-trips every type in the supported range with identical types and bytes', async () => {
        const buffer = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]).buffer
        const file = new File([new Uint8Array([9, 8, 7])], 'name.bin', { type: 'application/x-test', lastModified: 1234 })
        const error = new RangeError('out of range')
        const value = {
            undefinedValue: undefined, nan: NaN, inf: -Infinity, negativeZero: -0, big: -12345678901234567890n,
            holes: [1, , 3], date: new Date(42), invalidDate: new Date(NaN), regexp: /a+b/giu,
            map: new Map<unknown, unknown>([[1, 'one'], [{ k: 1 }, new Set([2, 3])]]),
            buffer, view: new Float64Array([1.5, -2]), dataView: new DataView(buffer, 2, 4), int64: new BigInt64Array([-1n]),
            blob: new Blob([new Uint8Array([4, 5])], { type: 'text/plain' }), file, error,
            boxes: [Object(1), Object('s'), Object(false), Object(5n)],
        }
        const { value: result, parts } = await roundTrip(value)
        expect(parts.slice(0, -1).every(part => part.byteLength === 7)).toBe(true)
        expect(result.undefinedValue).toBeUndefined()
        expect('undefinedValue' in result).toBe(true)
        expect(result.nan).toBeNaN()
        expect(result.inf).toBe(-Infinity)
        expect(Object.is(result.negativeZero, -0)).toBe(true)
        expect(result.big).toBe(-12345678901234567890n)
        expect(1 in result.holes).toBe(false)
        expect(result.holes.length).toBe(3)
        expect(result.date.getTime()).toBe(42)
        expect(result.invalidDate.getTime()).toBeNaN()
        expect(result.regexp.source).toBe('a+b')
        expect(result.regexp.flags).toBe('giu')
        expect([...result.map.keys()][0]).toBe(1)
        expect([...result.map.values()][1]).toEqual(new Set([2, 3]))
        expect(new Uint8Array(result.buffer)).toEqual(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]))
        expect(result.dataView.buffer).toBe(result.buffer)
        expect(result.dataView.byteOffset).toBe(2)
        expect(result.dataView.getUint8(0)).toBe(3)
        expect(result.view).toBeInstanceOf(Float64Array)
        expect([...result.view]).toEqual([1.5, -2])
        expect(result.int64[0]).toBe(-1n)
        expect(result.blob.type).toBe('text/plain')
        expect(new Uint8Array(await result.blob.arrayBuffer())).toEqual(new Uint8Array([4, 5]))
        expect(result.file).toBeInstanceOf(File)
        expect([result.file.name, result.file.type, result.file.lastModified]).toEqual(['name.bin', 'application/x-test', 1234])
        expect(new Uint8Array(await result.file.arrayBuffer())).toEqual(new Uint8Array([9, 8, 7]))
        expect(result.error).toBeInstanceOf(RangeError)
        expect(result.error.message).toBe('out of range')
        expect(result.boxes.map(box => typeof box)).toEqual(['object', 'object', 'object', 'object'])
        expect(result.boxes.map(box => box.valueOf())).toEqual([1, 's', false, 5n])
    })

    it('keeps cycles and shared buffers inside Map and Set with their references', async () => {
        const buffer = new ArrayBuffer(4)
        const map = new Map<string, unknown>()
        const set = new Set<unknown>()
        map.set('self', map).set('set', set).set('a', new Uint8Array(buffer, 0, 2)).set('b', new Uint16Array(buffer, 2, 1))
        set.add(set).add(map).add(buffer)
        const { value: result, parts } = await roundTrip({ map })
        expect(parts.reduce((size, part) => size + part.byteLength, 0)).toBe(4)
        const restored = result.map
        expect(restored.get('self')).toBe(restored)
        const restoredSet = restored.get('set') as Set<unknown>
        expect(restoredSet.has(restoredSet)).toBe(true)
        expect(restoredSet.has(restored)).toBe(true)
        const a = restored.get('a') as Uint8Array, b = restored.get('b') as Uint16Array
        expect(a.buffer).toBe(b.buffer)
        expect([...restoredSet].includes(a.buffer)).toBe(true)
    })

    it('stores many small buffers in one part and splits larger streams exactly', async () => {
        const small = Array.from({ length: 5 }, (_, index) => new Uint8Array([index]))
        const { parts: oneByte } = await roundTrip(small, 8)
        expect(oneByte.map(part => part.byteLength)).toEqual([5])
        const stream = new Uint8Array(17).map((_, index) => index)
        const { parts, value } = await roundTrip(stream, 8)
        expect(parts.map(part => part.byteLength)).toEqual([8, 8, 1])
        expect(value).toEqual(stream)
        // Blob and File bytes are taken from each part as it is read, so the reader may reuse its buffer.
        const blobs = encodeStructuredLocalPluginValue({ blob: new Blob([stream]), file: new File([stream], 'stream.bin') })
        const blobParts: Uint8Array[] = []
        for await (const part of structureParts(blobs, 8)) blobParts.push(part.slice())
        const reused = new Uint8Array(8)
        const read = await decodeStructuredLocalPluginValue<{ blob: Blob; file: File }>(
            parseStructureText(structureText(blobs, blobParts.map((_, index) => fakeHash(index)), 8), 8),
            async index => { reused.set(blobParts[index]); return reused.subarray(0, blobParts[index].byteLength) }, 8)
        expect(new Uint8Array(await read.blob.arrayBuffer())).toEqual(stream)
        expect(new Uint8Array(await read.file.arrayBuffer())).toEqual(stream)
        expect(read.file.name).toBe('stream.bin')
        const { parts: exact } = await roundTrip(new Uint8Array(16), 8)
        expect(exact.map(part => part.byteLength)).toEqual([8, 8])
        const { parts: none, text } = await roundTrip(new Date(0))
        expect(none).toEqual([])
        expect(parseStructureText(text, 7).parts).toEqual([])
    })

    it('keeps resizable buffers and length-tracking views', async () => {
        const Resizable = ArrayBuffer as unknown as new (size: number, options: { maxByteLength: number }) => ArrayBuffer & { resizable: boolean; maxByteLength: number }
        const buffer = new Resizable(4, { maxByteLength: 16 })
        const tracking = new Uint8Array(buffer, 1)
        const { value } = await roundTrip({ buffer, tracking })
        expect(value.buffer.resizable).toBe(true)
        expect(value.buffer.maxByteLength).toBe(16)
        expect(value.tracking.buffer).toBe(value.buffer)
        ;(value.buffer as unknown as { resize(size: number): void }).resize(8)
        expect(value.tracking.length).toBe(7)
    })

    it('writes the marker line and part references that are never asset keys', async () => {
        const { text } = await roundTrip(new Uint8Array(20))
        expect(isStructureText(text)).toBe(true)
        expect(text.startsWith(STRUCTURE_MARKER + '{"parts":["p:')).toBe(true)
        const body = JSON.parse(text.slice(STRUCTURE_MARKER.length))
        expect(Object.keys(body)).toEqual(['parts', 'bytes', 'nodes', 'root'])
        expect(body.parts.every((part: string) => /^p:[0-9a-f]{64}$/.test(part))).toBe(true)
        // The native side lists a value by the type and name of its first node.
        expect(body.nodes[0]).toMatchObject({ type: 'View', name: 'Uint8Array' })
        const boxed = JSON.parse(structureText(encodeStructuredLocalPluginValue(Object('s')), [], 8).slice(STRUCTURE_MARKER.length))
        expect(boxed.nodes[0]).toMatchObject({ type: 'Box', name: 'String' })
        expect(isStructureText(JSON.stringify(text))).toBe(false)
    })

    it('rejects unsupported values and damaged structures', async () => {
        expect(() => encodeStructuredLocalPluginValue({ fn() {} })).toThrow(TypeError)
        expect(() => encodeStructuredLocalPluginValue(Symbol('s'))).toThrow(TypeError)
        expect(() => encodeStructuredLocalPluginValue(new WeakMap())).toThrow(TypeError)
        const encoded = encodeStructuredLocalPluginValue(new Uint8Array(10))
        const parts: Uint8Array[] = []
        for await (const part of structureParts(encoded, 8)) parts.push(part)
        expect(() => structureText(encoded, [fakeHash(0)], 8)).toThrow()
        const structure = parseStructureText(structureText(encoded, [fakeHash(0), fakeHash(1)], 8), 8)
        await expect(decodeStructuredLocalPluginValue(structure, async index => index ? new Uint8Array(1) : parts[0], 8)).rejects.toThrow()
        expect(() => parseStructureText(STRUCTURE_MARKER + '{"parts":["' + fakeHash(0) + '"],"bytes":1,"nodes":[],"root":null}')).toThrow()
        expect(() => parseStructureText(STRUCTURE_MARKER + '{"parts":[],"bytes":1,"nodes":[],"root":null}')).toThrow()
        const mislabeled = structureText(encodeStructuredLocalPluginValue(Object('s')), [], 8).replace('"name":"String"', '"name":"Number"')
        await expect(decodeStructuredLocalPluginValue(parseStructureText(mislabeled, 8), async () => new Uint8Array(), 8)).rejects.toThrow()
    })
})
