import { invoke } from '@tauri-apps/api/core'

export interface ImageGeometry {
    contentHash: string
    width: number
    height: number
}

export interface ImageGeometryStore {
    read(hashes: readonly string[]): Promise<ImageGeometry[]>
    write(values: readonly ImageGeometry[]): Promise<void>
    compute(contentHash: string): Promise<ImageGeometry | null>
}

export function validImageDimensions(width: unknown, height: unknown): boolean {
    return typeof width === 'number' && Number.isInteger(width) && width > 0 && width <= 0xffffffff &&
        typeof height === 'number' && Number.isInteger(height) && height > 0 && height <= 0xffffffff
}

function validate(value: ImageGeometry): void {
    if (!/^[a-f0-9]{64}$/.test(value.contentHash) || !validImageDimensions(value.width, value.height)) {
        throw new TypeError('Invalid image geometry')
    }
}

export function createNativeImageGeometryStore(
    command: typeof invoke = invoke,
    getEpoch: () => number = () => 0,
): ImageGeometryStore {
    const pending = new Map<string, { value: ImageGeometry; epoch: number; promise: Promise<void>; resolve(): void; reject(error: unknown): void }>()
    const reads = new Map<string, { epoch: number; promise: Promise<ImageGeometry | undefined>; resolve(value?: ImageGeometry): void; reject(error: unknown): void }>()
    let scheduled = false
    let reading = false
    const changed = () => new DOMException('Image storage changed', 'AbortError')
    async function flushReads() {
        const batch = [...reads.entries()].slice(0, 64)
        try {
            if (batch.some(([, entry]) => entry.epoch !== getEpoch())) throw changed()
            const values = await command<ImageGeometry[]>('pds_read_image_geometry', { hashes: batch.map(([hash]) => hash) })
            if (batch.some(([, entry]) => entry.epoch !== getEpoch())) throw changed()
            const byHash = new Map<string, ImageGeometry>()
            for (const value of values) {
                validate(value)
                if (!batch.some(([hash]) => hash === value.contentHash) || byHash.has(value.contentHash)) throw new TypeError('Mismatched image geometry')
                byHash.set(value.contentHash, value)
            }
            for (const [hash, entry] of batch) entry.resolve(byHash.get(hash))
        } catch (error) {
            for (const [, entry] of batch) entry.reject(error)
        } finally {
            for (const [hash] of batch) reads.delete(hash)
            if (reads.size) void flushReads()
            else reading = false
        }
    }
    async function flush() {
        const batch = [...pending.values()].slice(0, 64)
        try {
            if (batch.some(entry => entry.epoch !== getEpoch())) throw changed()
            await command('pds_write_image_geometry', { values: batch.map(entry => entry.value) })
            for (const entry of batch) entry.resolve()
        } catch (error) {
            for (const entry of batch) entry.reject(error)
        } finally {
            for (const entry of batch) pending.delete(entry.value.contentHash)
            if (pending.size) void flush()
            else scheduled = false
        }
    }
    return {
        async read(hashes) {
            const requests = [...new Set(hashes)].map(hash => {
                const existing = reads.get(hash)
                if (existing) return existing.epoch === getEpoch() ? existing.promise : Promise.reject(changed())
                let resolve!: (value?: ImageGeometry) => void
                let reject!: (error: unknown) => void
                const promise = new Promise<ImageGeometry | undefined>((yes, no) => { resolve = yes; reject = no })
                reads.set(hash, { epoch: getEpoch(), promise, resolve, reject })
                return promise
            })
            if (reads.size && !reading) {
                reading = true
                queueMicrotask(() => void flushReads())
            }
            return (await Promise.all(requests)).filter((value): value is ImageGeometry => !!value)
        },
        async write(values) {
            for (const value of values) validate(value)
            const writes = values.map(value => {
                const existing = pending.get(value.contentHash)
                if (existing) {
                    if (existing.epoch !== getEpoch()) return Promise.reject(changed())
                    if (existing.value.width !== value.width || existing.value.height !== value.height) return Promise.reject(new TypeError('Conflicting image geometry'))
                    return existing.promise
                }
                let resolve!: () => void
                let reject!: (error: unknown) => void
                const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no })
                pending.set(value.contentHash, { value: { ...value }, epoch: getEpoch(), promise, resolve, reject })
                return promise
            })
            if (pending.size && !scheduled) {
                scheduled = true
                queueMicrotask(() => void flush())
            }
            await Promise.all(writes)
        },
        async compute(contentHash) {
            const epoch = getEpoch()
            const value = await command<ImageGeometry | null>('pds_compute_image_geometry', { contentHash })
            if (epoch !== getEpoch()) throw changed()
            if (value) {
                validate(value)
                if (value.contentHash !== contentHash) throw new TypeError('Mismatched computed image geometry')
            }
            return value
        },
    }
}
