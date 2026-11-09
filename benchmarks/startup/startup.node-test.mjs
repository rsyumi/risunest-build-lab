import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { maxConcurrentResources, sanitizeMetrics, percentile, SDK_VERSION, IPC_BOUNDARY, SEMANTIC_BOUNDARY,
    OBSERVER_INPUTS, IPC_COMMANDS, assertObservationIdentity, assertCompleteObservation } from './metrics.mjs'
import {
    addSyntheticAssets,
    countSyntheticAssetEntries,
    assertSyntheticProfile,
    seedExpression,
    syntheticPng,
} from './fixture.mjs'
import { DatabaseSync } from 'node:sqlite'
import { instrumentSource, readObservationInputs, verifyObservationManifest, startupObservationPlugin } from './observe.mjs'
import { observeWal, observeWindowsMemory, prepareWindowsMemory, summarizeMemorySamples } from './host-metrics.mjs'
import { mkdtemp, mkdir, writeFile, rm, readdir } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { startupInstrumentation } from './android-observation.mjs'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'
import { connectVerifiedIdentity, isAllowedStartupUrl } from './cdp.mjs'
import { assertAgentDataRoot, syntheticProfile, copyBenchmarkBinary, resolveCargoTarget } from './runtime.mjs'
import { EventEmitter, once } from 'node:events'
import { PassThrough, Writable } from 'node:stream'
import { runInNewContext } from 'node:vm'
import { webcrypto } from 'node:crypto'

function observationManifest(platform = 'windows') {
    return { schema: 1, boundary: IPC_BOUNDARY, semanticBoundary: SEMANTIC_BOUNDARY, platform,
        sourceRevision: 'a'.repeat(40), inputFingerprint: 'b'.repeat(64), diagnosticStages: false,
        sdk: { version: SDK_VERSION, sourceSha256: 'c'.repeat(64), transformedSha256: 'd'.repeat(64) },
        store: { sourceSha256: 'e'.repeat(64), transformedSha256: 'f'.repeat(64) },
        observerInputs: Object.fromEntries(OBSERVER_INPUTS.map(key => [key, '1'.repeat(64)])), selectedCommands: [...IPC_COMMANDS] }
}

function definition(source, name, kind = 'function') {
    const file = ts.createSourceFile('synthetic.ts', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
    let found
    const visit = node => {
        if ((kind === 'method' ? ts.isMethodDeclaration(node) : ts.isFunctionDeclaration(node)) && node.name?.getText(file) === name)
            found = node.getText(file)
        ts.forEachChild(node, visit)
    }
    visit(file)
    assert.ok(found, `Missing actual definition: ${name}`)
    return found
}

async function sdkRuntime({ platform = 'windows', nativeFetch, onPost } = {}) {
    const fixture = async name => readFile(new URL(`../../tests/fixtures/tauri-2.12.1/${name}`, import.meta.url), 'utf8')
    const processMessage = await fixture('process-ipc-message-fn.js')
    const render = source => source.replace('__TEMPLATE_invoke_key__', JSON.stringify('synthetic-key'))
        .replace('__RAW_process_ipc_message_fn__', processMessage)
        .replaceAll('__TEMPLATE_os_name__', JSON.stringify(platform))
        .replace('__TEMPLATE_protocol_scheme__', JSON.stringify('http'))
        .replace('__TEMPLATE_fetch_channel_data_command__', JSON.stringify('plugin:__TAURI_CHANNEL__|fetch'))
    let clock = 0
    const posts = [], fetches = []
    const manifest = observationManifest(platform)
    const context = { Headers, TextEncoder, TextDecoder, Uint8Array, ArrayBuffer, Error, DOMException,
        crypto: webcrypto, setTimeout, performance: { now: () => ++clock, mark() {} },
        console: { warn() {} }, localStorage: { getItem: () => 'false' },
        document: { addEventListener() {} }, PerformanceObserver: class { observe() {} }, __TAURI_INTERNALS__: {} }
    context.window = context
    context.fetch = async (...args) => {
        fetches.push(args)
        if (!nativeFetch) throw new Error('Synthetic fetch unavailable')
        return nativeFetch(...args)
    }
    context.ipc = { postMessage(data) {
        const message = JSON.parse(data)
        posts.push(message)
        const reply = (value, failed = false) => context.__TAURI_INTERNALS__.runCallback(failed ? message.error : message.callback, value)
        if (onPost) onPost(message, reply)
        else reply({ revision: 17 })
    } }
    runInNewContext(render(await fixture('core.js')), context)
    runInNewContext(render(await fixture('ipc-protocol.js')) + '\n' + (await readFile(
        new URL('../../src-tauri/src/ipc_read_guard.js', import.meta.url), 'utf8',
    )).replace('__RISUNEST_ANDROID__', String(platform === 'android')), context)
    context.__TAURI_INTERNALS__.ipc = message => context.__TAURI_INTERNALS__.postMessage(message)
    runInNewContext(startupInstrumentation(platform, false, manifest), context)
    const sdk = await readFile(new URL('../../node_modules/@tauri-apps/api/core.js', import.meta.url), 'utf8')
    const observed = instrumentSource(sdk, '/node_modules/@tauri-apps/api/core.js', platform)
    const invoke = runInNewContext(`(() => {${definition(observed, 'invoke')}; return invoke;})()`, context)
    return { context, invoke, posts, fetches, manifest, advance: ms => { clock += ms } }
}

test('CDP retries a disappearing endpoint and still verifies identity before returning', async (t) => {
    const originalWebSocket = globalThis.WebSocket
    t.after(() => {
        globalThis.WebSocket = originalWebSocket
    })
    const sockets = []
    t.mock.method(globalThis, 'fetch', async () => ({
        json: async () => [{ type: 'page', webSocketDebuggerUrl: 'ws://synthetic' }],
    }))
    globalThis.WebSocket = class {
        closed = false
        methods = []
        constructor() {
            this.index = sockets.push(this)
            queueMicrotask(() => (this.index === 1 ? this.onerror() : this.onopen()))
        }
        close() {
            this.closed = true
        }
        send(encoded) {
            const { id, method } = JSON.parse(encoded)
            this.methods.push(method)
            queueMicrotask(() =>
                this.onmessage({
                    data: JSON.stringify({
                        id,
                        result:
                            method === 'Runtime.evaluate'
                                ? { result: { value: this.index === 3 } }
                                : {},
                    }),
                }),
            )
        }
    }
    const client = await connectVerifiedIdentity(12345, 'synthetic', 1000)
    assert.equal(sockets.length, 3)
    assert.equal(sockets[0].closed, true)
    assert.equal(sockets[1].closed, true)
    assert.equal(sockets[2].closed, false)
    assert.ok(
        sockets[2].methods.indexOf('Network.setBlockedURLs') <
            sockets[2].methods.indexOf('Runtime.evaluate'),
    )
    client.close()
    assert.equal(sockets[2].closed, true)
    let failedSocketsClosed = 0
    globalThis.WebSocket = class {
        constructor() {
            queueMicrotask(() => this.onerror())
        }
        close() {
            failedSocketsClosed++
        }
    }
    await assert.rejects(
        connectVerifiedIdentity(12345, 'synthetic', 20),
        /Verified isolated CDP target unavailable/,
    )
    assert.ok(failedSocketsClosed > 0)
})

test('M0 summary requires per-sample semantic commits and canonical current Windows memory', async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'risunest-m0-summary-'))
    const file = path.join(directory, 'result.json')
    try {
        const manifest = observationManifest('android')
        const row = (kind) => ({
            kind,
            warmup: false,
            interaction: {
                success: true,
                failure: null,
                inputAccepted: true,
                scrollImmediateChanged: true,
                inputMs: 1,
                scrollMs: 2,
            },
            stabilizationTimeout: false,
            documentVisible: true,
            ipcObservation: manifest, observerFailure: null, unfinishedOperations: [],
            calls: [{ command: 'pds_commit_android_finish', success: true, start: 1, ms: 2, bytes: null }],
            operations: [{ stage: 'store-commit', start: 0, ms: 3, success: true }],
            longTasks: [],
            usedHeapBytes: 1024,
            host: {
                memory: { appPssBytes: 2048, appRssBytes: 4096 },
                wal: { startBytes: 0, endBytes: 0 },
            },
        })
        let input = {
            platform: 'android-arm64',
            ipcObservation: manifest,
            samples: ['reload', 'restart'].flatMap((kind) =>
                Array.from({ length: 20 }, () => row(kind)),
            ),
        }
        const summarize = () =>
            spawnSync(
                process.execPath,
                [fileURLToPath(new URL('./m0-summary.mjs', import.meta.url)), file],
                { encoding: 'utf8', windowsHide: true },
            )
        await writeFile(file, JSON.stringify(input))
        const valid = summarize()
        assert.equal(valid.status, 0, valid.stderr)
        assert.equal(JSON.parse(valid.stdout).groups.reload.samples, 20)
        assert.equal(JSON.parse(valid.stdout).groups.reload.semanticCommitMs.p50, 3)
        assert.equal(JSON.parse(valid.stdout).groups.reload.commitIpcMs, undefined)
        const validAndroid = structuredClone(input)
        input.samples[0].documentVisible = false
        await writeFile(file, JSON.stringify(input))
        assert.notEqual(summarize().status, 0)
        input.samples[0].documentVisible = true
        input.samples[1].operations.push(...input.samples[0].operations)
        input.samples[0].operations = []
        await writeFile(file, JSON.stringify(input))
        assert.notEqual(summarize().status, 0)
        input = structuredClone(validAndroid)
        delete input.ipcObservation
        await writeFile(file, JSON.stringify(input))
        assert.notEqual(summarize().status, 0)
        input = structuredClone(validAndroid)
        input.samples[0].ipcObservation = observationManifest('windows')
        await writeFile(file, JSON.stringify(input))
        assert.notEqual(summarize().status, 0)
        for (const breakRow of [
            value => { value.operations[0].success = 'false' },
            value => { value.operations[0].start = -1 },
            value => { delete value.operations[0].start },
            value => { value.calls[0].start = -1 },
            value => { delete value.calls[0].start },
        ]) {
            input = structuredClone(validAndroid)
            breakRow(input.samples[0])
            await writeFile(file, JSON.stringify(input))
            assert.notEqual(summarize().status, 0)
        }

        const sweeps = [
            { ...memorySample(10, 500, 850), nativeWorkingSetBytes: 400, nativePrivateBytes: 100 },
            { ...memorySample(120, 1000, 600), nativeWorkingSetBytes: 100, nativePrivateBytes: 500 },
            { ...memorySample(240, 600, 700), nativeWorkingSetBytes: 200, nativePrivateBytes: 300 },
        ]
        input = structuredClone(validAndroid)
        input.platform = 'windows'
        input.ipcObservation = observationManifest()
        for (const sample of input.samples) {
            sample.ipcObservation = input.ipcObservation
            sample.calls[0].command = 'pds_commit_shared_finish'
            sample.host.memory = { ...summarizeMemorySamples(structuredClone(sweeps), 100, { terminalFailure: null, cleanupFailure: null }),
                nativePeakWorkingSetBytes: 99999999 }
            sample.host.wal.peakBytes = 4
        }
        await writeFile(file, JSON.stringify(input))
        const windows = summarize()
        assert.equal(windows.status, 0, windows.stderr)
        const group = JSON.parse(windows.stdout).groups.reload
        for (const [name, value] of Object.entries({ nativeSettledWorkingSetBytes: 200, nativeSampledMaxWorkingSetBytes: 400,
            treeSettledWorkingSetBytes: 600, treeSampledMaxWorkingSetBytes: 1000, nativeSettledPrivateBytes: 300,
            nativeSampledMaxPrivateBytes: 500, treeSettledPrivateBytes: 700, treeSampledMaxPrivateBytes: 850 }))
            assert.equal(group[name].p50, value, name)
        assert.equal(group.nativePeakWorkingSetBytes, undefined)
        assert.equal(group.nativeLifetimePeakAvailability, 'not-collected-by-current-stream')
        assert.equal(JSON.stringify(group).includes('99999999'), false)
        const validWindows = JSON.parse(JSON.stringify(input))
        const invalid = [
            memory => { memory.coverageComplete = false },
            memory => { memory.terminalFailure = { code: 'helper-exit' } },
            memory => { memory.cleanupFailure = { code: 'shutdown-failed' } },
            memory => { memory.measurementFailure = { code: 'measurement-failed' } },
            memory => { delete memory.series },
            memory => { delete memory.series[0].nativePrivateBytes },
            memory => { memory.sampledPeak = null },
            memory => { delete memory.first },
            memory => { memory.settled.treeWorkingSetBytes++ },
            memory => { memory.observedCompleteSweepMax.treePrivateBytes++ },
            memory => { memory.series.push(incompleteMemorySample(300)); memory.samples++; memory.completeSamples++ },
            memory => { for (const key of Object.keys(memory)) delete memory[key]; Object.assign(memory, { nativeWorkingSetBytes: 200, nativePeakWorkingSetBytes: 999, treeWorkingSetBytes: 600 }) },
        ]
        for (const breakMemory of invalid) {
            input = structuredClone(validWindows)
            breakMemory(input.samples[0].host.memory)
            await writeFile(file, JSON.stringify(input))
            assert.notEqual(summarize().status, 0)
        }
        input = structuredClone(validAndroid)
        input.samples = input.samples.slice(1)
        await writeFile(file, JSON.stringify(input))
        assert.notEqual(summarize().status, 0)
    } finally {
        await rm(directory, { recursive: true, force: true })
    }
})

test('startup summary keeps legacy definitions separate from SDK and semantic observations', async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'risunest-invocation-summary-'))
    const file = path.join(directory, 'result.json')
    try {
        const manifest = observationManifest()
        const sample = { warmup: false, kind: 'restart', mode: 'no-plugins', assets: 0,
            ipcObservation: manifest, observerFailure: null, unfinishedOperations: [], interactiveMs: 10,
            calls: [{ command: 'pds_open', start: 1, ms: 2, success: true, bytes: null },
                { command: 'pds_commit_shared_finish', start: 5, ms: 2, success: true, bytes: null }],
            operations: [{ stage: 'bootstrap', start: 0, ms: 10, success: true },
                { stage: 'store-commit', start: 4, ms: 4, success: true }], longTasks: [] }
        const run = async input => {
            await writeFile(file, JSON.stringify(input))
            return spawnSync(process.execPath, [fileURLToPath(new URL('./summarize.mjs', import.meta.url)), file],
                { encoding: 'utf8', windowsHide: true })
        }
        const current = await run({ ipcObservation: manifest, samples: [sample] })
        assert.equal(current.status, 0, current.stderr)
        const group = JSON.parse(current.stdout)[0]
        assert.equal(group.observationBoundary, IPC_BOUNDARY)
        assert.equal(group.semanticCommitCount.p50, 1)
        assert.equal(group.semanticCommitMs.p50, 4)
        assert.equal(group.sdkInvocationsByCommand.pds_commit_shared_finish.count, 1)
        assert.equal(group.commits, undefined)
        assert.equal(group.openBytes.measured, 0)
        assert.equal(group.openBytes.p50, null)
        const legacy = structuredClone(sample)
        delete legacy.ipcObservation
        legacy.operations = legacy.operations.filter(operation => operation.stage !== 'store-commit')
        legacy.calls[0].bytes = 123
        legacy.calls[1].command = 'pds_commit'
        const old = await run({ samples: [legacy] })
        assert.equal(old.status, 0, old.stderr)
        const historic = JSON.parse(old.stdout)[0]
        assert.equal(historic.observationBoundary, 'legacy-transport-or-store')
        assert.equal(historic.commits.p50, 1)
        assert.equal(historic.openBytes.p50, 123)
        assert.equal(historic.semanticCommitCount, undefined)
        assert.notEqual((await run({ samples: [sample] })).status, 0)
        assert.notEqual((await run({ ipcObservation: manifest, samples: [legacy] })).status, 0)
    } finally { await rm(directory, { recursive: true, force: true }) }
})

test('SDK observer accepts successful guarded responses without retry, clone, or extra serialization', async () => {
    let reads = 0, serialized = 0
    const response = { revision: 17, toJSON() { serialized++; throw new Error('Unexpected serialization') } }
    const runtime = await sdkRuntime({ nativeFetch: async () => ({
        headers: new Headers({ 'Content-Type': 'application/json', 'Tauri-Response': 'ok' }),
        json: async () => { reads++; return response },
    }) })
    const originalFetch = runtime.context.fetch
    const unsupported = []
    runtime.context.fetch = (...args) => originalFetch(...args).then(value => new Proxy(value, {
        get(target, key) {
            if (key === 'clone' || key === 'ok') { unsupported.push(key); throw new Error('Unsupported response member') }
            return Reflect.get(target, key)
        },
    }))
    assert.equal(await runtime.invoke('pds_open'), response)
    assert.equal(runtime.fetches.length, 1)
    assert.equal(runtime.posts.length, 0)
    assert.equal(reads, 1)
    assert.equal(serialized, 0)
    assert.deepEqual(unsupported, [])
    const state = runtime.context.__startupMetrics
    assert.equal(state.calls.length, 1)
    assert.equal(state.calls[0].success, true)
    assert.equal(state.calls[0].bytes, null)
    assert.equal(state.firstRevision, 17)
    assert.equal(Object.getOwnPropertyDescriptor(runtime.context.__TAURI_INTERNALS__, 'invoke').writable, false)
    assertCompleteObservation(state, runtime.manifest)

    runtime.context.__startupRecord = () => { throw new Error('Synthetic observer failure') }
    assert.equal(await runtime.invoke('pds_read_root'), response)
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(state.observerFailure, 'recording-failed')
    assert.throws(() => assertCompleteObservation(state, runtime.manifest), /observer failed/)
})

test('SDK observer counts one final invocation across fallback and preserves rejection identity', async () => {
    const replies = []
    const runtime = await sdkRuntime({ onPost: (message, reply) => { replies.push(reply) } })
    const pending = runtime.invoke('pds_open')
    await new Promise(resolve => setImmediate(resolve))
    const state = runtime.context.__startupMetrics
    assert.equal(runtime.fetches.length, 1)
    assert.equal(runtime.posts.length, 1)
    assert.equal(state.calls.length, 1)
    assert.equal(state.calls[0].ms, null)
    assert.throws(() => assertCompleteObservation(state, runtime.manifest), /Incomplete/)
    runtime.advance(50)
    const response = { revision: 19 }
    replies.shift()(response)
    assert.equal(await pending, response)
    assert.ok(state.calls[0].ms >= 50)
    assert.equal(state.calls.length, 1)
    assert.equal(state.calls[0].success, true)
    assertCompleteObservation(state, runtime.manifest)
    const failure = new Error('Synthetic native failure')
    const rejected = runtime.invoke('pds_read_revision')
    replies.shift()(failure, true)
    await assert.rejects(rejected, error => error === failure)
    assert.equal(state.calls[1].success, false)
    assert.equal(state.calls[1].bytes, null)
    assert.throws(() => runtime.context.__startupObserveCall('pds_commit', () => { throw failure }), error => error === failure)
    runtime.context.__startupRecord = () => { throw new Error('Synthetic observer failure') }
    const failedObservation = runtime.invoke('pds_read_root')
    replies.shift()(failure, true)
    await assert.rejects(failedObservation, error => error === failure)
    await new Promise(resolve => setImmediate(resolve))
    assert.throws(() => assertCompleteObservation(state, runtime.manifest), /observer failed/)
})

test('SDK observer tracks concurrent media and filters commands on the immutable Android bridge', async () => {
    const replies = []
    const runtime = await sdkRuntime({ platform: 'android', onPost: (message, reply) => replies.push(reply) })
    const first = runtime.invoke('asset_cas_read_bytes')
    const second = runtime.invoke('asset_cas_read_bytes')
    const state = runtime.context.__startupMetrics
    assert.equal(state.mediaInFlight, 2)
    assert.equal(state.maxMediaInFlight, 2)
    replies.shift()(new Uint8Array([1, 2]))
    await first
    assert.equal(state.mediaInFlight, 1)
    const failure = new Error('Synthetic media failure')
    replies.shift()(failure, true)
    await assert.rejects(second, error => error === failure)
    assert.equal(state.mediaInFlight, 0)
    const unselectedCommand = 'unselected_synthetic_command'
    const filtered = runtime.invoke(unselectedCommand)
    replies.shift()(23)
    assert.equal(await filtered, 23)
    assert.equal(state.calls.length, 2)
    assert.equal(runtime.fetches.length, 0)
    assertCompleteObservation(state, runtime.manifest)
})

test('semantic commit settling after two minutes is retained instead of silently dropped', async () => {
    const runtime = await sdkRuntime()
    let release
    const pending = new Promise(resolve => { release = resolve })
    assert.equal(runtime.context.__startupObserveOperation('store-commit', () => pending), pending)
    runtime.advance(120001)
    release(29)
    assert.equal(await pending, 29)
    const state = runtime.context.__startupMetrics
    const commits = state.operations.filter(operation => operation.stage === 'store-commit')
    assert.equal(commits.length, 1)
    assert.equal(commits[0].success, true)
    assert.ok(commits[0].ms >= 120001)
    assertCompleteObservation(state, runtime.manifest)
})

test('legacy Android mutation smoke refuses before APK or ADB access', () => {
    const result = spawnSync(process.execPath, [fileURLToPath(new URL('./android-smoke.mjs', import.meta.url)),
        '--adb=missing-synthetic-adb', '--apk=missing-synthetic.apk', '--serial=emulator-5554',
        '--avd=synthetic', '--adb-port=12345'], { encoding: 'utf8', windowsHide: true })
    assert.equal(result.status, 1)
    assert.equal(result.stdout, '')
    assert.deepEqual(JSON.parse(result.stderr.trim()), { phase: 'preflight', profile: 'legacy-api21-mutation',
        success: false, reason: 'unsupported-legacy-profile' })
})

test('SDK build boundaries and artifact custody reject drift, duplication, and stale manifests', async () => {
    const repository = fileURLToPath(new URL('../..', import.meta.url))
    const sdkId = '/node_modules/@tauri-apps/api/core.js'
    const storeId = '/src/ts/storage/sqlitePersistentDataStore.ts'
    const sdk = await readFile(new URL('../../node_modules/@tauri-apps/api/core.js', import.meta.url), 'utf8')
    const store = await readFile(new URL('../../src/ts/storage/sqlitePersistentDataStore.ts', import.meta.url), 'utf8')
    assert.throws(() => instrumentSource(sdk, sdkId, 'windows', '0.0.0'), /version/)
    assert.throws(() => instrumentSource(sdk.replace('async function invoke(cmd, args = {}, options)', 'async function invoke(cmd, args, options)'), sdkId, 'windows'), /AST/)
    assert.throws(() => instrumentSource(sdk.replace('invoke, isTauri', 'isTauri'), sdkId, 'windows'), /boundary/)
    assert.throws(() => instrumentSource(sdk + '\n' + definition(sdk, 'invoke'), sdkId, 'windows'), /duplicate/)
    assert.throws(() => instrumentSource(store.replace('    commit(input:', '    renamedCommit(input:'), storeId, 'windows'), /boundary/)
    const plugin = startupObservationPlugin('windows', repository)
    await plugin.buildStart()
    assert.throws(() => plugin.buildEnd(), /Missing/)
    plugin.transform(sdk, sdkId)
    assert.throws(() => plugin.transform(sdk, sdkId), /duplicate/)
    plugin.transform(store, storeId)
    plugin.buildEnd()
    let emitted
    plugin.generateBundle.call({ emitFile: asset => { emitted = JSON.parse(asset.source) } })
    const inputs = await readObservationInputs(repository, 'windows')
    assert.deepEqual(verifyObservationManifest(emitted, inputs).sdk, emitted.sdk)
    for (const breakManifest of [
        value => { delete value.boundary },
        value => { value.boundary = 'legacy-transport' },
        value => { value.sdk.version = '0.0.0' },
        value => { value.sdk.transformedSha256 = '0'.repeat(64) },
        value => { value.store.transformedSha256 = '0'.repeat(64) },
        value => { value.inputFingerprint = '0'.repeat(64) },
        value => { value.observerInputs[OBSERVER_INPUTS[0]] = '0'.repeat(64) },
        value => { value.sourceRevision = '0'.repeat(40) },
        value => { delete value.store.transformedSha256 },
    ]) {
        const bad = structuredClone(emitted)
        breakManifest(bad)
        assert.throws(() => verifyObservationManifest(bad, inputs), /artifact/)
    }
    assert.throws(() => verifyObservationManifest(undefined, inputs), /artifact/)
    assert.throws(() => verifyObservationManifest(emitted, { ...inputs, inputFingerprint: '0'.repeat(64) }), /artifact/)
    const otherOption = await readObservationInputs(repository, 'windows', !inputs.diagnosticStages)
    assert.notEqual(otherOption.inputFingerprint, inputs.inputFingerprint)
    assert.throws(() => verifyObservationManifest(emitted, otherOption), /artifact/)
    assert.throws(() => startupInstrumentation('windows', true, observationManifest()), /diagnostic option/)
})

test('Android chunked save has one semantic operation and separate SDK invocations with native error restoration', async () => {
    let committed = 7, bytes = 0, failFinish = false
    const runtime = await sdkRuntime({ platform: 'android', onPost(message, reply) {
        const args = message.payload
        switch (message.cmd) {
            case 'pds_commit_android_open': bytes = 0; reply({ capacity: 32768 }); break
            case 'pds_commit_android_chunk':
                assert.equal(args.offset, bytes)
                bytes += new TextEncoder().encode(args.chunk).byteLength
                reply(bytes); break
            case 'pds_commit_android_finish':
                if (failFinish) reply({ code: 'store-error', message: 'Synthetic commit rejection' }, true)
                else reply({ revision: ++committed })
                break
            case 'pds_commit_android_cancel': reply(null); break
            default: throw new Error('Unexpected synthetic persistence command')
        }
    } })
    const sourceNames = ['nativePersistenceValue', 'androidBinaryCommitBridge', 'androidCommitTransport', 'nativeCommitTransport']
    const source = Object.fromEntries(await Promise.all(sourceNames.map(async name => [name,
        await readFile(new URL(`../../src/ts/storage/${name}.ts`, import.meta.url), 'utf8')])) )
    const cache = new Map()
    const load = name => {
        if (cache.has(name)) return cache.get(name)
        if (name === '@tauri-apps/api/core') return { invoke: runtime.invoke }
        if (name === '@tauri-apps/plugin-os') return { platform: () => 'android' }
        const key = name.replace(/^\.\//, '')
        assert.ok(source[key], `Unexpected source dependency: ${key}`)
        const module = { exports: {} }
        cache.set(name, module.exports)
        const code = ts.transpileModule(source[key], { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText
        runInNewContext(`(function(require,module,exports){${code}\n})`, runtime.context)(load, module, module.exports)
        return module.exports
    }
    const transport = load('nativeCommitTransport').nativeCommitTransport
    const actual = await readFile(new URL('../../src/ts/storage/sqlitePersistentDataStore.ts', import.meta.url), 'utf8')
    const observed = instrumentSource(actual, '/src/ts/storage/sqlitePersistentDataStore.ts', 'android')
    const code = ts.transpileModule(`${definition(actual, 'restoreStoreError')}
        class SqlitePersistentDataStore { constructor(transport) { this.transport = transport; }
        ${definition(observed, 'commit', 'method')} }
        globalThis.__SyntheticStore = SqlitePersistentDataStore;`,
        { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText
    runInNewContext(code, runtime.context)
    const store = new runtime.context.__SyntheticStore(transport)
    const response = await store.commit({ expectedRevision: 7, root: { synthetic: 'x'.repeat(140000) } })
    assert.equal(response.revision, 8)
    assert.ok(bytes > 140000)
    const state = runtime.context.__startupMetrics
    assert.equal(state.operations.filter(operation => operation.stage === 'store-commit').length, 1)
    assert.equal(state.calls.filter(call => call.command === 'pds_commit').length, 0)
    assert.ok(state.calls.filter(call => call.command === 'pds_commit_android_chunk').length > 1)
    assert.equal(state.operations.find(operation => operation.stage === 'store-commit').success, true)
    assertCompleteObservation(state, runtime.manifest)
    failFinish = true
    await assert.rejects(store.commit({ expectedRevision: 8, root: { synthetic: 'y'.repeat(140000) } }),
        error => error instanceof Error && error.message === 'Synthetic commit rejection' && error.code === 'store-error')
    const commits = state.operations.filter(operation => operation.stage === 'store-commit')
    assert.equal(commits.length, 2)
    assert.equal(commits[1].success, false)
    assertCompleteObservation(state, runtime.manifest)
})

test('asset counts reject a replacement generation with no aliases', async () => {
    const parent = await mkdtemp(path.join(os.tmpdir(), 'risunest-catalog-test-'))
    const identifier = 'RisuNest.phase3benchmark.r0123456789ab.catalogtest'
    const root = path.join(parent, identifier)
    await mkdir(path.join(root, 'persistent'), { recursive: true })
    const file = path.join(root, 'persistent/persistent.sqlite')
    try {
        const db = new DatabaseSync(file)
        db.exec(`CREATE TABLE meta(key TEXT,value TEXT);
            INSERT INTO meta VALUES('activeGeneration','1');
            CREATE TABLE asset_objects(object_hash TEXT,byte_size INTEGER,created_at_ms INTEGER);
            CREATE TABLE asset_aliases(generation INTEGER,logical_key TEXT,object_hash TEXT,kind TEXT,size INTEGER,mime TEXT,name TEXT,ext TEXT);`)
        db.close()
        const first = await addSyntheticAssets(root, identifier, 2)
        assert.equal(first.objects, 2)
        assert.equal(first.aliases, 2)
        const replaced = new DatabaseSync(file)
        const objects = replaced.prepare('SELECT object_hash FROM asset_aliases ORDER BY logical_key').all()
        replaced.exec("UPDATE meta SET value='2' WHERE key='activeGeneration'")
        replaced.close()
        for (const [index, { object_hash: hash }] of objects.entries()) {
            assert.deepEqual(
                await readFile(path.join(root, 'assets/objects', hash.slice(0, 2), hash.slice(2))),
                Buffer.from('Synthetic startup asset ' + index),
            )
        }
        await assert.rejects(addSyntheticAssets(root, identifier, 2), /catalog counts/)
    } finally {
        await rm(parent, { recursive: true, force: true })
    }
})

test('WAL observer rejects ordinary profiles and measures only synthetic file sizes', async () => {
    await assert.rejects(observeWal('C:/test/RisuNest', 'RisuNest'))
    const parent = await mkdtemp(path.join(os.tmpdir(), 'risunest-wal-test-'))
    const identifier = 'RisuNest.phase3benchmark.r0123456789ab.waltest'
    const root = path.join(parent, identifier)
    try {
        await mkdir(path.join(root, 'persistent'), { recursive: true })
        const finish = await observeWal(root, identifier)
        try {
            await writeFile(path.join(root, 'persistent/persistent.sqlite-wal'), new Uint8Array(8192))
            const measured = await finish()
            assert.equal(measured.startBytes, 0)
            assert.equal(measured.endBytes, 8192)
            assert.equal(measured.peakBytes, 8192)
            assert.deepEqual(await finish(), measured)
        } finally {
            await finish()
        }
    } finally {
        await rm(parent, { recursive: true, force: true })
    }
})

test('refuses ordinary and mismatched profiles before fixture IO', () => {
    assert.throws(() => assertSyntheticProfile('C:/test/RisuNest', 'RisuNest'))
    assert.throws(() =>
        assertSyntheticProfile(
            'C:/test/RisuNest',
            'RisuNest.phase3benchmark.r0123456789ab.fixture',
        ),
    )
    assert.doesNotThrow(() =>
        assertSyntheticProfile(
            'C:/test/RisuNest.phase3benchmark.r0123456789ab.fixture',
            'RisuNest.phase3benchmark.r0123456789ab.fixture',
        ),
    )
})

test('allowlisted projection removes body, path, identifier, and exception sentinels', () => {
    const sentinel = '__synthetic_private_sentinel__'
    const output = sanitizeMetrics({
        interactiveMs: 10,
        body: sentinel,
        firstRevision: sentinel,
        calls: [
            { command: sentinel, ms: 1 },
            { command: 'pds_open', ms: 2, path: sentinel, bytes: 15, success: true },
        ],
        operations: [{ stage: 'canonical', ms: 3, body: sentinel }],
        phases: [{ stage: sentinel, ms: 1 }],
        active: { [sentinel]: 1 },
        interaction: {
            selectionMs: 1,
            inputMs: sentinel,
            success: true,
            text: sentinel,
        },
    })
    assert.equal(JSON.stringify(output).includes(sentinel), false)
    assert.equal(output.calls.length, 1)
    assert.equal(output.calls[0].bytes, 15)
    assert.equal(output.interaction.inputMs, null)
})

test('nearest-rank P95 uses the nineteenth of twenty samples', () => {
    assert.equal(
        percentile(
            Array.from({ length: 20 }, (_, i) => i + 1),
            95,
        ),
        19,
    )
    assert.equal(percentile([], 95), null)
})

test('resource concurrency counts overlap without merging adjacent requests', () => {
    assert.equal(
        maxConcurrentResources([
            { start: 0, ms: 10 },
            { start: 5, ms: 10 },
        ]),
        2,
    )
    assert.equal(
        maxConcurrentResources([
            { start: 0, ms: 10 },
            { start: 10, ms: 10 },
        ]),
        1,
    )
    assert.equal(
        maxConcurrentResources([
            { start: 0, ms: 0 },
            { start: 1, ms: null },
        ]),
        0,
    )
})

test('synthetic PNG samples have distinct contents and real image dimensions', () => {
    const first = syntheticPng(0),
        second = syntheticPng(1)
    assert.notDeepEqual(first, second)
    assert.equal(first.readUInt32BE(16), 256)
    assert.equal(first.readUInt32BE(20), 256)
})

test('synthetic seed supplies ordered selectable characters and exact body references', async () => {
    let root
    const characters = []
    const invoke = async (command, args) => {
        if (command === 'pds_materialize') return { characters: [], botPresets: [] }
        if (command === 'pds_replace_begin') return { stagingId: 'synthetic-staging' }
        if (command === 'pds_replace_put_root') root = args.root
        if (command === 'pds_replace_add_characters') characters.push(...args.characters)
        if (command === 'pds_replace_preserve_repositories') return { revision: 1 }
    }
    const expression = seedExpression({
        characters: 3,
        bytes: 4096,
        pluginBytes: 32,
        referencedAssets: 10,
    })
    const result = await new Function('window', 'localStorage', `return ${expression}`)(
        { __TAURI_INTERNALS__: { invoke } },
        { setItem() {} },
    )
    assert.equal(result.success, true)
    assert.deepEqual(
        root.characterOrder,
        characters.map((c) => c.chaId),
    )
    assert.ok(characters.every((c) => Array.isArray(c.globalLore) && c.chats.length > 0))
    const references = characters.flatMap((c) => c.additionalAssets.map((a) => a[1]))
    assert.equal(references.length, 10)
    assert.equal(new Set(references).size, 10)
})

test('every measured source boundary exists in the current checkout', async () => {
    for (const file of [
        'storage/databasePreparation.ts',
        'storage/saveCoordinatorHelpers.ts',
        'storage/saveCoordinator.ts',
        'bootstrap.ts',
        'globalApi.svelte.ts',
    ]) {
        const source = await readFile(new URL('../../src/ts/' + file, import.meta.url), 'utf8')
        const transformed = instrumentSource(source, '/src/ts/' + file)
        assert.ok(
            transformed.includes('__startupRecord') ||
                transformed.includes('__startupTrackPromise'),
        )
    }
})


test('named agent roots reject the default, another profile, and a sibling before access', () => {
    const identifier = 'RisuNest.phase3benchmark.r0123456789ab.namedtest'
    const profile = syntheticProfile(identifier, path.resolve('synthetic-local'))
    assert.doesNotThrow(() => assertSyntheticProfile(profile.data, identifier, profile))
    for (const root of [path.dirname(profile.data), path.join(profile.localAppData, 'RisuNest-agent'),
        path.join(profile.localAppData, 'RisuNestProfiles-agent', 'agent-other', 'data')]) {
        assert.throws(() => assertAgentDataRoot(root, profile), /root mismatch/)
    }
    assert.throws(() => assertSyntheticProfile(profile.data, 'RisuNest', profile))
})

test('private artifact remains the built bytes when the shared target is overwritten', async () => {
    const temporary = await mkdtemp(path.join(os.tmpdir(), 'risunest-binary-test-'))
    try {
        const target = path.join(temporary, 'target')
        await mkdir(path.join(target, 'release'), { recursive: true })
        await writeFile(path.join(target, 'release/RisuNest.exe'), 'synthetic-build-A')
        const copied = await copyBenchmarkBinary(target, 'RisuNest', temporary)
        await writeFile(path.join(target, 'release/RisuNest.exe'), 'synthetic-build-B')
        assert.equal(await readFile(copied.binary, 'utf8'), 'synthetic-build-A')
        assert.ok(path.basename(copied.binary).includes(copied.sha256))
        assert.equal(resolveCargoTarget('/unused', target), path.resolve(target))
        assert.throws(() => resolveCargoTarget('/unused'), /CARGO_TARGET_DIR/)
        assert.throws(() => resolveCargoTarget('/unused', 'relative-target'), /CARGO_TARGET_DIR/)
    } finally { await rm(temporary, { recursive: true, force: true }) }
})

test('native root mismatch closes CDP without admitting page content reads', async (t) => {
    const profile = syntheticProfile('RisuNest.phase3benchmark.r0123456789ab.rootcheck', path.resolve('synthetic-local'))
    t.mock.method(globalThis, 'fetch', async () => ({ json: async () => [{ type: 'page', webSocketDebuggerUrl: 'ws://synthetic' }] }))
    const original = globalThis.WebSocket
    t.after(() => { globalThis.WebSocket = original })
    let socket
    const expressions = []
    globalThis.WebSocket = class {
        constructor() { socket = this; queueMicrotask(() => this.onopen()) }
        close() { this.closed = true }
        send(encoded) {
            const { id, method, params } = JSON.parse(encoded)
            if (method === 'Runtime.evaluate') expressions.push(params.expression)
            const value = params?.expression?.includes('app_paths_roots') ? { data: path.resolve('ordinary-profile') } : true
            queueMicrotask(() => this.onmessage({ data: JSON.stringify({ id, result: method === 'Runtime.evaluate' ? { result: { value } } : {} }) }))
        }
    }
    await assert.rejects(connectVerifiedIdentity(12345, 'synthetic', 1000, profile), /root mismatch/)
    assert.equal(socket.closed, true)
    assert.equal(expressions.length, 2)
    assert.ok(expressions[0].includes('plugin:app|identifier'))
    assert.ok(expressions[1].includes('app_paths_roots'))
})

test('startup network admission allows packaged resources and rejects remote and Realm requests', () => {
    for (const url of ['http://tauri.localhost/assets/main.js', 'http://ipc.localhost/app_paths_roots', 'asset://localhost/fixture.png', 'data:image/png;base64,AA==']) {
        assert.equal(isAllowedStartupUrl(url), true, url)
    }
    for (const url of ['https://example.invalid/', 'http://127.0.0.1:11435/api/generate', 'http://tauri.localhost/rs/hash',
        'https://sv.risuai.xyz/hub/realm/search', 'https://update.rsyumi.workers.dev/translator/prompt-presets.json']) {
        assert.equal(isAllowedStartupUrl(url), false, url)
    }
})

test('zero-plugin baseline seeds no legacy records or hidden plugin payload', async () => {
    let root
    const invoke = async (command, args) => {
        if (command === 'pds_materialize') return { characters: [], botPresets: [] }
        if (command === 'pds_replace_begin') return { stagingId: 'synthetic' }
        if (command === 'pds_replace_put_root') root = args.root
        if (command === 'pds_replace_preserve_repositories') return { revision: 1 }
    }
    const deviceSource = await readFile(new URL('./android-device.mjs', import.meta.url), 'utf8')
    const file = ts.createSourceFile('android-device.mjs', deviceSource, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS)
    let deviceOptions
    const visit = node => {
        if (ts.isCallExpression(node) && node.expression.getText(file) === 'seedExpression')
            deviceOptions = new Function('return (' + node.arguments[0].getText(file) + ')')()
        ts.forEachChild(node, visit)
    }
    visit(file)
    assert.ok(deviceOptions)
    for (const options of [{ zeroPlugins: true }, deviceOptions]) {
        const result = await new Function('window', 'localStorage', 'return ' + seedExpression({
            ...options, characters: 2, bytes: 4096, images: false,
        }))({ __TAURI_INTERNALS__: { invoke } }, { setItem() {} })
        assert.equal(result.success, true)
        assert.deepEqual(root.plugins, [])
        assert.deepEqual(root.pluginCustomStorage, {})
        assert.ok(result.measuredBytes < 10000)
    }
})

function memorySample(atMs, working = 100, privateBytes = 200) {
    return { requestAtMs: Math.max(0, atMs - 10), atMs,
        nativeStartMs: atMs + 100, snapshotStartMs: atMs + 101, snapshotEndMs: atMs + 102, nativeEndMs: atMs + 103,
        enumerationMs: 1, snapshotCount: 1,
        complete: true, errors: [], knownCount: 3, queriedCount: 3, exitedCount: 0, identityRefusedCount: 0,
        nativeWorkingSetBytes: 10, nativePrivateBytes: 20, treeWorkingSetBytes: working, treePrivateBytes: privateBytes, processCount: 3 }
}

function incompleteMemorySample(atMs = 10, code = 'counter-query-failed', win32Error = 5) {
    const sample = { ...memorySample(atMs), complete: false, queriedCount: 2, errors: [{ code, win32Error }] }
    for (const key of ['nativeWorkingSetBytes', 'nativePrivateBytes', 'treeWorkingSetBytes', 'treePrivateBytes', 'processCount']) sample[key] = null
    return sample
}

function memoryProcess({ ready = Promise.resolve(), onSample = index => memorySample(index * 10, index === 1 ? 300 : 80) } = {}) {
    const child = new EventEmitter()
    child.stdout = new PassThrough()
    let samples = 0
    const reply = value => queueMicrotask(() => child.stdout.write(JSON.stringify(value) + '\n'))
    child.stdin = new Writable({ write(chunk, encoding, done) {
        const request = chunk.toString().trim()
        if (request === 'stop') {
            child.stopped = true
            queueMicrotask(() => { child.stdout.end(); child.emit('exit', 0) })
        } else if (/^bind [1-9][0-9]* [1-9][0-9]*$/.test(request)) reply({ kind: 'bound' })
        else if (request === 'sample') {
            const sample = onSample(++samples)
            if (sample !== null) reply({ ...sample, kind: 'sample' })
        }
        else { done(new Error('Unexpected synthetic request')); return }
        done()
    } })
    void ready.then(() => reply({ kind: 'ready', protocol: 2, nativeFrequency: 10_000_000 }))
    return child
}

test('sampled peaks use whole sweeps and an incomplete sweep invalidates the observation', () => {
    const samples = [memorySample(0, 100, 250), memorySample(120, 300, 200), memorySample(900, 80, 90)]
    const result = summarizeMemorySamples(samples, 100)
    assert.equal(result.sampledPeak.treeWorkingSetBytes, 300)
    assert.equal(result.sampledPeak.treePrivateBytes, 250)
    assert.equal(result.settled.treeWorkingSetBytes, 80)
    assert.equal(result.maxGapMs, 780)
    assert.equal(result.maxNativeStartGapMs, 780)
    assert.deepEqual(result.firstObservationBoundsMs, { earliest: 0, latest: 0 })
    const failed = incompleteMemorySample(1000)
    const incomplete = summarizeMemorySamples([...samples, failed], 100)
    assert.equal(incomplete.coverageComplete, false)
    assert.equal(incomplete.sampledPeak, null)
    assert.equal(incomplete.observedCompleteSweepMax.treeWorkingSetBytes, 300)
    assert.equal(incomplete.series.at(-1).errors[0].win32Error, 5)
    assert.throws(() => summarizeMemorySamples([{ ...failed, treeWorkingSetBytes: 0 }], 100), /partial counters/)
    assert.throws(() => summarizeMemorySamples([], 100), /No owned/)
    assert.throws(() => summarizeMemorySamples([memorySample(120), memorySample(0)], 100), /backwards/)
})

test('memory preparation waits for readiness and one helper supplies the final sample', async () => {
    let releaseReady
    const ready = new Promise(resolve => { releaseReady = resolve })
    let child, spawned = 0, admitted = false
    const pending = prepareWindowsMemory({ spawnProcess() { spawned++; child = memoryProcess({ ready }); return child } })
    void pending.then(() => { admitted = true })
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(admitted, false)
    releaseReady()
    const prepared = await pending
    const finish = await prepared.bind(42, { launchAtMs: performance.now(), anchor: 'launch' })
    const result = await finish()
    assert.equal(spawned, 1)
    assert.equal(child.stopped, true)
    assert.equal(result.preparedBeforeLaunch, true)
    assert.equal(result.sampledPeak.treeWorkingSetBytes, 300)
    assert.equal(result.settled.treeWorkingSetBytes, 80)
    assert.equal(result.samples, 2)
    assert.ok(result.firstObservationBoundsMs.latest >= result.firstObservationBoundsMs.earliest)
})

test('an incomplete native response retains its errors and never publishes a peak', async () => {
    let child
    const finish = await observeWindowsMemory(42, { spawnProcess() {
        child = memoryProcess({ onSample: () => incompleteMemorySample(10, 'missing-from-snapshot', 0) })
        return child
    } })
    const result = await finish()
    assert.equal(result.coverageComplete, false)
    assert.equal(result.sampledPeak, null)
    assert.equal(result.observedCompleteSweepMax, null)
    assert.equal(result.anchor, 'existing-process')
    assert.equal(child.stopped, true)
})

test('helper exit and timeout retain an earlier incomplete sweep receipt', async () => {
    let child
    const exited = await observeWindowsMemory(42, { spawnProcess() {
        child = memoryProcess({ onSample: () => incompleteMemorySample() })
        return child
    } })
    child.stdout.end()
    child.emit('exit', 1)
    const failed = await exited()
    assert.equal(failed.terminalFailure.code, 'helper-exited')
    assert.equal(failed.coverageComplete, false)
    assert.equal(failed.sampledPeak, null)
    assert.equal(failed.series.length, 1)
    assert.equal(failed.first.errors[0].win32Error, 5)
    assert.equal(child.stopped, true)

    const timedOut = await observeWindowsMemory(42, { timeoutMs: 10, spawnProcess() {
        child = memoryProcess({ onSample: index => index === 1 ? incompleteMemorySample() : null })
        return child
    } })
    const timeout = await timedOut()
    assert.equal(timeout.terminalFailure.code, 'response-timeout')
    assert.equal(timeout.series.length, 1)
    assert.equal(timeout.first.errors[0].code, 'counter-query-failed')
    assert.equal(child.stopped, true)
})

test('the actual launcher waits for preparation and times readiness before sampler binding completes', async () => {
    const text = await readFile(new URL('./profile.mjs', import.meta.url), 'utf8')
    const file = ts.createSourceFile('profile.mjs', text, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS)
    let arrow
    const visit = node => {
        if (ts.isVariableDeclaration(node) && node.name.getText(file) === 'launch') arrow = node.initializer.getText(file)
        ts.forEachChild(node, visit)
    }
    visit(file)
    assert.ok(arrow)
    const factory = new Function('prepareWindowsMemory', 'performance', 'spawn', 'once', 'connect', 'waitForInteractive',
        'observation', 'assertObservationIdentity', 'owned',
        'let child, client, launchEpoch; const artifact={binary:"synthetic.exe",observation},config={identifier:"synthetic"},profile={name:"agent-synthetic"},temporary="synthetic",env={},cdpPort=1; return (' + arrow + ');')
    let releaseReady, releaseBinding, appStarted = false, clock = 0, app
    const preparation = new Promise(resolve => { releaseReady = resolve })
    const binding = new Promise(resolve => { releaseBinding = resolve })
    const owned = new WeakSet()
    const launch = factory(() => preparation, { now: () => clock, timeOrigin: 0 },
        () => { appStarted = true; app = new EventEmitter(); app.pid = 42; return app },
        once, async () => ({ evaluate: async () => { clock = 80; return { ipcObservation: observationManifest(), observerFailure: null } } }),
        async () => { clock = 40 }, observationManifest(), assertObservationIdentity, owned)
    const result = launch(true)
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(appStarted, false)
    releaseReady({ bind: () => binding, dispose: async () => {} })
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(appStarted, true)
    assert.equal(owned.has(app), true)
    clock = 500
    releaseBinding(async () => {})
    assert.equal((await result).readyMs, 40)
    app.emit('exit', 0)

    const observation = summarizeMemorySamples([incompleteMemorySample()], 100)
    const connectionFailure = new Error('Synthetic connection failure')
    const disconnected = factory(async () => ({ bind: async () => async () => observation, dispose: async () => {} }),
        { now: () => 0, timeOrigin: 0 },
        () => { app = new EventEmitter(); app.pid = 42; return app }, once,
        async () => { throw connectionFailure }, async () => {}, undefined, undefined, owned)
    await assert.rejects(disconnected(true), error => error === connectionFailure
        && error.memoryObservation.series === observation.series && !error.memoryObservation.coverageComplete
        && error.memoryObservation.sampledPeak === null)
    assert.equal(owned.has(app), true)
    app.emit('exit', 0)

    appStarted = false
    const failure = new Error('Synthetic preparation failure')
    const failedLaunch = factory(async () => { throw failure }, { now: () => 0, timeOrigin: 0 },
        () => { appStarted = true }, once, async () => ({}), async () => {}, undefined, undefined, owned)
    await assert.rejects(failedLaunch(true), error => error === failure)
    assert.equal(appStarted, false)
})

test('the actual app stop uses only its original owned child and confirms exit', async () => {
    const text = await readFile(new URL('./profile.mjs', import.meta.url), 'utf8')
    const file = ts.createSourceFile('profile.mjs', text, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS)
    const definition = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'stop')
    assert.ok(definition)
    // The product rejects a command that no Windows main-window capability grants.
    const capabilities = new URL('../../src-tauri/capabilities/', import.meta.url)
    const granted = new Set()
    for (const name of (await readdir(capabilities)).filter(name => name.endsWith('.json'))) {
        const capability = JSON.parse(await readFile(new URL(name, capabilities), 'utf8'))
        if (capability.windows?.includes('main') && (!capability.platforms || capability.platforms.includes('windows')))
            for (const permission of capability.permissions) if (typeof permission === 'string') granted.add(permission)
    }
    // Tauri 2.12 includes core:event:allow-emit-to in core:event:default, which core:default includes.
    const sets = { 'event|emit_to': ['core:event:allow-emit-to', 'core:event:default', 'core:default'] }
    const commands = [...definition.getText(file).matchAll(/plugin:([a-z]+)\|([a-z_-]+)/g)].map(match => match[1] + '|' + match[2])
    assert.ok(commands.length)
    for (const command of commands) {
        const [plugin, name] = command.split('|')
        const accepted = sets[command] ?? [`core:${plugin}:allow-${name.replaceAll('_', '-')}`]
        assert.ok(accepted.some(permission => granted.has(permission)), `${command} is not granted on Windows`)
    }
    const factory = new Function('owned', 'stopped', 'once', 'delay', 'process', 'setTimeout', 'clearTimeout', 'run',
        'return (' + definition.getText(file) + ');')
    function scenario({ graceful = false, kill = 'exit', platform = 'win32', close = 'success' } = {}) {
        const owned = new WeakSet(), stopped = new WeakSet(), timers = new Map()
        const child = new EventEmitter()
        child.pid = 42; child.exitCode = null; child.signalCode = null
        let killed = 0, closed = 0, requested = 0, nextTimer = 0, numericKills = 0
        const killError = new Error('Synthetic original-handle failure')
        const closeError = new Error(close === 'prior-closed' ? 'CDP closed' : 'Synthetic close command failure')
        let rejectClose
        const finish = () => { child.exitCode = 0; child.emit('exit', 0) }
        child.kill = signal => {
            assert.equal(signal, 'SIGKILL')
            killed++
            if (kill === 'error') throw killError
            if (kill === 'exit') finish()
            if (kill === 'false-exit') { finish(); return false }
            return kill !== 'false'
        }
        owned.add(child)
        const client = {
            call: (method, params) => {
                assert.equal(method, 'Runtime.evaluate')
                // The product listens on its WebviewWindow; Tauri routes emit_to there only for these kinds.
                assert.match(params.expression, /plugin:event\|emit_to', \{target: \{kind: '(AnyLabel|WebviewWindow)', label: 'main'\}, event: 'tauri:\/\/close-requested'/)
                requested++
                if (graceful) finish()
                if (close === 'live-error' || close === 'prior-closed') return Promise.reject(closeError)
                if (close === 'pending' || close === 'unknown-after-exit') return new Promise((_, reject) => { rejectClose = reject })
                if (close === 'page-rejected') return Promise.resolve({ result: {}, exceptionDetails: {} })
                return Promise.resolve({ result: {} })
            },
            close: () => { closed++; rejectClose?.(close === 'unknown-after-exit' ? closeError : new Error('CDP closed')) },
        }
        const forbidden = () => { numericKills++; assert.fail('Numeric PID boundary used') }
        const stop = factory(owned, stopped, once, async () => {}, { platform, kill: forbidden },
            (callback, ms) => { const id = ++nextTimer; timers.set(id, { callback, ms }); return id },
            id => timers.delete(id), forbidden)
        return { child, client, owned, stopped, stop, finish, killError, closeError, timers,
            counts: () => ({ killed, closed, requested, numericKills }) }
    }
    async function advanceGrace(run) {
        await new Promise(resolve => setImmediate(resolve))
        const timer = [...run.timers.values()].find(timer => timer.ms === 5000)
        assert.ok(timer, 'The real stop must wait through its bounded grace period')
        timer.callback()
        await new Promise(resolve => setImmediate(resolve))
    }
    const graceful = scenario({ graceful: true })
    assert.deepEqual(await graceful.stop(graceful.child, graceful.client), { closeSettlement: 'response', forced: false })
    await graceful.stop(graceful.child, graceful.client)
    assert.deepEqual(graceful.counts(), { killed: 0, closed: 1, requested: 1, numericKills: 0 })
    assert.equal(graceful.child.listenerCount('exit'), 0)
    assert.equal(graceful.timers.size, 0)

    const pendingClose = scenario({ graceful: true, close: 'pending' })
    assert.deepEqual(await pendingClose.stop(pendingClose.child, pendingClose.client), { closeSettlement: 'own-post-exit-close', forced: false })
    assert.equal(pendingClose.timers.size, 0)
    assert.equal(pendingClose.child.listenerCount('exit'), 0)

    // An answered in-page rejection, such as a denied command, still fails after the owned exit.
    for (const close of ['live-error', 'prior-closed', 'page-rejected']) {
        const brokenClose = scenario({ close })
        const rejectedClose = assert.rejects(brokenClose.stop(brokenClose.child, brokenClose.client), close === 'page-rejected'
            ? /Synthetic page evaluation failed/ : error => error === brokenClose.closeError)
        await advanceGrace(brokenClose)
        await rejectedClose
        assert.equal(brokenClose.counts().killed, 1)
        assert.equal(brokenClose.counts().closed, 1)
        assert.equal(brokenClose.stopped.has(brokenClose.child), false)
        assert.equal(brokenClose.timers.size, 0)
        assert.equal(brokenClose.child.listenerCount('exit'), 0)
    }
    const unknownClose = scenario({ graceful: true, close: 'unknown-after-exit' })
    await assert.rejects(unknownClose.stop(unknownClose.child, unknownClose.client), error => error === unknownClose.closeError)
    assert.equal(unknownClose.counts().killed, 0)
    assert.equal(unknownClose.timers.size, 0)
    const foreignClosed = scenario({ graceful: true, close: 'prior-closed' })
    await assert.rejects(foreignClosed.stop(foreignClosed.child, foreignClosed.client), error => error === foreignClosed.closeError)
    assert.equal(foreignClosed.counts().killed, 0)
    assert.equal(foreignClosed.timers.size, 0)

    const forced = scenario()
    const decoy = new EventEmitter()
    decoy.pid = forced.child.pid
    decoy.kill = () => { throw new Error('Unrelated reused PID was targeted') }
    await assert.rejects(forced.stop(decoy, forced.client), /unowned/)
    assert.equal(forced.counts().requested, 0)
    const forcedStop = forced.stop(forced.child, forced.client)
    await advanceGrace(forced)
    assert.deepEqual(await forcedStop, { closeSettlement: 'response', forced: true })
    assert.deepEqual(forced.counts(), { killed: 1, closed: 1, requested: 1, numericKills: 0 })
    assert.equal(forced.child.listenerCount('exit'), 0)
    assert.equal(forced.timers.size, 0)

    const exited = scenario()
    exited.child.signalCode = 'SIGKILL'
    assert.deepEqual(await exited.stop(exited.child, exited.client), { closeSettlement: 'already-exited', forced: false })
    assert.equal(exited.counts().killed, 0)
    assert.equal(exited.child.listenerCount('exit'), 0)

    const raced = scenario({ kill: 'false-exit' })
    const racedStop = raced.stop(raced.child, raced.client)
    await advanceGrace(raced)
    await racedStop
    assert.equal(raced.stopped.has(raced.child), true)

    const failed = scenario({ kill: 'error' })
    const failedStop = assert.rejects(failed.stop(failed.child, failed.client), error => error === failed.killError)
    await advanceGrace(failed)
    await failedStop
    assert.equal(failed.counts().closed, 1)
    assert.equal(failed.stopped.has(failed.child), false)
    assert.equal(failed.child.listenerCount('exit'), 0)
    assert.equal(failed.child.listenerCount('error'), 0)
    assert.equal(failed.timers.size, 0)

    const earlyError = scenario({ close: 'pending' })
    const originalError = new Error('Synthetic child error during graceful wait')
    const earlyRejected = assert.rejects(earlyError.stop(earlyError.child, earlyError.client), error => error === originalError)
    await new Promise(resolve => setImmediate(resolve))
    assert.equal([...earlyError.timers.values()][0].ms, 5000)
    earlyError.child.emit('error', originalError)
    await earlyRejected
    assert.equal(earlyError.timers.size, 0)
    assert.equal(earlyError.counts().closed, 1)
    assert.equal(earlyError.child.listenerCount('exit'), 0)
    assert.equal(earlyError.child.listenerCount('error'), 0)
    assert.equal(earlyError.stopped.has(earlyError.child), false)

    const missingExit = scenario({ kill: 'false' })
    const pending = missingExit.stop(missingExit.child, missingExit.client)
    const rejected = assert.rejects(pending, /exit timed out/)
    await advanceGrace(missingExit)
    assert.equal(missingExit.stopped.has(missingExit.child), false)
    assert.equal(missingExit.timers.size, 1)
    const timeout = missingExit.timers.values().next().value
    assert.equal(timeout.ms, 15_000)
    timeout.callback()
    await rejected
    assert.equal(missingExit.counts().closed, 1)
    assert.equal(missingExit.child.listenerCount('exit'), 0)
    assert.equal(missingExit.child.listenerCount('error'), 0)
    assert.equal(missingExit.timers.size, 0)

    const unsupported = scenario({ platform: 'linux' })
    const unsupportedStop = assert.rejects(unsupported.stop(unsupported.child, unsupported.client), /requires Windows/)
    await advanceGrace(unsupported)
    await unsupportedStop
    assert.equal(unsupported.counts().killed, 0)
    assert.equal(unsupported.child.listenerCount('exit'), 0)
})

test('the actual measurement persists collected diagnostics before rethrowing a CDP failure', async () => {
    const text = await readFile(new URL('./profile.mjs', import.meta.url), 'utf8')
    const file = ts.createSourceFile('profile.mjs', text, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS)
    let arrow
    const visit = node => {
        if (ts.isVariableDeclaration(node) && node.name.getText(file) === 'measure') arrow = node.initializer.getText(file)
        ts.forEachChild(node, visit)
    }
    visit(file)
    assert.ok(arrow)
    const factory = new Function('observeWal', 'observeWindowsMemory', 'client', 'samples', 'persist',
        'const options={hostMetrics:"true"},profileRoot="synthetic",config={identifier:"synthetic"},profile={},child={pid:42},scenario={mode:"synthetic"}; return (' + arrow + ');')
    for (const sample of [memorySample(10), incompleteMemorySample()]) {
        const observation = summarizeMemorySamples([sample], 100)
        const records = []
        let persisted = false
        const failure = new Error('Synthetic measurement failure')
        const measure = factory(async () => async () => {}, async () => async () => observation,
            { evaluate: async () => { throw failure } }, records, async () => { persisted = true })
        await assert.rejects(measure('reload'), error => error === failure)
        assert.equal(persisted, true)
        assert.equal(records.length, 1)
        assert.equal(records[0].status, 'failed')
        assert.deepEqual(records[0].host.memory.series, observation.series)
        assert.equal(records[0].host.memory.coverageComplete, false)
        assert.equal(records[0].host.memory.sampledPeak, null)
        assert.deepEqual(records[0].host.memory.observedCompleteSweepMax, observation.observedCompleteSweepMax)
    }
})

test('native collector preserves ownership and rejects incomplete counter sweeps', { skip: process.platform !== 'win32' }, async () => {
    const text = (await readFile(new URL('./host-memory.ps1', import.meta.url), 'utf8')).replaceAll('\r\n', '\n')
    const marker = "$collectorSource = @'\n"
    const start = text.indexOf(marker) + marker.length
    const end = text.indexOf("\n'@", start)
    assert.ok(start >= marker.length && end > start)
    const contracts = String.raw`namespace RisuNestStartupMemory {
    using System;
    using System.Collections.Generic;
    using System.ComponentModel;
    sealed class FakeLease : ProcessLease {
        public long Identity;
        public bool Live = true, Deny, DieOnRead, Disposed;
        public int Disposals, Reads;
        public Memory Value;
        public FakeLease(long identity, long working, long privateBytes) { Identity = identity; Value = new Memory(working, privateBytes, working * 10); }
        public override long Created { get { return Identity; } }
        public override bool Alive { get { return Live; } }
        public override Memory Read() { Reads++; if (Deny) throw new Win32Exception(5); if (DieOnRead) Live = false; return Value; }
        public override void Dispose() { Disposed = true; Disposals++; }
    }
    sealed class FakeSource : ProcessSource {
        public List<Relation> Rows = new List<Relation>();
        public Dictionary<uint, FakeLease> Processes = new Dictionary<uint, FakeLease>();
        public bool SnapshotDenied;
        public Action AfterSnapshot;
        double clock;
        public double Now { get { return ++clock; } }
        public List<Relation> Snapshot() {
            if (SnapshotDenied) throw new Win32Exception(5);
            List<Relation> rows = new List<Relation>(Rows);
            Action after = AfterSnapshot; AfterSnapshot = null;
            if (after != null) after();
            return rows;
        }
        public ProcessLease Open(uint pid) { return Processes[pid]; }
        public FakeLease Add(uint pid, uint parent, long created, long working) {
            FakeLease process = new FakeLease(created, working, working * 10);
            Processes[pid] = process; Rows.Add(new Relation(pid, parent)); return process;
        }
    }
    public static class ContractCases {
        static void Require(bool pass, string reason) { if (!pass) throw new Exception(reason); }
        public static string Run() {
            Require(System.Runtime.InteropServices.Marshal.SizeOf(typeof(Native.Entry)) == (IntPtr.Size == 8 ? 568 : 556), "PROCESSENTRY32W ABI");
            Require(System.Runtime.InteropServices.Marshal.SizeOf(typeof(Native.Counters)) == (IntPtr.Size == 8 ? 80 : 44), "PROCESS_MEMORY_COUNTERS_EX ABI");
            FakeSource source = new FakeSource();
            FakeLease root = source.Add(1, 0, 10, 10), parent = source.Add(2, 1, 20, 20), child = source.Add(3, 2, 30, 30);
            using (Collector collector = new Collector(source)) {
                collector.Bind(1);
                Sample first = collector.Read();
                Require(first.Complete && first.TreeWorkingSetBytes == 60 && first.TreePrivateBytes == 600, "current tree counters, not process peaks");
                parent.Live = false; source.Rows.RemoveAll(row => row.Id == 2);
                Sample retained = collector.Read();
                Require(retained.Complete && retained.TreeWorkingSetBytes == 40 && parent.Disposed, "retain known descendants after parent exit");
                child.Deny = true;
                Sample denied = collector.Read();
                Require(!denied.Complete && denied.TreeWorkingSetBytes == null && denied.Errors.Exists(error => error.Win32Error == 5), "explicit transient access failure");
                child.Deny = false;
                Require(collector.Read().Complete, "retry current counters without stale substitution");
                source.Rows.RemoveAll(row => row.Id == 3);
                Sample missing = collector.Read();
                Require(!missing.Complete && missing.Errors.Exists(error => error.Code == "missing-from-snapshot"), "known live member absent from snapshot");
                source.Rows.Add(new Relation(3, 2));
                child.DieOnRead = true;
                Sample exited = collector.Read();
                Require(!exited.Complete && exited.TreePrivateBytes == null, "exit inside a sweep is incomplete");
            }
            Require(root.Disposals == 1 && parent.Disposals == 1 && child.Disposals == 1, "all owned handles released once");

            source = new FakeSource(); root = source.Add(1, 0, 10, 10); source.Add(2, 1, 20, 20);
            FakeLease foreign = new FakeLease(50, 999, 999);
            source.AfterSnapshot = () => { source.Processes[2] = foreign; source.Rows[1] = new Relation(2, 999); };
            using (Collector collector = new Collector(source)) {
                collector.Bind(1);
                Sample raced = collector.Read();
                Require(!raced.Complete && raced.TreeWorkingSetBytes == null && raced.IdentityRefusedCount == 1, "newer PID reused between snapshot and open");
                Require(foreign.Reads == 0 && foreign.Disposals == 1, "foreign process counters never read and opened handle released");
            }

            source = new FakeSource();
            root = source.Add(1, 0, 10, 10);
            FakeLease original = source.Add(2, 1, 20, 20);
            using (Collector collector = new Collector(source)) {
                collector.Bind(1); Require(collector.Read().Complete, "initial identity");
                original.Live = false;
                FakeLease reused = new FakeLease(5, 999, 999);
                source.Processes[2] = reused;
                Sample refused = collector.Read();
                Require(!refused.Complete && refused.TreeWorkingSetBytes == null && refused.IdentityRefusedCount == 1, "PID identity refusal");
                Require(original.Disposed && reused.Disposed, "old and refused handles released");
            }
            source = new FakeSource(); root = source.Add(1, 0, 10, 10);
            using (Collector collector = new Collector(source)) {
                collector.Bind(1); source.SnapshotDenied = true;
                Sample failed = collector.Read();
                Require(!failed.Complete && failed.TreePrivateBytes == null && failed.Errors.Exists(error => error.Win32Error == 5), "snapshot failure is not zero memory");
            }
            Require(root.Disposed, "failure cleanup");
            source = new FakeSource(); root = source.Add(1, 0, 10, 10);
            using (Collector collector = new Collector(source)) {
                bool failed = false;
                try { collector.Bind(1, 999); } catch (InvalidOperationException) { failed = true; }
                Require(failed && root.Disposed, "refuse a PID not created by the owning driver");
            }
            source = new FakeSource(); root = source.Add(1, 0, 10, 10); root.Live = false;
            using (Collector collector = new Collector(source)) {
                bool failed = false;
                try { collector.Bind(1); } catch (InvalidOperationException) { failed = true; }
                Require(failed && root.Disposed, "exited root binding cleanup");
            }
            return "native-contracts-passed";
        }
    }
}`
    const temporary = await mkdtemp(path.join(os.tmpdir(), 'risunest-native-memory-contracts-'))
    try {
        const script = path.join(temporary, 'contracts.ps1')
        await writeFile(script, "$ErrorActionPreference='Stop'\n$source = @'\n" + text.slice(start, end) + '\n' + contracts
            + "\n'@\nAdd-Type -TypeDefinition $source\n[RisuNestStartupMemory.ContractCases]::Run()\n")
        const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', script],
            { encoding: 'utf8', windowsHide: true, timeout: 30_000 })
        assert.equal(result.status, 0, result.stderr)
        assert.match(result.stdout, /native-contracts-passed/)
    } finally { await rm(temporary, { recursive: true, force: true }) }
})

test('WAL observation accepts the verified named data root and rejects another profile', async () => {
    const temporary = await mkdtemp(path.join(os.tmpdir(), 'risunest-named-wal-'))
    const identifier = 'RisuNest.phase3benchmark.r0123456789ab.namedwal'
    const profile = syntheticProfile(identifier, temporary)
    try {
        await mkdir(path.join(profile.data, 'persistent'), { recursive: true })
        const finish = await observeWal(profile.data, identifier, profile)
        await writeFile(path.join(profile.data, 'persistent/persistent.sqlite-wal'), new Uint8Array(4096))
        const result = await finish()
        assert.equal(result.endBytes, 4096)
        await assert.rejects(observeWal(path.join(temporary, 'ordinary'), identifier, profile), /root mismatch/)
    } finally { await rm(temporary, { recursive: true, force: true }) }
})


test('zero-asset native profiles need no legacy asset directory', async () => {
    const temporary = await mkdtemp(path.join(os.tmpdir(), 'risunest-zero-assets-'))
    const identifier = 'RisuNest.phase3benchmark.r0123456789ab.zeroassets'
    const profile = syntheticProfile(identifier, temporary)
    try {
        await mkdir(profile.data, { recursive: true })
        assert.equal(await countSyntheticAssetEntries(profile.data, identifier, profile), 0)
        await mkdir(path.join(profile.data, 'assets'))
        await writeFile(path.join(profile.data, 'assets/synthetic.bin'), 'synthetic')
        assert.equal(await countSyntheticAssetEntries(profile.data, identifier, profile), 1)
        await rm(path.join(profile.data, 'assets'), { recursive: true })
        await writeFile(path.join(profile.data, 'assets'), 'synthetic-file-instead-of-directory')
        await assert.rejects(countSyntheticAssetEntries(profile.data, identifier, profile))
    } finally { await rm(temporary, { recursive: true, force: true }) }
})
