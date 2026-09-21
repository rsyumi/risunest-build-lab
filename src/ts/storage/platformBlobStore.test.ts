import { describe, expect, test, vi } from 'vitest'
import {
    configureActiveBlobStore,
    createBackedBlobStore,
    createBrowserBlobBackend,
    createGatedBlobStore,
    createOpfsBlobBackend,
    createStorageBlobStore,
    createTauriCasObjectUrl,
    createTauriNativeMediaUrl,
    createNativeMediaEndpointProvider,
    getBlobStore,
    physicalBlobKeys,
    readBlobForFacade,
} from './platformBlobStore'
import {
    createInRealmStorageLockManager,
    createStorageMutationGate,
    type StorageMutationGate,
} from './storageMutationGate'

function memoryBackend() {
    const values = new Map<string, Uint8Array>()
    return {
        values,
        backend: {
            write: async (key: string, value: Uint8Array) =>
                void values.set(key, value.slice()),
            read: async (key: string) => values.get(key)?.slice() ?? null,
            keys: async () => [...values.keys()],
            remove: async (key: string) => void values.delete(key),
        },
    }
}

function deferred() {
    let resolve!: () => void
    const promise = new Promise<void>((done) => {
        resolve = done
    })
    return { promise, resolve }
}

describe('platform BlobStore', () => {
    const endpoint = 'http://127.0.0.1:12345/0123456789abcdef0123456789abcdef/'
    test('encodes physical keys into a scoped loopback endpoint without AppData paths', () => {
        expect(
            createTauriNativeMediaUrl('assets/folder/photo.jpg', endpoint),
        ).toBe(`${endpoint}6173736574732f666f6c6465722f70686f746f2e6a7067`)
        expect(() =>
            createTauriNativeMediaUrl('assets/a', 'https://outside.invalid/'),
        ).toThrow('endpoint')
    })

    test('coalesces endpoint requests and retries failed native initialization', async () => {
        const invoke = vi
            .fn()
            .mockRejectedValueOnce(new Error('unavailable'))
            .mockResolvedValue(endpoint)
        const get = createNativeMediaEndpointProvider(invoke)
        await expect(get()).rejects.toThrow('unavailable')
        expect(await Promise.all([get(), get(), get()])).toEqual([
            endpoint,
            endpoint,
            endpoint,
        ])
        expect(invoke).toHaveBeenCalledTimes(2)
        expect(invoke).toHaveBeenLastCalledWith('native_media_base_url')
    })

    test('builds an exact native CAS URL from the alias hash, MIME, and size', () => {
        const hash = 'ab'.repeat(32)

        expect(
            createTauriCasObjectUrl(
                {
                    contentHash: hash,
                    mime: 'image/custom; profile=exact',
                    size: 42,
                },
                endpoint,
            ),
        ).toBe(
            `${endpoint}${Buffer.from(`assets/objects/ab/${hash.slice(2)}`).toString('hex')}` +
                '?mime=image%2Fcustom%3B+profile%3Dexact&size=42',
        )
        expect(() =>
            createTauriCasObjectUrl(
                {
                    contentHash: hash,
                    mime: 'image/png\0text/html',
                    size: 42,
                },
                endpoint,
            ),
        ).toThrow('MIME')
    })

    test('gates writes and removals while leaving reads ungated', async () => {
        const { backend } = memoryBackend()
        const events: string[] = []
        const gate = {
            async runWrite<T>(operation: () => Promise<T>) {
                events.push('lock')
                return operation()
            },
            async runKeyedWrite<T>(_key: string, operation: () => Promise<T>) {
                events.push('lock')
                return operation()
            },
            async runTransition<T>(operation: () => Promise<T>) { return operation() },
        }
        const store = createGatedBlobStore(createBackedBlobStore(backend), gate)
        const metadata = { kind: 'asset' as const, mime: 'application/octet-stream', name: 'a', ext: '' }

        await store.put('assets/a', new Uint8Array([1]), metadata)
        expect(events).toEqual(['lock'])
        expect(await store.read('assets/a')).toEqual(new Uint8Array([1]))

        events.length = 0
        await store.read('assets/a')
        await store.stat('assets/a')
        await store.list()
        await store.resolveUrl('assets/a')
        expect(events).toEqual([])

        await store.remove('assets/a')
        expect(events).toEqual(['lock'])
        expect(await store.read('assets/a')).toBeNull()
    })

    test('gates a supplied authoritative store by default', async () => {
        const runKeyedWrite = vi.fn(async (
            _key: string,
            operation: () => Promise<unknown>,
        ) => operation())
        const gate: StorageMutationGate = {
            runWrite: (operation) => operation(),
            runKeyedWrite<T>(key: string, operation: () => Promise<T>) {
                return runKeyedWrite(key, operation) as Promise<T>
            },
            runTransition: (operation) => operation(),
        }
        const authoritative = createBackedBlobStore(memoryBackend().backend)
        configureActiveBlobStore(gate, authoritative)

        await getBlobStore().put('assets/a', new Uint8Array([1]), {
            kind: 'asset', mime: 'application/octet-stream', name: 'a', ext: '',
        })

        expect(runKeyedWrite).toHaveBeenCalledTimes(1)
    })

    test('does not gate an already-guarded authoritative store a second time', async () => {
        const runKeyedWrite = vi.fn(async (
            _key: string,
            operation: () => Promise<unknown>,
        ) => operation())
        const gate: StorageMutationGate = {
            runWrite: (operation) => operation(),
            runKeyedWrite<T>(key: string, operation: () => Promise<T>) {
                return runKeyedWrite(key, operation) as Promise<T>
            },
            runTransition: (operation) => operation(),
        }
        const authoritative = createBackedBlobStore(memoryBackend().backend)
        configureActiveBlobStore(gate, authoritative, { alreadyGuarded: true })

        await getBlobStore().put('assets/a', new Uint8Array([1]), {
            kind: 'asset', mime: 'application/octet-stream', name: 'a', ext: '',
        })

        expect(runKeyedWrite).not.toHaveBeenCalled()
    })

    test('owns blob bytes before a deferred write gate proceeds', async () => {
        const { backend } = memoryBackend()
        let release!: () => void
        const blocked = new Promise<void>((resolve) => { release = resolve })
        const gate = {
            async runWrite<T>(operation: () => Promise<T>) { await blocked; return operation() },
            async runKeyedWrite<T>(_key: string, operation: () => Promise<T>) { await blocked; return operation() },
            async runTransition<T>(operation: () => Promise<T>) { return operation() },
        }
        const backing = createBackedBlobStore(backend)
        const store = createGatedBlobStore(backing, gate)
        const source = new Uint8Array([1])
        const pending = store.put('assets/a', source, {
            kind: 'asset', mime: 'application/octet-stream', name: 'a', ext: '',
        })
        source[0] = 9
        release()

        await pending
        expect(await backing.read('assets/a')).toEqual(new Uint8Array([1]))
    })

    test('gates native Inlay image writes and owns their inputs', async () => {
        let release!: () => void
        const blocked = new Promise<void>((resolve) => { release = resolve })
        const optimized = vi.fn(async (key: string, data: Uint8Array, input: { name: string }) => ({
            key, kind: 'inlay' as const, size: data.byteLength, mime: 'image/webp',
            name: input.name, ext: 'webp', inlayType: 'image' as const, width: 1, height: 1,
        }))
        const store = {
            ...createBackedBlobStore(memoryBackend().backend),
            putNewInlayImage: optimized,
        }
        const gated = createGatedBlobStore(store, {
            async runWrite<T>(operation: () => Promise<T>) { await blocked; return operation() },
            async runKeyedWrite<T>(_key: string, operation: () => Promise<T>) { await blocked; return operation() },
            async runTransition<T>(operation: () => Promise<T>) { return operation() },
        })
        const source = Uint8Array.of(4, 5)
        const input = { name: 'original.png' }

        const pending = gated.putNewInlayImage!('owned', source, input)
        source[0] = 8
        input.name = 'mutated.png'
        release()
        await pending

        expect(optimized).toHaveBeenCalledWith('owned', Uint8Array.of(4, 5), { name: 'original.png' })
    })

    test('prevents native optimization from interleaving with opaque restore for the same Inlay', async () => {
        const opaqueRelease = deferred()
        const events: string[] = []
        const store = {
            ...createBackedBlobStore(memoryBackend().backend),
            async put(key: string, data: Uint8Array, metadata: any) {
                events.push('opaque-start')
                await opaqueRelease.promise
                events.push('opaque-end')
                return { ...metadata, key, size: data.byteLength }
            },
            async putNewInlayImage(key: string, data: Uint8Array, input: { name: string }) {
                events.push('optimized')
                return {
                    key, kind: 'inlay' as const, size: data.byteLength, mime: 'image/webp',
                    name: input.name, ext: 'webp', inlayType: 'image' as const, width: 1, height: 1,
                }
            },
        }
        const gated = createGatedBlobStore(
            store,
            createStorageMutationGate({ locks: createInRealmStorageLockManager() }),
        )

        const opaque = gated.put('same-id', Uint8Array.of(1), {
            kind: 'inlay', mime: 'image/png', name: 'restore.png', ext: 'png', inlayType: 'image',
        })
        const optimized = gated.putNewInlayImage!('same-id', Uint8Array.of(2), { name: 'new.png' })

        await Promise.resolve()
        await Promise.resolve()
        expect(events).toEqual(['opaque-start'])
        opaqueRelease.resolve()
        await Promise.all([opaque, optimized])
        expect(events).toEqual(['opaque-start', 'opaque-end', 'optimized'])
    })

    test('maps web logical paths and encodes raw inlay ids', () => {
        expect(physicalBlobKeys('assets/photo.jpg')).toEqual({
            payload: 'assets/photo.jpg',
            metadata: 'blobstore/metadata/6173736574732f70686f746f2e6a7067.json',
        })
        expect(physicalBlobKeys('../raw')).toEqual({
            payload: 'blobstore/inlays/2e2e2f726177.bin',
            metadata: 'blobstore/metadata/2e2e2f726177.json',
        })
    })





    test('refuses AccountStorage before invoking it', () => {
        const storage = {
            setItem: async () => { throw new Error('must not run') },
            getItem: async () => { throw new Error('must not run') },
            keys: async () => { throw new Error('must not run') },
            removeItem: async () => { throw new Error('must not run') },
        }
        expect(() => createStorageBlobStore({ storage, isAccount: true })).toThrow(TypeError)
    })

    test('OPFS keys skip foreign non-hex names in the shared root', async () => {
        const entries = [
            { kind: 'file', name: Buffer.from('assets/a.png', 'utf-8').toString('hex') },
            { kind: 'file', name: 'coldstorage_3f6b.json' },
            { kind: 'file', name: 'ABCDEF' },
            { kind: 'file', name: 'abc' },
            { kind: 'directory', name: '6162' },
        ]
        const directory = {
            values: async function* () { yield* entries },
        } as unknown as FileSystemDirectoryHandle
        const backend = createOpfsBlobBackend(directory)

        expect(await backend.keys()).toEqual(['assets/a.png'])
    })

    test('OPFS bounded reads use File.slice', async () => {
        const sliceCalls: [number, number][] = []
        const file = new File([new Uint8Array([0, 1, 2, 3])], 'value')
        const originalSlice = file.slice.bind(file)
        file.slice = ((start?: number, end?: number) => {
            sliceCalls.push([start!, end!])
            return originalSlice(start, end)
        }) as typeof file.slice
        const directory = {
            getFileHandle: async () => ({ getFile: async () => file }),
        } as unknown as FileSystemDirectoryHandle
        const backend = createOpfsBlobBackend(directory)
        expect(await backend.readRange!('assets/a', { start: 1, endExclusive: 3 })).toEqual(new Uint8Array([1, 2]))
        expect(sliceCalls).toEqual([[1, 3]])
    })

    test('OPFS write abort preserves the write failure and does not close an errored stream', async () => {
        const writeError = new DOMException('quota exhausted', 'QuotaExceededError')
        const abort = vi.fn(async () => undefined)
        const close = vi.fn(async () => undefined)
        const directory = {
            getFileHandle: async () => ({
                createWritable: async () => ({
                    write: async () => { throw writeError },
                    abort,
                    close,
                }),
            }),
        } as unknown as FileSystemDirectoryHandle
        const backend = createOpfsBlobBackend(directory)

        await expect(backend.write('assets/a', Uint8Array.of(1))).rejects.toBe(writeError)
        expect(abort).toHaveBeenCalledWith(writeError)
        expect(close).not.toHaveBeenCalled()
    })

    test('OPFS write reports an abort failure without replacing the write failure', async () => {
        const writeError = new Error('write failed')
        const abortError = new Error('abort failed')
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        const directory = {
            getFileHandle: async () => ({
                createWritable: async () => ({
                    write: async () => { throw writeError },
                    abort: async () => { throw abortError },
                    close: vi.fn(),
                }),
            }),
        } as unknown as FileSystemDirectoryHandle
        const backend = createOpfsBlobBackend(directory)

        await expect(backend.write('assets/a', Uint8Array.of(1))).rejects.toBe(writeError)
        expect(consoleError).toHaveBeenCalledWith('OPFS blob write abort failed', abortError)
        consoleError.mockRestore()
    })

    test('production browser selection keeps OPFS reads bounded', async () => {
        const getItem = vi.fn(async () => new Uint8Array([0, 1, 2, 3]))
        const selected = {
            blobStorageKind: 'opfs' as const,
            setItem: vi.fn(async () => undefined),
            getItem,
            keys: vi.fn(async () => []),
            removeItem: vi.fn(async () => undefined),
        }
        const file = new File([new Uint8Array([0, 1, 2, 3])], 'value')
        const directory = {
            getFileHandle: async () => ({ getFile: async () => file }),
        } as unknown as FileSystemDirectoryHandle

        const backend = await createBrowserBlobBackend(selected, async () => directory)

        expect(await backend.readRange!('assets/a', { start: 1, endExclusive: 3 })).toEqual(new Uint8Array([1, 2]))
        expect(getItem).not.toHaveBeenCalled()
    })

    test('rejects an unconfigured browser storage provider', async () => {
        await expect(createBrowserBlobBackend(null)).rejects.toThrow(
            'Blob storage provider is not configured',
        )
    })

    test('facade rejects missing native blobs', async () => {
        const store = createBackedBlobStore(memoryBackend().backend)

        await expect(readBlobForFacade(store, 'assets/missing', true)).rejects.toThrow('Missing asset')
    })

    test('facade preserves nullable browser reads', async () => {
        const store = createBackedBlobStore(memoryBackend().backend)

        await expect(readBlobForFacade(store, 'assets/missing', false)).resolves.toBeNull()
    })
})
