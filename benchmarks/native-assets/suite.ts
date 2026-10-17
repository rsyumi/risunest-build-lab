import { invoke, type InvokeArgs, type InvokeOptions } from '@tauri-apps/api/core'
import { createCompleteAssetRepositoryBlobStore } from '../../src/ts/storage/assetRepository'
import { createNativeAssetObjectUrlResolver, createNativeDurableAssetWriteSessionFactory,
    createNativeImmutablePayloadCas, createNativeNewInlayImageEncoder, beginCasJob, prepareCasObject,
    releaseCasJob } from '../../src/ts/storage/nativeAssetRepository'
import { hashPayloadBytes } from '../../src/ts/storage/payloadCas'
import type { PersistentDataStore } from '../../src/ts/storage/persistentDataStore'
import { defaultInlayEncodeOptions } from '../../src/ts/storage/blobStore'
import { getAndroidBinaryCommitBridge, type AndroidBinaryCommitBridge } from '../../src/ts/storage/androidBinaryCommitBridge'
import { readNativePluginDeviceValue, writeNativePluginDeviceValues } from '../../src/ts/plugins/pluginDeviceTransfer'

export async function verifyConcurrentNativeByteWriters(store: PersistentDataStore) {
    const value = 'synthetic-byte-writer-🐿️'.repeat(32 * 1024)
    const source = Uint8Array.from({ length: 1024 * 1024 + 17 }, (_, index) => index % 256)
    const root = await store.readRoot()
    const owner = 'synthetic-byte-writers'
    const session = await beginCasJob('direct-asset-or-inlay-write')
    try {
        const [, , prepared] = await Promise.all([
            store.commit({ expectedRevision: root.revision, rootMutations: [{ type: 'set', key: 'username', value }] }),
            writeNativePluginDeviceValues(owner, [{ type: 'set', space: 'string', key: 'large', value }]),
            prepareCasObject(session, source, 'direct-object'),
        ])
        if ((await store.readRoot()).value.username !== value
            || await readNativePluginDeviceValue(owner, 'string', 'large') !== value) {
            throw new Error('Concurrent native DB/device write differs from its source')
        }
        const read = await createNativeImmutablePayloadCas().readObject(prepared.contentHash)
        if (prepared.contentHash !== await hashPayloadBytes(source) || !read || read.length !== source.length
            || read.some((byte, index) => byte !== source[index])) {
            throw new Error('Concurrent native asset write differs from its source')
        }
        return { passed: true, binaryBridge: Boolean(getAndroidBinaryCommitBridge()), assetBytes: source.length }
    } finally {
        await releaseCasJob(session, 'aborted')
    }
}

interface Sample {
    kind: string
    bytes: number
    iteration: number
    writeMs: number
    readMs: number
    calls: Record<string, number>
    sentPayloadBytes: number
    receivedPayloadBytes: number
    hash: string
    diagnostics?: { focused: boolean; visibility: string; commands: { command: string; start: number; duration: number }[] }
}

export async function runNativeAssetMeasurements(store: PersistentDataStore, diagnosticSmall = false) {
    const longTasks: number[] = []
    const observer = typeof PerformanceObserver !== 'undefined' && PerformanceObserver.supportedEntryTypes.includes('longtask')
        ? new PerformanceObserver(list => longTasks.push(...list.getEntries().map(entry => entry.duration))) : undefined
    observer?.observe({ type: 'longtask' })
    let calls: Record<string, number> = {}
    let sentPayloadBytes = 0
    let receivedPayloadBytes = 0
    let commandTimings: { command: string; start: number; duration: number }[] = []
    const observed = async <T>(command: string, operation: () => Promise<T>): Promise<T> => {
        const start = performance.now()
        try { return await operation() }
        finally { commandTimings.push({ command, start, duration: performance.now() - start }) }
    }
    const observedStore = diagnosticSmall ? new Proxy(store, {
        get(target, property) {
            const method = Reflect.get(target, property)
            return typeof method === 'function'
                ? (...args: unknown[]) => observed(`store.${String(property)}`, () => method.apply(target, args)) : method
        },
    }) : store
    const bridge = getAndroidBinaryCommitBridge()
    const bridgeDescriptor = Object.getOwnPropertyDescriptor(window, 'RisuNestCommit')
    if (bridge) {
        const observedBridge: AndroidBinaryCommitBridge = {
            get onmessage() { return bridge.onmessage },
            set onmessage(value) { bridge.onmessage = value },
            postMessage(packet) {
                calls.android_binary_part = (calls.android_binary_part ?? 0) + 1
                sentPayloadBytes += packet.byteLength - 40
                bridge.postMessage(packet)
            },
        }
        Object.defineProperty(window, 'RisuNestCommit', { configurable: true, value: observedBridge })
    }
    try {
    const measuredInvoke = async <T>(command: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> => {
        calls[command] = (calls[command] ?? 0) + 1
        const payload = args instanceof Uint8Array || args instanceof ArrayBuffer ? args
            : args && !Array.isArray(args) ? args.data : undefined
        sentPayloadBytes += payload instanceof Uint8Array || payload instanceof ArrayBuffer ? payload.byteLength
            : Array.isArray(payload) ? payload.length : 0
        const result = diagnosticSmall ? await observed(command, () => invoke<T>(command, args, options))
            : await invoke<T>(command, args, options)
        receivedPayloadBytes += result instanceof Uint8Array || result instanceof ArrayBuffer ? result.byteLength
            : Array.isArray(result) ? result.length : 0
        return result
    }
    const blobs = createCompleteAssetRepositoryBlobStore({
        store: observedStore, cas: createNativeImmutablePayloadCas(measuredInvoke),
        writeSessions: createNativeDurableAssetWriteSessionFactory(measuredInvoke),
        objectUrls: createNativeAssetObjectUrlResolver(),
        newInlayImages: createNativeNewInlayImageEncoder(measuredInvoke),
    })
    const samples: Sample[] = []
    let seed = 1
    const synthetic = (size: number) => {
        const value = Uint8Array.from({ length: size }, (_, index) => (index * 73 + (index >>> 8) + seed) % 256)
        if (size >= 4) new DataView(value.buffer).setUint32(0, seed)
        seed++
        return value
    }
    const measure = async (kind: 'asset' | 'inlay', source: Uint8Array, iteration: number) => {
        const key = kind === 'asset' ? `assets/synthetic-${seed++}.bin` : `synthetic-inlay-${seed++}`
        const expectedHash = await hashPayloadBytes(source)
        calls = {}
        sentPayloadBytes = 0
        receivedPayloadBytes = 0
        commandTimings = []
        const started = performance.now()
        if (kind === 'asset') await blobs.put(key, source, { kind, mime: 'application/octet-stream', name: 'Synthetic', ext: 'bin' })
        else await blobs.putNewInlayImage(key, source, { name: 'synthetic.png', options: { ...defaultInlayEncodeOptions, format: 'original' } })
        const written = performance.now()
        const read = await blobs.read(key)
        const finished = performance.now()
        if (!read || read.length !== source.length || read.some((byte, index) => byte !== source[index])) {
            throw new Error('Native asset bytes differ from the synthetic source')
        }
        const alias = await store.readAssetAlias({ kind, key })
        if (!alias || alias.value.objectHash !== expectedHash) throw new Error('Native asset alias has an incorrect hash')
        samples.push({ kind, bytes: source.length, iteration, writeMs: written - started, readMs: finished - written,
            calls: { ...calls }, sentPayloadBytes, receivedPayloadBytes, hash: expectedHash,
            ...(diagnosticSmall ? { diagnostics: { focused: document.hasFocus(), visibility: document.visibilityState,
                commands: commandTimings.map(command => ({ ...command, start: command.start - started })) } } : {}) })
    }
    const sizes = diagnosticSmall ? [0, 1024, 64 * 1024, 64 * 1024 + 1]
        : [0, 1024, 64 * 1024, 64 * 1024 + 1, 256 * 1024, 1024 * 1024, 8 * 1024 * 1024]
    for (const size of sizes) {
        for (let iteration = 0; iteration < (diagnosticSmall ? 31 : 6); iteration++) await measure('asset', synthetic(size), iteration)
    }
    if (diagnosticSmall) return { runtime: navigator.userAgent, samples,
        note: 'Synthetic small-transfer diagnostics with per-command timing; excludes inlays and many-asset writes.' }
    const canvas = document.createElement('canvas')
    canvas.width = 320
    canvas.height = 320
    const context = canvas.getContext('2d')!
    const pixels = context.createImageData(canvas.width, canvas.height)
    let random = 1
    for (let i = 0; i < pixels.data.length; i++) {
        random = Math.imul(random, 1664525) + 1013904223 | 0
        pixels.data[i] = i % 4 === 3 ? 255 : random >>> 24
    }
    context.putImageData(pixels, 0, 0)
    const png = await new Promise<Blob>((resolve, reject) => canvas.toBlob(blob => blob ? resolve(blob) : reject(new Error('Synthetic PNG encoding failed')), 'image/png'))
    const image = new Uint8Array(await png.arrayBuffer())
    for (let iteration = 0; iteration < 6; iteration++) await measure('inlay', image, iteration)
    const manyStarted = performance.now()
    for (let i = 0; i < 64; i++) await measure('asset', synthetic(96 * 1024), i)
    const manyAssetsMs = performance.now() - manyStarted
    if (observer) longTasks.push(...observer.takeRecords().map(entry => entry.duration))
    return { runtime: navigator.userAgent, samples, manyAssetsMs, maxLongTaskMs: observer ? Math.max(0, ...longTasks) : null,
        manyAssets: 64, note: 'Synthetic direct asset and original inlay writes through production adapters, including alias commit and verified readback. First iteration is warm-up; reads follow writes with a warm filesystem cache. Many-assets is a direct-write workload, not an import or restore. Call and byte counts cover the asset/media adapters; alias-store control calls are included in timing only. Long-task observations have a 50 ms floor.' }
    } finally {
        observer?.disconnect()
        if (bridgeDescriptor && bridge) Object.defineProperty(window, 'RisuNestCommit', bridgeDescriptor)
    }
}
