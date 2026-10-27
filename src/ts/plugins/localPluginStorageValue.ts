// Node layout adapted from PocketRisu's local plugin storage fix (#98).
// Values JSON reproduces exactly stay JSON text. Everything else is stored as
// the marker line and a node table; its bytes form one stream that is kept
// as 8 MiB parts outside the text.

export const STRUCTURE_MARKER = '#risunest-structured:1\n'
export const STRUCTURE_PART_BYTES = 8 * 1024 * 1024
const PART_PREFIX = 'p:'
const HASH = /^[0-9a-f]{64}$/

type ViewConstructor = {
    new(buffer: ArrayBuffer, byteOffset?: number, length?: number): ArrayBufferView
    BYTES_PER_ELEMENT?: number
}
const viewTypes: Record<string, ViewConstructor> = {
    Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array,
    Int32Array, Uint32Array, Float32Array, Float64Array,
    BigInt64Array, BigUint64Array, DataView,
}
const float16 = (globalThis as unknown as { Float16Array?: ViewConstructor }).Float16Array
if (float16) viewTypes.Float16Array = float16

const errorTypes: Record<string, ErrorConstructor> = {
    Error, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError,
}

// A boxed primitive is stored under its constructor's name and rebuilt from
// a primitive of the matching type.
const boxTypes = { Number: 'number', String: 'string', Boolean: 'boolean', BigInt: 'bigint' } as const

// RAB is newer than the project's ES2023 type library. Feature detection keeps
// ordinary buffers usable in browsers that do not implement resize().
type ResizableBuffer = ArrayBuffer & { resizable?: boolean; maxByteLength?: number; resize(size: number): void }
function isLengthTracking(view: ArrayBufferView, name: string): boolean {
    if (!(view.buffer as ResizableBuffer).resizable) return false
    // JavaScript exposes no tracking flag. Probe a native clone, never the
    // caller's buffer. Cloning also rejects views that are already out of bounds.
    const probe = structuredClone(view)
    const buffer = probe.buffer as ResizableBuffer
    if (probe.byteLength) {
        buffer.resize(probe.byteOffset)
        try {
            if (name === 'DataView') void probe.byteLength
            else (probe as unknown as Uint8Array).values()
            return true
        } catch { return false }
    }
    const width = viewTypes[name].BYTES_PER_ELEMENT ?? 1
    if ((buffer.maxByteLength ?? 0) - probe.byteOffset >= width) {
        buffer.resize(probe.byteOffset + width)
        return probe.byteLength !== 0
    }
    // With no room to grow, zero-length fixed/tracking views behave identically.
    return false
}

function unsupported(): never {
    throw new TypeError('getLocalPluginStorage cannot store this value')
}

/**
 * True when `JSON.parse(JSON.stringify(value))` gives back an equal value with
 * the same types and no lost identity, so the value can stay JSON text.
 */
export function isPlainLocalPluginValue(value: unknown): boolean {
    const seen = new WeakSet<object>()
    const visit = (item: unknown): boolean => {
        if (item === null || typeof item === 'string' || typeof item === 'boolean') return true
        if (typeof item === 'number') return Number.isFinite(item) && !Object.is(item, -0)
        if (typeof item !== 'object') return false
        if (seen.has(item)) return false
        seen.add(item)
        if (Array.isArray(item)) {
            if (Object.getPrototypeOf(item) !== Array.prototype || Object.keys(item).length !== item.length) return false
            for (let index = 0; index < item.length; index++) if (!visit(item[index])) return false
            return true
        }
        const prototype = Object.getPrototypeOf(item)
        if (prototype !== Object.prototype && prototype !== null) return false
        for (const key of Object.keys(item)) if (!visit((item as Record<string, unknown>)[key])) return false
        return true
    }
    return visit(value)
}

type StoredValue = null | boolean | string | number | { ref: number }
type Properties = [string, StoredValue][]
type StoredNode =
    | { type: 'Object'; properties: Properties }
    | { type: 'Array'; length: number; properties: Properties }
    | { type: 'Map'; entries: [StoredValue, StoredValue][] }
    | { type: 'Set'; values: StoredValue[] }
    | { type: 'ArrayBuffer'; offset: number; byteLength: number; maxByteLength?: number }
    | { type: 'View'; name: string; buffer: number; byteOffset: number; byteLength: number; lengthTracking?: boolean }
    | { type: 'Blob'; offset: number; byteLength: number; mimeType: string }
    | { type: 'File'; offset: number; byteLength: number; mimeType: string; name: string; lastModified: number }
    | { type: 'Date'; time: number | null }
    | { type: 'RegExp'; source: string; flags: string }
    | { type: 'Error'; name: string; message?: string; stack?: string }
    | { type: 'Primitive'; name: 'undefined' | 'bigint' | 'number'; value: string }
    | { type: 'Box'; name: keyof typeof boxTypes; value: StoredValue }

type ByteNode = Extract<StoredNode, { offset: number }>

/** A value that needs the structure form, before its parts are stored. */
export interface StructuredLocalPluginValue {
    readonly nodes: StoredNode[]
    readonly root: StoredValue
    readonly byteLength: number
    /** The byte stream in order: whole buffers and blobs, each once. */
    readonly chunks: readonly (Uint8Array | Blob)[]
}

/**
 * Builds the node table. Buffers and blobs are referenced, not copied; the
 * caller owns `value` and must not change it until the parts are stored.
 */
export function encodeStructuredLocalPluginValue(value: unknown): StructuredLocalPluginValue {
    const nodes: StoredNode[] = []
    const seen = new Map<object, number>()
    const chunks: (Uint8Array | Blob)[] = []
    let byteLength = 0
    const bytes = (chunk: Uint8Array | Blob, length: number) => {
        const offset = byteLength
        if (length > 0) chunks.push(chunk)
        byteLength += length
        return offset
    }
    function store(item: unknown): StoredValue {
        if (item === null || typeof item === 'string' || typeof item === 'boolean') return item as StoredValue
        if (typeof item === 'number' && Number.isFinite(item) && !Object.is(item, -0)) return item
        if (typeof item === 'function' || typeof item === 'symbol') unsupported()
        if (typeof item === 'object' && seen.has(item)) return { ref: seen.get(item)! }
        const ref = nodes.length
        // Register before visiting children so self references are valid.
        nodes.push(null as unknown as StoredNode)
        if (typeof item === 'object') seen.set(item, ref)
        let node: StoredNode
        const tag = Object.prototype.toString.call(item)
        if (item === undefined || typeof item === 'bigint' || typeof item === 'number') {
            node = { type: 'Primitive', name: typeof item as 'undefined' | 'bigint' | 'number',
                value: Object.is(item, -0) ? '-0' : String(item) }
        } else if (item instanceof Blob) {
            const offset = bytes(item, item.size)
            node = typeof File !== 'undefined' && item instanceof File
                ? { type: 'File', offset, byteLength: item.size, mimeType: item.type, name: item.name, lastModified: item.lastModified }
                : { type: 'Blob', offset, byteLength: item.size, mimeType: item.type }
        } else if (item instanceof ArrayBuffer) {
            const offset = bytes(new Uint8Array(item), item.byteLength)
            node = { type: 'ArrayBuffer', offset, byteLength: item.byteLength }
            if ((item as ResizableBuffer).resizable) node.maxByteLength = (item as ResizableBuffer).maxByteLength
        } else if (ArrayBuffer.isView(item)) {
            const name = tag.slice(8, -1)
            if (!Object.hasOwn(viewTypes, name) || !(item.buffer instanceof ArrayBuffer)) unsupported()
            node = { type: 'View', name, buffer: (store(item.buffer) as { ref: number }).ref,
                byteOffset: item.byteOffset, byteLength: item.byteLength }
            if ((item.buffer as ResizableBuffer).resizable) node.lengthTracking = isLengthTracking(item, name)
        } else if (item instanceof Date) {
            node = { type: 'Date', time: Number.isNaN(item.getTime()) ? null : item.getTime() }
        } else if (item instanceof RegExp) {
            node = { type: 'RegExp', source: item.source, flags: item.flags }
        } else if (item instanceof Map) {
            const entries: [StoredValue, StoredValue][] = []
            for (const [key, entry] of item) entries.push([store(key), store(entry)])
            node = { type: 'Map', entries }
        } else if (item instanceof Set) {
            const values: StoredValue[] = []
            for (const entry of item) values.push(store(entry))
            node = { type: 'Set', values }
        } else if (tag === '[object Error]') {
            // The structured clone algorithm keeps these fields and normalizes
            // an unknown name to Error.
            const error = item as Error
            const name = Object.hasOwn(errorTypes, error.name) ? error.name : 'Error'
            node = { type: 'Error', name }
            const message = Object.getOwnPropertyDescriptor(error, 'message')
            if (message && 'value' in message) node.message = String(message.value)
            if (typeof error.stack === 'string') node.stack = error.stack
        } else if (['[object Number]', '[object Boolean]', '[object String]', '[object BigInt]'].includes(tag)) {
            node = { type: 'Box', name: tag.slice(8, -1) as keyof typeof boxTypes, value: store((item as { valueOf(): unknown }).valueOf()) }
        } else if (Array.isArray(item) || tag === '[object Object]') {
            const record = item as Record<string, unknown>
            const properties = Object.keys(record).map((key): [string, StoredValue] => [key, store(record[key])])
            node = Array.isArray(item) ? { type: 'Array', length: item.length, properties } : { type: 'Object', properties }
        } else {
            unsupported()
        }
        nodes[ref] = node
        return { ref }
    }
    const root = store(value)
    return { nodes, root, byteLength, chunks }
}

export function structurePartCount(byteLength: number, partBytes = STRUCTURE_PART_BYTES): number {
    return Math.ceil(byteLength / partBytes)
}

/** The stored text, with `parts` holding each part's SHA-256 in hex. */
export function structureText(value: StructuredLocalPluginValue, parts: readonly string[], partBytes = STRUCTURE_PART_BYTES): string {
    if (parts.length !== structurePartCount(value.byteLength, partBytes) || parts.some(part => !HASH.test(part))) {
        throw new Error('Invalid local plugin storage parts')
    }
    return STRUCTURE_MARKER + JSON.stringify({
        parts: parts.map(part => PART_PREFIX + part), bytes: value.byteLength, nodes: value.nodes, root: value.root,
    })
}

/** Yields the byte stream as parts of exactly `partBytes` except the last. */
export async function* structureParts(value: StructuredLocalPluginValue, partBytes = STRUCTURE_PART_BYTES): AsyncGenerator<Uint8Array> {
    let part = new Uint8Array(Math.min(partBytes, value.byteLength))
    let filled = 0, remaining = value.byteLength
    for (const chunk of value.chunks) {
        const size = chunk instanceof Blob ? chunk.size : chunk.byteLength
        for (let offset = 0; offset < size;) {
            const take = Math.min(size - offset, part.length - filled)
            const piece = chunk instanceof Blob
                ? new Uint8Array(await chunk.slice(offset, offset + take).arrayBuffer())
                : chunk.subarray(offset, offset + take)
            if (piece.byteLength !== take) throw new Error('Local plugin storage value changed while it was stored')
            part.set(piece, filled)
            filled += take; offset += take; remaining -= take
            if (filled === part.length) {
                yield part
                part = new Uint8Array(Math.min(partBytes, remaining))
                filled = 0
            }
        }
    }
    if (remaining !== 0 || filled !== 0) throw new Error('Local plugin storage value changed while it was stored')
}

export function isStructureText(text: string): boolean {
    return text.startsWith(STRUCTURE_MARKER)
}

export interface ParsedStructure {
    readonly parts: string[]
    readonly byteLength: number
    readonly nodes: StoredNode[]
    readonly root: StoredValue
}

const invalid = (): never => { throw new Error('Invalid local plugin storage structure') }
const natural = (n: unknown): n is number => typeof n === 'number' && Number.isSafeInteger(n) && n >= 0

export function parseStructureText(text: string, partBytes = STRUCTURE_PART_BYTES): ParsedStructure {
    if (!isStructureText(text)) invalid()
    const body: unknown = JSON.parse(text.slice(STRUCTURE_MARKER.length))
    if (!body || typeof body !== 'object' || Array.isArray(body)) return invalid()
    const { parts, bytes, nodes, root } = body as Record<string, unknown>
    if (!Array.isArray(parts) || !natural(bytes) || !Array.isArray(nodes)) return invalid()
    if (parts.length !== structurePartCount(bytes, partBytes)) return invalid()
    const hashes = parts.map(part => typeof part === 'string' && part.startsWith(PART_PREFIX) && HASH.test(part.slice(2))
        ? part.slice(2) : invalid())
    return { parts: hashes, byteLength: bytes, nodes: nodes as StoredNode[], root: root as StoredValue }
}

/**
 * Rebuilds the value. `readPart(index)` returns that part's bytes; parts are
 * read in order and each byte node copies its range out of them.
 */
export async function decodeStructuredLocalPluginValue<T>(
    structure: ParsedStructure,
    readPart: (index: number) => Promise<Uint8Array>,
    partBytes = STRUCTURE_PART_BYTES,
): Promise<T> {
    const { nodes, root, byteLength } = structure
    const values: unknown[] = new Array(nodes.length)
    function read(value: StoredValue): any {
        if (value === null || typeof value === 'string' || typeof value === 'boolean'
            || (typeof value === 'number' && Number.isFinite(value) && !Object.is(value, -0))) return value
        if (!value || typeof value !== 'object' || !natural(value.ref) || value.ref >= nodes.length) return invalid()
        return values[value.ref]
    }

    // Byte nodes cover the stream in node order without gaps.
    const byteNodes: [number, ByteNode][] = []
    let streamOffset = 0
    for (const [index, node] of nodes.entries()) {
        if (!node || typeof node !== 'object') return invalid()
        if (node.type === 'ArrayBuffer' || node.type === 'Blob' || node.type === 'File') {
            if (!natural(node.offset) || !natural(node.byteLength) || node.offset !== streamOffset) return invalid()
            streamOffset += node.byteLength
            byteNodes.push([index, node])
        }
    }
    if (streamOffset !== byteLength) return invalid()

    let partIndex = 0, partStart = 0
    let part: Uint8Array = new Uint8Array()
    const take = async (length: number, sink: (piece: Uint8Array) => void): Promise<void> => {
        while (length > 0) {
            if (partStart === part.length) {
                if (partIndex >= structure.parts.length) return invalid()
                const expected = Math.min(partBytes, byteLength - partIndex * partBytes)
                part = await readPart(partIndex++)
                if (part.byteLength !== expected) throw new Error('Local plugin storage part is damaged')
                partStart = 0
            }
            const count = Math.min(length, part.length - partStart)
            sink(part.subarray(partStart, partStart + count))
            partStart += count; length -= count
        }
    }

    for (const [index, node] of byteNodes) {
        if (node.type === 'ArrayBuffer') {
            let buffer: ArrayBuffer
            if (node.maxByteLength === undefined) buffer = new ArrayBuffer(node.byteLength)
            else {
                if (!natural(node.maxByteLength) || node.maxByteLength < node.byteLength) return invalid()
                const BufferConstructor = ArrayBuffer as unknown as {
                    new(size: number, options: { maxByteLength: number }): ResizableBuffer
                }
                buffer = new BufferConstructor(node.byteLength, { maxByteLength: node.maxByteLength })
                if (!(buffer as ResizableBuffer).resizable) throw new Error('Resizable ArrayBuffer is not supported here')
            }
            // Each part is copied in as it is read, so a large buffer is not held twice.
            const target = new Uint8Array(buffer)
            let offset = 0
            await take(node.byteLength, piece => { target.set(piece, offset); offset += piece.byteLength })
            values[index] = buffer
        } else {
            if (typeof node.mimeType !== 'string') return invalid()
            // Each part is copied into the Blob as it is read, so a large Blob is not held twice.
            let blob = new Blob([])
            await take(node.byteLength, piece => { blob = new Blob([blob, piece as BlobPart]) })
            if (node.type === 'File') {
                if (typeof node.name !== 'string' || !Number.isFinite(node.lastModified)) return invalid()
                values[index] = new File([blob], node.name, { type: node.mimeType, lastModified: node.lastModified })
            } else values[index] = new Blob([blob], { type: node.mimeType })
        }
    }
    if (partIndex !== structure.parts.length || partStart !== part.length) return invalid()

    for (const [index, node] of nodes.entries()) {
        switch (node.type) {
            case 'Object': values[index] = {}; break
            case 'Array':
                if (!natural(node.length) || node.length > 0xffffffff) return invalid()
                values[index] = new Array(node.length); break
            case 'Map': values[index] = new Map(); break
            case 'Set': values[index] = new Set(); break
            case 'Date':
                if (node.time !== null && !Number.isFinite(node.time)) return invalid()
                values[index] = new Date(node.time === null ? NaN : node.time); break
            case 'RegExp':
                if (typeof node.source !== 'string' || typeof node.flags !== 'string') return invalid()
                values[index] = new RegExp(node.source, node.flags); break
            case 'Error': {
                if (!Object.hasOwn(errorTypes, node.name)) return invalid()
                if (node.message !== undefined && typeof node.message !== 'string') return invalid()
                if (node.stack !== undefined && typeof node.stack !== 'string') return invalid()
                const error = new errorTypes[node.name]()
                if (node.message !== undefined) Object.defineProperty(error, 'message', { value: node.message, writable: true, configurable: true })
                if (node.stack !== undefined) Object.defineProperty(error, 'stack', { value: node.stack, writable: true, configurable: true })
                else delete (error as { stack?: string }).stack
                values[index] = error
                break
            }
            case 'Primitive':
                if (node.name === 'undefined' && node.value === 'undefined') values[index] = undefined
                else if (node.name === 'bigint' && /^-?\d+$/.test(node.value)) values[index] = BigInt(node.value)
                else if (node.name === 'number' && ['NaN', 'Infinity', '-Infinity', '-0'].includes(node.value)) values[index] = Number(node.value)
                else return invalid()
                break
            case 'ArrayBuffer': case 'Blob': case 'File': case 'View': case 'Box': break
            default: return invalid()
        }
    }
    // Buffers and primitives exist before the views and boxes that use them.
    for (const [index, node] of nodes.entries()) {
        if (node.type === 'View') {
            if (!Object.hasOwn(viewTypes, node.name) || !natural(node.buffer)
                || nodes[node.buffer]?.type !== 'ArrayBuffer' || !natural(node.byteOffset) || !natural(node.byteLength)) return invalid()
            const buffer = values[node.buffer] as ArrayBuffer
            const ctor = viewTypes[node.name]
            const width = ctor.BYTES_PER_ELEMENT ?? 1
            if (node.byteOffset > buffer.byteLength || node.byteLength > buffer.byteLength - node.byteOffset
                || node.byteOffset % width || node.byteLength % width) return invalid()
            if (node.lengthTracking !== undefined && typeof node.lengthTracking !== 'boolean') return invalid()
            if (node.lengthTracking && !(buffer as ResizableBuffer).resizable) return invalid()
            values[index] = node.lengthTracking
                ? new ctor(buffer, node.byteOffset) : new ctor(buffer, node.byteOffset, node.byteLength / width)
            if ((values[index] as ArrayBufferView).byteLength !== node.byteLength) return invalid()
        } else if (node.type === 'Box') {
            const primitive = read(node.value)
            if (!Object.hasOwn(boxTypes, node.name) || typeof primitive !== boxTypes[node.name]) return invalid()
            values[index] = Object(primitive)
        }
    }
    for (const [index, node] of nodes.entries()) {
        if (node.type === 'Object' || node.type === 'Array') {
            if (!Array.isArray(node.properties)) return invalid()
            for (const property of node.properties) {
                if (!Array.isArray(property) || property.length !== 2 || typeof property[0] !== 'string'
                    || (node.type === 'Array' && property[0] === 'length')) return invalid()
                Object.defineProperty(values[index], property[0], {
                    value: read(property[1]), writable: true, enumerable: true, configurable: true,
                })
            }
        } else if (node.type === 'Map') {
            if (!Array.isArray(node.entries)) return invalid()
            const map = values[index] as Map<unknown, unknown>
            for (const entry of node.entries) {
                if (!Array.isArray(entry) || entry.length !== 2) return invalid()
                map.set(read(entry[0]), read(entry[1]))
            }
        } else if (node.type === 'Set') {
            if (!Array.isArray(node.values)) return invalid()
            for (const value of node.values) (values[index] as Set<unknown>).add(read(value))
        }
    }
    return read(root) as T
}
