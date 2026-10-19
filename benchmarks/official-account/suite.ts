import { invoke } from '@tauri-apps/api/core'
import type { InvokeArgs, InvokeOptions } from '@tauri-apps/api/core'
import { Buffer } from 'buffer'
import type { Database } from '../../src/ts/storage/database.svelte'
import type { AssetAlias, PersistentDataStore } from '../../src/ts/storage/persistentDataStore'
import { createCompleteAssetRepositoryBlobStore } from '../../src/ts/storage/assetRepository'
import { createNativeAssetObjectUrlResolver, createNativeDurableAssetWriteSessionFactory,
    createNativeImmutablePayloadCas, createNativeNewInlayImageEncoder, createNativeRemoteAssetReader } from '../../src/ts/storage/nativeAssetRepository'
import { createNativeDeviceSettings } from '../../src/ts/storage/nativeDeviceSettings'
import { createNativeOfficialAssetLedger } from '../../src/ts/storage/sync/officialAssetLedger'
import { createNativeOfficialAccountAssets } from '../../src/ts/storage/sync/nativeOfficialAccountAssets'
import { nativeOfficialAccountKeys } from '../../src/ts/storage/sync/nativeOfficialAccountFlow'
import type { AccountReadOptions, AccountReadResult } from '../../src/ts/storage/accountStorage'
import { hashPayloadBytes } from '../../src/ts/storage/payloadCas'
import { accountReadDeadline, readAccountBody } from '../../src/ts/storage/accountReadBody'
import { transferOfficialAccountAssets } from '../../src/ts/storage/sync/officialAccountTransfer'

export async function runOfficialAccountMeasurements(store: PersistentDataStore, origin: string) {
    if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(origin)) throw new Error('Synthetic loopback origin required')
    let timings: Record<string, number> = {}, calls: Record<string, number> = {}
    let sentBytes = 0, receivedBytes = 0
    const heapUsed = () => (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize ?? null
    let peakHeap = 0
    const timed = async <T>(name: string, run: () => Promise<T>): Promise<T> => {
        const start = performance.now()
        try { return await run() }
        finally { timings[name] = (timings[name] ?? 0) + performance.now() - start; calls[name] = (calls[name] ?? 0) + 1; peakHeap = Math.max(peakHeap, heapUsed() ?? 0) }
    }
    const measuredInvoke = async <T>(command: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> => {
        const data = args instanceof Uint8Array || args instanceof ArrayBuffer ? args : args && !Array.isArray(args) ? args.data : undefined
        sentBytes += data instanceof Uint8Array || data instanceof ArrayBuffer ? data.byteLength : Array.isArray(data) ? data.length : 0
        const result = await timed(`ipc.${command}`, () => invoke<T>(command, args, options))
        receivedBytes += result instanceof Uint8Array || result instanceof ArrayBuffer ? result.byteLength : Array.isArray(result) ? result.length : 0
        return result
    }
    const observedStore = new Proxy(store, { get(target, property) {
        const value = Reflect.get(target, property)
        return typeof value === 'function' ? (...args: unknown[]) => timed(`store.${String(property)}`, () => value.apply(target, args)) : value
    } })
    const baseBlobs = createCompleteAssetRepositoryBlobStore({
        store: observedStore, cas: createNativeImmutablePayloadCas(measuredInvoke),
        remote: createNativeRemoteAssetReader(measuredInvoke),
        writeSessions: createNativeDurableAssetWriteSessionFactory(measuredInvoke),
        objectUrls: createNativeAssetObjectUrlResolver(), newInlayImages: createNativeNewInlayImageEncoder(measuredInvoke),
    })
    const blobs = new Proxy(baseBlobs, { get(target, property) {
        const value = Reflect.get(target, property)
        return typeof value === 'function' ? (...args: unknown[]) => timed(`blob.${String(property)}`, () => value.apply(target, args)) : value
    } })
    const nativeSettings = new Proxy(createNativeDeviceSettings(), { get(target, property) {
        const value = Reflect.get(target, property)
        return typeof value === 'function' ? (...args: unknown[]) => timed(`settings.${String(property)}`, () => value.apply(target, args)) : value
    } })
    const samples: unknown[] = []
    let sequence = 1
    for (const size of [32 * 1024, 4 * 1024 * 1024]) for (const latency of [300, 1000]) for (const concurrency of [1, 2, 4] as const) {
        const count = 12
        const keys = Array.from({ length: count }, () => `assets/official-measure-${sequence++}-${latency}-${size}.bin`)
        await store.replaceFromDatabase({ characters: [], username: '', botPresets: [], botPresetsId: 0,
            modules: [{ id: 'synthetic-assets', name: 'Synthetic', assets: keys.map(key => ['synthetic', key, 'bin']) }],
            syntheticPadding: 'x'.repeat(4 * 1024 * 1024),
        } as unknown as Database)
        const ledger = await createNativeOfficialAssetLedger(nativeSettings, nativeOfficialAccountKeys.assetLedger, () => 'synthetic')
        await ledger.clearAll()
        let ledgerMs = 0
        const recorded = { ...ledger, record(key: string, replacement: string) {
            const start = performance.now(); ledger.record(key, replacement); ledgerMs += performance.now() - start
        } }
        const expected = new Map<string, string>()
        const account = { async readItem(key: string, options: AccountReadOptions = {}): Promise<AccountReadResult> {
            return timed('network.read', async () => {
                const deadline = accountReadDeadline(options.signal)
                try {
                    const response = await fetch(`${origin}/api/account/read/${Buffer.from(key).toString('hex')}`, {
                        headers: { 'x-risu-key': key }, signal: deadline.signal,
                    })
                    if (!response.ok) throw new Error(`Synthetic server status ${response.status}`)
                    const bytes = await readAccountBody(response, deadline.signal, deadline.reset, options.onBytes)
                    expected.set(key, await hashPayloadBytes(bytes))
                    return { kind: 'value', bytes }
                } finally { deadline.dispose() }
            })
        } }
        const assets = createNativeOfficialAccountAssets({ settings: nativeSettings, store: observedStore,
            account, ledger: recorded, resolveBlobs: async () => blobs, flushMetadata: () => timed('ledger.flush', () => ledger.flush()), origin, concurrency,
            getAccountId: () => 'synthetic',
        })
        await assets.prepare('synthetic')
        await assets.bind('synthetic')
        timings = {}; calls = {}; sentBytes = 0; receivedBytes = 0
        const initialHeap = heapUsed(); peakHeap = initialHeap ?? 0
        const started = performance.now()
        const startedAt = Date.now()
        const outcome = await assets.complete('synthetic')
        const elapsedMs = performance.now() - started
        if (outcome.state !== 'succeeded' || outcome.counts.installed !== count || expected.size !== count) throw new Error('Synthetic restore did not download every body')
        const sample = { count, size, latency, concurrency, elapsedMs, ledgerMs, timings: { ...timings }, calls: { ...calls }, sentBytes, receivedBytes, startedAt, finishedAt: Date.now(),
            initialJSHeapBytes: initialHeap, sampledPeakJSHeapBytes: peakHeap || null }
        for (const key of keys) {
            const alias = await store.readAssetAlias({ kind: 'asset', key })
            if (alias?.value.objectHash !== expected.get(key) || alias.value.size !== size) throw new Error('Synthetic restored alias differs')
        }
        samples.push(sample)
    }
    const ledgerSamples: unknown[] = []
    for (const count of [1000, 4000, 8000, 34000, 100000]) {
        let serializedCharacters = 0, snapshots = 0, last = ''
        const storage = { get: async () => null, set: async () => { last = '' }, patch: async (_key: string, entries: Record<string, string | null>) => {
            for (const value of Object.values(entries)) if (value !== null) { last = value; serializedCharacters += value.length; snapshots++ }
        } }
        const ledger = await createNativeOfficialAssetLedger(storage, 'synthetic', () => 'synthetic')
        const started = performance.now()
        for (let index = 0; index < count; index++) { const key = `assets/${index.toString(16).padStart(64, '0')}.png`; ledger.record(key, key) }
        await ledger.flush()
        const elapsedMs = performance.now() - started
        if (Object.keys(JSON.parse(last).assets).length !== count) throw new Error('Synthetic ledger lost entries')
        ledgerSamples.push({ count, elapsedMs, snapshots, serializedCharacters, finalCharacters: last.length, storage: 'in-memory serialization sink' })
    }
    const nativeLedgerSamples: unknown[] = []
    for (const count of [1000, 4000, 8000, 34000, 100000]) {
        let writes = 0, writeMs = 0, characters = 0
        const settings = { get: nativeSettings.get.bind(nativeSettings), set: nativeSettings.set.bind(nativeSettings), patch: async (key: string, entries: Record<string, string | null>) => {
            const start = performance.now()
            await nativeSettings.patch(key, entries)
            writeMs += performance.now() - start
            for (const value of Object.values(entries)) if (value !== null) { writes++; characters += value.length }
        } }
        const ledger = await createNativeOfficialAssetLedger(settings, nativeOfficialAccountKeys.assetLedger, () => 'synthetic')
        await ledger.clearAll()
        writes = 0; writeMs = 0; characters = 0
        const startedAt = Date.now()
        const initialHeap = heapUsed(); peakHeap = initialHeap ?? 0
        const start = performance.now()
        for (let index = 0; index < count; index++) { const key = `assets/${index.toString(16).padStart(64, '0')}.png`; ledger.record(key, key) }
        const recordMs = performance.now() - start
        await ledger.flush()
        const elapsedMs = performance.now() - start
        const saved = await settings.get(nativeOfficialAccountKeys.assetLedger) as Record<string, string>
        if (Object.keys(JSON.parse(saved['officialPublishedAssets:synthetic']).assets).length !== count) throw new Error('Native ledger lost entries')
        nativeLedgerSamples.push({ count, recordMs, elapsedMs, writes, writeMs, serializedCharacters: characters, startedAt, finishedAt: Date.now(),
            initialJSHeapBytes: initialHeap, sampledPeakJSHeapBytes: peakHeap || null })
    }
    const presenceSamples: unknown[] = []
    const seed = Uint8Array.of(17, 23, 9)
    const seedHash = await hashPayloadBytes(seed)
    await baseBlobs.putVerified!('assets/presence-seed', seed, { kind: 'asset', mime: '', name: 'seed', ext: 'bin' })
    for (const count of [1000, 4000, 8000, 34000, 100000]) {
        const aliases: AssetAlias[] = Array.from({ length: count }, (_, index) => ({ kind: 'asset', key: `assets/presence-${index}`,
            objectHash: index % 2 ? index % 4 === 1 ? 'e'.repeat(64) : null : seedHash, size: 3, mime: '', name: 'synthetic', ext: 'bin' }))
        await store.replaceFromDatabase({ characters: [], botPresets: [], botPresetsId: 0,
            syntheticPadding: 'x'.repeat(4 * 1024 * 1024),
        } as unknown as Database, undefined, aliases)
        const keys = aliases.map(alias => alias.key)
        const startedAt = Date.now()
        timings = {}; calls = {}
        const initialHeap = heapUsed(); peakHeap = initialHeap ?? 0
        const start = performance.now()
        const found = await blobs.statMany!(keys)
        const elapsedMs = performance.now() - start
        if (found.some((value, index) => !!value !== (index % 2 === 0))) throw new Error('Batched presence differs from native residency')
        presenceSamples.push({ count, elapsedMs, present: found.filter(Boolean).length, calls: { ...calls }, startedAt, finishedAt: Date.now(),
            initialJSHeapBytes: initialHeap, sampledPeakJSHeapBytes: peakHeap || null })
    }
    const manifestSamples: unknown[] = [], schedulerSamples: unknown[] = []
    for (const count of [1000, 4000, 8000, 34000, 100000]) {
        const keys = Array.from({ length: count }, (_, index) => `assets/${index.toString(16).padStart(64, '0')}.bin`)
        await store.replaceFromDatabase({ characters: [], botPresets: [], botPresetsId: 0,
            modules: [{ id: 'synthetic-manifest', name: 'Synthetic', assets: keys.map(key => ['synthetic', key, 'bin']) }],
            syntheticPadding: 'x'.repeat(4 * 1024 * 1024),
        } as unknown as Database)
        const continuation = createNativeOfficialAccountAssets({ settings: nativeSettings, store: observedStore,
            resolveBlobs: async () => blobs, account: { readItem: async () => { throw new Error('Manifest measurement must not download') } },
            ledger: { record() {}, clear() {}, reset() {}, publishedAs: () => null }, flushMetadata: async () => {}, origin, getAccountId: () => 'synthetic',
        })
        const startedAt = Date.now(), started = performance.now()
        await continuation.prepare('synthetic'); await continuation.bind('synthetic')
        const writtenMs = performance.now() - started
        const saved = await continuation.load('synthetic')
        if (saved?.manifest.length !== count || saved.manifest.some((key, index) => key !== keys[index])) throw new Error('Frozen native manifest lost its identity or ordering')
        manifestSamples.push({ count, writtenMs, elapsedMs: performance.now() - started, startedAt, finishedAt: Date.now() })
        await continuation.clear()
        let completed = 0
        const scheduling = performance.now()
        await transferOfficialAccountAssets({ accountId: 'synthetic-scheduler', origin, keys,
            read: async () => ({ kind: 'missing' }), install: async () => { throw new Error('Missing bodies must not install') },
            settled: (_key, result) => { if (result !== 'missing') throw new Error('Unexpected synthetic scheduler result'); completed++ },
        })
        if (completed !== count) throw new Error('Synthetic scheduler lost keys')
        schedulerSamples.push({ count, elapsedMs: performance.now() - scheduling, note: 'Production scheduler with immediate synthetic missing responses; no network or storage.' })
    }
    let checkpoints = 0, checkpointWriteMs = 0
    const checkpointsLedger = await createNativeOfficialAssetLedger({ get: async () => null, set: nativeSettings.set.bind(nativeSettings),
        patch: async (key, entries) => { const start = performance.now(); await nativeSettings.patch(key, entries); checkpoints++; checkpointWriteMs += performance.now() - start },
    }, nativeOfficialAccountKeys.assetLedger, () => 'synthetic')
    const checkpointStart = performance.now()
    for (let batch = 0; batch < 2; batch++) {
        for (let index = batch * 4000; index < (batch + 1) * 4000; index++) checkpointsLedger.record(`assets/timed-${index}`, `assets/timed-${index}`)
        await new Promise(resolve => setTimeout(resolve, 31_000))
    }
    checkpointsLedger.record('assets/final', 'assets/final')
    await checkpointsLedger.flush()
    const checkpointElapsedMs = performance.now() - checkpointStart
    const persisted = await nativeSettings.get(nativeOfficialAccountKeys.assetLedger) as Record<string, string>
    if (checkpoints < 2 || Object.keys(JSON.parse(persisted['officialPublishedAssets:synthetic']).assets).length !== 8001) throw new Error('Elapsed native checkpoints did not persist the final generation')
    return { runtime: navigator.userAgent, rootPaddingBytes: 4 * 1024 * 1024, samples, ledgerSamples, nativeLedgerSamples, presenceSamples, manifestSamples, schedulerSamples,
        elapsedCheckpoints: { count: 8001, checkpoints, elapsedMs: checkpointElapsedMs, writeMs: checkpointWriteMs },
        note: 'Actual account asset completion with native SQLite/CAS and loopback HTTP at the account transport boundary. Timings are nested, not additive. Network timing includes independent source hashing. Ledger sink and actual native ledger samples are separate. Sampled JS heap excludes native/WebView process memory and may miss short-lived peaks. Synthetic data only.' }
}
