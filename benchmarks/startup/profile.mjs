import { spawn, execFileSync } from 'node:child_process'
import { once } from 'node:events'
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { buildBenchmarkConfig } from '../phase3/tauri-cdp.mjs'
import { connect, delay, waitForInteractive } from './cdp.mjs'
import { sanitizeMetrics, assertObservationIdentity, assertCompleteObservation, OBSERVATION_MANIFEST } from './metrics.mjs'
import { readObservationInputs, verifyObservationManifest } from './observe.mjs'
import { observeWal, observeWindowsMemory, prepareWindowsMemory } from './host-metrics.mjs'
import { runOnPrivateDesktop } from './background.mjs'
import { addSyntheticAssets, assertSyntheticProfile, seedExpression, countSyntheticAssetEntries } from './fixture.mjs'
import { resolveCargoTarget, syntheticProfile, copyBenchmarkBinary } from './runtime.mjs'

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const options = Object.fromEntries(
    process.argv.slice(2).map((arg) => {
        const [key, value = 'true'] = arg.replace(/^--/, '').split('=')
        return [key, value]
    }),
)
if (options.suite || options.mode && options.mode !== 'no-plugins' || options.mutation === 'true') {
    throw new Error('Obsolete API 2.1 comparison removed; this runner measures the no-plugins baseline')
}
if (options.fresh) throw new Error('Profile reset is unsupported; start a new isolated run')
const runs = Number(options.runs ?? 5)
const assets = Number(options.assets ?? 0)
const characters = Number(options.characters ?? 2)
const bytes = Number(options.bytes ?? 1_048_576)
const legacyAssets = Number(options.legacyAssets ?? 0)
const referencedAssets = Number(options.referencedAssets ?? 0)
const images = options.images === 'true'
for (const value of [runs, assets, characters, bytes, legacyAssets, referencedAssets]) {
    if (!Number.isSafeInteger(value) || value < 0) throw new Error('Invalid numeric option')
}
if (referencedAssets > (options.suite ? 11345 : assets) || (images && assets < 16)) {
    throw new Error('Synthetic body references require matching asset objects')
}

async function run(command, args, env, output = 'ignore') {
    const child = spawn(command, args, {
        cwd: repository,
        env,
        windowsHide: true,
        stdio: output,
    })
    const [code] = await once(child, 'exit')
    if (code !== 0) throw new Error('Benchmark child failed')
}

const stopped = new WeakSet()
const owned = new WeakSet()
async function stop(child, client) {
    if (!child) return
    if (!owned.has(child)) throw new Error('Refusing to stop an unowned app')
    if (stopped.has(child)) return
    const hasExited = () => child.exitCode !== null || child.signalCode !== null
    if (hasExited()) {
        client?.close()
        stopped.add(child)
        return { closeSettlement: 'already-exited', forced: false }
    }
    const controller = new AbortController()
    const exited = once(child, 'exit', { signal: controller.signal })
    void exited.catch(() => {})
    let graceTimer, timer, failure, closePending = false, ownCloseFlush = false, closeSettlement = 'not-requested', forced = false
    try {
        if (client) {
            // The event Tauri emits to the main window on a native close request, so the product's
            // close drain saves, checkpoints and ends in destroy before the next process.
            closePending = true
            client.call('Runtime.evaluate', {
                expression: "window.__TAURI_INTERNALS__.invoke('plugin:event|emit_to', {target: {kind: 'AnyLabel', label: 'main'}, event: 'tauri://close-requested', payload: null})",
                awaitPromise: true,
                returnByValue: true,
            }).then(result => {
                closePending = false
                closeSettlement = 'response'
                if (!result || result.exceptionDetails) failure ??= new Error('Synthetic page evaluation failed')
            }, error => {
                closePending = false
                if (ownCloseFlush && hasExited() && error instanceof Error && error.message === 'CDP closed') {
                    closeSettlement = 'own-post-exit-close'
                } else {
                    closeSettlement = 'failed'
                    failure ??= error
                }
            })
            await Promise.race([exited, new Promise(resolve => {
                graceTimer = setTimeout(resolve, 5000)
            })])
            clearTimeout(graceTimer)
        }
        if (!hasExited()) {
            if (process.platform !== 'win32') throw new Error('Owned app fallback requires Windows')
            // The spawned child's Windows handle identifies its original creation.
            forced = true
            child.kill('SIGKILL')
        }
        if (!hasExited()) await Promise.race([
            exited,
            new Promise((_, reject) => {
                timer = setTimeout(() => reject(new Error('Owned app exit timed out')), 15_000)
            }),
        ])
        if (!hasExited()) throw new Error('Owned app exit was not confirmed')
    } catch (error) {
        failure ??= error
    } finally {
        clearTimeout(graceTimer)
        clearTimeout(timer)
        controller.abort()
        // Observe an existing direct-RPC rejection before identifying our own close.
        await Promise.resolve()
        ownCloseFlush = hasExited() && closePending
        try { client?.close() } catch (error) { failure ??= error }
        await Promise.resolve()
        ownCloseFlush = false
        if (closePending) failure ??= new Error('Owned app close RPC did not settle')
    }
    if (failure !== undefined) throw failure
    stopped.add(child)
    return { closeSettlement, forced }
}

async function main() {
    if (options.kind && !['reload', 'restart'].includes(options.kind))
        throw new Error('Invalid measurement kind')
    const server = net.createServer()
    server.listen(0, '127.0.0.1')
    await once(server, 'listening')
    const port = server.address().port
    await new Promise((resolve) => server.close(resolve))
    const temporary = options.config
        ? path.dirname(path.resolve(options.config))
        : await mkdtemp(path.join(os.tmpdir(), 'risunest-startup-'))
    const revision = execFileSync(
        'git',
        ['-c', `safe.directory=${repository.replaceAll('\\', '/')}`, 'rev-parse', 'HEAD'],
        { cwd: repository, encoding: 'utf8' },
    ).trim()
    const original = JSON.parse(
        await readFile(path.join(repository, 'src-tauri/tauri.conf.json'), 'utf8'),
    )
    const config = options.config
        ? JSON.parse(await readFile(options.config, 'utf8'))
        : buildBenchmarkConfig(original, port, path.basename(temporary), revision)
    if (!config.identifier.startsWith('RisuNest.phase3benchmark.')) throw new Error('Unsafe config')
    const cdpPort = options.config
        ? Number(
              config.app.windows[0].additionalBrowserArgs.match(/--remote-debugging-port=(\d+)/)[1],
          )
        : port
    const configPath = path.join(temporary, 'tauri.startup.json')
    const agent = JSON.parse(
        await readFile(path.join(repository, 'src-tauri/tauri.agent.conf.json'), 'utf8'),
    )
    config.build = {
        ...config.build,
        ...agent.build,
        beforeBuildCommand: 'node benchmarks/startup/before-build.mjs',
        frontendDist: '../.tmp/startup-benchmark-dist',
    }
    // An absolute WebView directory prevents cwd-dependent reuse.
    config.app.windows[0].dataDirectory = path.join(temporary, 'webview')
    config.plugins = { ...config.plugins, risunest: { ...config.plugins?.risunest, agent: true } }
    if (!options.config || options.rebuild === 'true') await writeFile(configPath, JSON.stringify(config))
    const cargoTarget = resolveCargoTarget(repository, process.env.CARGO_TARGET_DIR)
    const profile = syntheticProfile(config.identifier, process.env.LOCALAPPDATA)
    const env = {
        ...process.env,
        CARGO_TARGET_DIR: cargoTarget,
        VITE_RISU_LEGAL_CONFIGURED: 'TRUE',
        STARTUP_BENCHMARK_VARIANT: options.variant ?? 'optimized',
    }
    const observationInputs = await readObservationInputs(repository, 'windows')
    if (!options.config || options.rebuild === 'true') {
        await run(
            process.execPath,
            [
                path.join(repository, 'node_modules/@tauri-apps/cli/tauri.js'),
                'build',
                '--no-bundle',
                '--ci',
                '--config',
                'src-tauri/tauri.agent.conf.json',
                '--config',
                configPath,
            ],
            env,
            'inherit',
        )
    }
    const binaryInfoFile = path.join(temporary, 'binary-info.local.json')
    let artifact
    if (options.config && options.rebuild !== 'true') {
        artifact = JSON.parse(await readFile(binaryInfoFile, 'utf8'))
        if (artifact.sourceRevision !== revision || !config.identifier.includes('.r' + revision.slice(0, 12) + '.')) {
            throw new Error('Reused benchmark binary source revision differs from checkout')
        }
        if (!/^[0-9a-f]{64}$/.test(artifact.sha256)) throw new Error('Invalid private benchmark hash')
        const expected = path.join(temporary, 'binary', original.mainBinaryName + '-' + artifact.sha256 + '.exe')
        if (artifact.binary !== expected || createHash('sha256').update(await readFile(expected)).digest('hex') !== artifact.sha256) {
            throw new Error('Private benchmark binary identity mismatch')
        }
    } else {
        const observation = verifyObservationManifest(JSON.parse(await readFile(
            path.join(repository, '.tmp/startup-benchmark-dist', OBSERVATION_MANIFEST), 'utf8',
        )), observationInputs)
        artifact = { ...await copyBenchmarkBinary(cargoTarget, original.mainBinaryName, temporary), sourceRevision: revision, observation }
        await writeFile(binaryInfoFile, JSON.stringify(artifact))
    }
    artifact.observation = verifyObservationManifest(artifact.observation, observationInputs)
    if (
        process.platform === 'win32' &&
        process.env.STARTUP_BENCHMARK_DISPLAY !== 'private-desktop' &&
        options.display !== 'foreground'
    ) {
        // Compilers already run without GUI windows. Keep their native worker
        // threads on the regular desktop; isolate only the actual app runner.
        const backgroundArgs = process.argv
            .slice(2)
            .filter((arg) => !arg.startsWith('--config=') && !arg.startsWith('--rebuild='))
        await runOnPrivateDesktop(
            [...backgroundArgs, `--config=${configPath}`],
            options.output ?? 'benchmarks/startup/result.local.json',
        )
        return
    }
    let child, client, launchEpoch
    const samples = []
    let fixture
    let seededStatistics,
        profileRoot,
        recentSnapshotReady = false
    const binarySha256 = artifact.sha256
    let scenario = {
        assets,
        mode: 'no-plugins',
        recentSnapshot: options.snapshot !== 'missing',
        interaction: options.interact === 'true',
    }
    const output = path.resolve(options.output ?? 'benchmarks/startup/result.local.json')
    await mkdir(path.dirname(output), { recursive: true })
    const persist = () =>
        writeFile(
            output,
            JSON.stringify(
                {
                    revision,
                    productPatch:
                        options.config && options.rebuild !== 'true'
                            ? 'verified-reused-binary'
                            : (options.variant ?? 'optimized'),
                    binarySha256,
                    binarySourceRevision: artifact.sourceRevision,
                    ipcObservation: artifact.observation,
                    display: process.env.STARTUP_BENCHMARK_DISPLAY ?? 'foreground',
                    fixture: {
                        ...fixture,
                        ...seededStatistics,
                        characters,
                        targetBytes: bytes,
                        legacyAssets,
                        referencedAssets,
                        bodyReferencedObjects: Math.max(
                            referencedAssets,
                            images ? Math.min(16, characters) : 0,
                        ),
                    },
                    samples,
                    profileIsolation: { named: true, nativeRootVerified: true },
                    measurementScope: 'full-product startup and synthetic selection/input/scroll; no generation or sync workload',
                    limitations: [
                        'WebView network admission starts at CDP attachment and permits packaged resources only; native network calls are outside CDP coverage.',
                        'No plugins are configured; this does not measure API v3 plugin execution or compatibility.',
                        'Process memory reports sampled live descendants and cannot establish complete GPU or detached/shared-process footprint.',
                        'OS caches were not cleared',
                        'Selected SDK invocation duration includes native queueing and transport retries; direct globals, plugin RPC and asynchronous channel/event deliveries are excluded.',
                        'Invocation bytes are unavailable; no serialized estimate or total wire-volume claim is made.',
                    ],
                },
                null,
                2,
            ),
        )
    const launch = async (sampleMemory = false) => {
        const preparedMemory = sampleMemory ? await prepareWindowsMemory() : null
        const started = performance.now()
        launchEpoch = performance.timeOrigin + started
        let memoryStarted = Promise.resolve(null)
        try {
            child = spawn(
                artifact.binary,
                ['--profile=' + profile.name],
                { cwd: temporary, env, windowsHide: true, stdio: 'ignore' },
            )
            owned.add(child)
            memoryStarted = preparedMemory
                ? preparedMemory.bind(child.pid, { launchAtMs: started, anchor: 'launch' })
                : Promise.resolve(null)
            memoryStarted.catch(() => {})
            client = await Promise.race([
            connect(cdpPort, config.identifier, 120_000, profile),
            once(child, 'exit').then(() => {
                throw new Error('Isolated app exited before connection')
            }),
        ])
            await waitForInteractive(client)
            const readyMs = performance.now() - started
            assertObservationIdentity(await client.evaluate(`({
                ipcObservation: window.__startupMetrics?.ipcObservation,
                observerFailure: window.__startupMetrics?.observerFailure,
            })`), artifact.observation)
            return { readyMs, finishMemory: await memoryStarted }
        } catch (error) {
            const finishMemory = await memoryStarted.catch(() => null)
            const observation = finishMemory ? await finishMemory() : null
            if (preparedMemory) await preparedMemory.dispose().catch(() => {})
            const failure = error instanceof Error ? error : new Error('Synthetic application readiness failed')
            if (observation) failure.memoryObservation = {
                ...observation, coverageComplete: false, sampledPeak: null,
                measurementFailure: { code: 'application-readiness-failed', phase: 'launch' },
            }
            throw failure
        }
    }
    const prepareNext = async () => {
        if (legacyAssets > 0) {
            assertSyntheticProfile(profileRoot, config.identifier, profile)
            const directory = path.join(profileRoot, 'assets')
            await mkdir(directory, { recursive: true })
            for (let i = 0; i < legacyAssets; i++)
                await writeFile(
                    path.join(directory, `startup-legacy-${i}.bin`),
                    'Synthetic legacy asset',
                )
        }
        await client.evaluate(`(async () => {
            const invoke = window.__TAURI_INTERNALS__.invoke;
            if (${!scenario.recentSnapshot || !recentSnapshotReady}) {
                const snapshots = await invoke('pds_snapshot_list');
                if (${scenario.recentSnapshot}) {
                    if (!snapshots.length) await invoke('pds_snapshot_create', {reason: 'manual'});
                } else {
                    for (const snapshot of snapshots) await invoke('pds_snapshot_delete', {path: snapshot.path});
                }
            }
            localStorage.setItem('startupInteract', '${scenario.interaction}');
            await invoke('pds_set_device_setting', {key: 'risuNestDeviceSettings', value: JSON.stringify({
                schema: 'risunest.device-settings/v1', performanceProfile: 'normal',
                androidKeepAliveDuringGeneration: false, nativeFileLogEnabled: false,
                generationHistoryLimitEnabled: false, generationHistoryLimitMultiplier: 2, messageSendKey: 'enter'
            })});
        })()`)
        recentSnapshotReady = scenario.recentSnapshot
    }
    const measure = async (kind, warmup = false) => {
        const finishWal =
            options.hostMetrics === 'true' ? await observeWal(profileRoot, config.identifier, profile) : null
        let finishMemory, memory, observationRecorded = false
        try {
            let launchToReadyMs = null
            if (kind === 'restart') {
                const launched = await launch(options.hostMetrics === 'true')
                launchToReadyMs = launched.readyMs
                finishMemory = launched.finishMemory
            } else {
                if (options.hostMetrics === 'true') finishMemory = await observeWindowsMemory(child.pid)
                const origin = await client.evaluate('performance.timeOrigin')
                await client.call('Page.reload')
                const navigationDeadline = Date.now() + 120_000
                while ((await client.evaluate('performance.timeOrigin')) === origin) {
                    if (Date.now() > navigationDeadline) throw new Error('Reload did not navigate')
                    await delay(50)
                }
                await waitForInteractive(client)
            }
            await delay(10_000)
            const deadline = Date.now() + 20_000
            let settled = false
            do {
                settled = await client.evaluate(`(() => {
                const s = window.__startupMetrics;
                if (!s) return true;
                return s.operations.some(o => o.stage === 'flush')
                    && !Object.values(s.active).some(n => n > 0)
                    && !s.calls.some(c => c.ms === null);
            })()`)
                if (settled) break
                await delay(250)
            } while (Date.now() < deadline)
            const sample = sanitizeMetrics(
                await client.evaluate(`(async () => ({
            ipcObservation: window.__startupMetrics?.ipcObservation,
            observerFailure: window.__startupMetrics?.observerFailure,
            interactiveMs: performance.getEntriesByName('boot:interactive')[0]?.startTime ?? null,
            firstPaintMs: performance.getEntriesByName('first-contentful-paint')[0]?.startTime ?? null,
            marks: performance.getEntriesByType('mark').filter(e => /^boot:[a-z-]+$/.test(e.name)).map(e => ({stage: e.name, ms: e.startTime})),
            calls: window.__startupMetrics?.calls ?? null,
            longTasks: window.__startupMetrics?.longTasks ?? null,
            operations: window.__startupMetrics?.operations,
            phases: window.__startupMetrics?.phases,
            active: window.__startupMetrics?.active,
            elapsedSeen: window.__startupMetrics?.elapsedSeen,
            elapsedVisibleAfterStartup: !!document.querySelector('.loading-progress > [aria-live="off"]'),
            interaction: window.__startupMetrics?.interaction,
            firstRevision: window.__startupMetrics?.firstRevision,
            lastRevision: (await window.__TAURI_INTERNALS__.invoke('pds_read_root')).revision,
            usedHeapBytes: performance.memory?.usedJSHeapSize ?? null,
            sampledPeakHeapBytes: window.__startupMetrics?.peakHeapBytes,
            maxMediaInFlight: window.__startupMetrics?.maxMediaInFlight,
            documentVisible: document.visibilityState === 'visible',
            cleanChunksReason: window.__startupMetrics?.cleanChunksReason,
            imageResources: performance.getEntriesByType('resource').filter(e => e.initiatorType === 'img').map(e => ({start: e.startTime, ms: e.duration})),
            stabilizationTimeout: ${!settled},
        }))()`),
            )
            const assetDirectoryEntriesAfter = await countSyntheticAssetEntries(profileRoot, config.identifier, profile)
            const paintEpoch =
                kind === 'restart'
                    ? await client.evaluate(`(() => {
                const paint = performance.getEntriesByName('first-contentful-paint')[0];
                return paint ? performance.timeOrigin + paint.startTime : null;
            })()`)
                    : null
            const launchToFirstPaintMs =
                Number.isFinite(paintEpoch) && Number.isFinite(launchEpoch)
                    ? paintEpoch - launchEpoch
                    : null
            const stopMemory = finishMemory
            finishMemory = null
            memory = stopMemory ? await stopMemory() : null
            samples.push({
                ...scenario,
                kind,
                warmup,
                launchToReadyMs,
                launchToFirstPaintMs,
                assetDirectoryEntriesAfter,
                ...sample,
                ...(finishWal
                    ? {
                          host: {
                              wal: await finishWal(),
                              memory,
                          },
                      }
                    : {}),
            })
            observationRecorded = true
            await persist()
            if (memory && !memory.coverageComplete) throw new Error('Owned process memory coverage incomplete')
            assertCompleteObservation(sample, artifact.observation)
            const opens = sample.calls.filter(call => call.command === 'pds_open')
            if (!opens.length || opens.some(call => !call.success)) throw new Error('Missing successful native startup open')
            if (
                warmup &&
                scenario.interaction &&
                (!sample.interaction?.success || sample.interaction.failure !== null)
            )
                throw new Error('Visible interaction warmup failed')
            console.log(
                JSON.stringify({
                    kind,
                    warmup,
                    interactiveMs: sample.interactiveMs,
                    mode: scenario.mode,
                    objects: scenario.assets,
                    selectionMs: sample.interaction?.selectionMs,
                    settled: !sample.stabilizationTimeout,
                }),
            )
        } catch (error) {
            if (finishMemory) {
                const finish = finishMemory
                finishMemory = null
                memory = await finish()
            }
            memory ??= error?.memoryObservation
            if (memory && !observationRecorded) {
                memory = {
                    ...memory, coverageComplete: false, sampledPeak: null,
                    measurementFailure: memory.measurementFailure ?? { code: 'measurement-failed', phase: 'measurement' },
                }
                samples.push({ ...scenario, kind, warmup, status: 'failed', failurePhase: 'measurement', host: { memory } })
                observationRecorded = true
                await persist()
            }
            throw error
        } finally {
            if (finishWal) await finishWal()
        }
    }
    // One line per stop records how the owned app ended, without page data.
    const stopApp = async after => {
        console.log(JSON.stringify({ phase: 'app-stopped', after, ...await stop(child, client) }))
    }
    try {
        console.log(JSON.stringify({ phase: 'initial-launch' }))
        await launch()
        const root = profile.data
        assertSyntheticProfile(root, config.identifier, profile)
        profileRoot = root
        await writeFile(
            path.join(temporary, 'profile-root.local.json'),
            JSON.stringify({ identifier: config.identifier, root }),
        )
        console.log(JSON.stringify({ phase: 'seed' }))
        const seeded = await client.evaluate(
            seedExpression({
                characters,
                bytes,
                zeroPlugins: true,
                pluginBytes: 0,
                images: options.images === 'true',
                investigation: options.shape === 'investigation',
                mutation: options.mutation === 'true',
                referencedAssets,
                coldStorage: options.coldStorage === 'true',
                locale: options.locale ?? 'en',
            }),
        )
        console.log(JSON.stringify({ phase: 'seed-result', ...seeded }))
        if (!seeded.success) throw new Error('Synthetic seed failed')
        seededStatistics = seeded
        await client.evaluate(
            `localStorage.setItem('startupInteract', ${JSON.stringify(options.interact === 'true' ? 'true' : 'false')})`,
        )
        await stopApp('seed')
        fixture = await addSyntheticAssets(
            root,
            config.identifier,
            assets,
            options.images === 'true',
            profile,
        )
        console.log(JSON.stringify({ phase: 'assets-ready', objects: fixture.objects }))
        const scenarios = [scenario]
        for (const next of scenarios) {
            scenario = next
            recentSnapshotReady = false
            // The initial fixture already has this count. Reopen the offline
            // store only when growing it, avoiding a redundant WAL checkpoint.
            if (fixture.objects !== scenario.assets) {
                fixture = await addSyntheticAssets(
                    root,
                    config.identifier,
                    scenario.assets,
                    options.images === 'true',
                    profile,
                )
            }
            console.log(
                JSON.stringify({
                    phase: 'mode-control',
                    mode: scenario.mode,
                    objects: scenario.assets,
                }),
            )
            await launch()
            // Let the setup launch's pending save finish before the measured restart.
            await delay(10_000)
            await prepareNext()
            await stopApp('setup')
            // Exclude normalization and the seed WAL checkpoint from measured samples.
            await measure('restart', true)
            await client.evaluate(
                `localStorage.setItem('startupInteract', ${JSON.stringify(options.suite || options.interact === 'true' ? 'true' : 'false')})`,
            )
            for (let i = 0; i < (options.kind === 'restart' ? 0 : runs); i++) {
                await prepareNext()
                await measure('reload')
            }
            await prepareNext()
            await stopApp('warmup')
            for (let i = 0; i < (options.kind === 'reload' ? 0 : runs); i++) {
                await measure('restart')
                await prepareNext()
                await stopApp('restart')
            }
        }
        await persist()
    } finally {
        await stop(child, client)
    }
}

main().catch((error) => {
    // Emit source locations and fixed classifications, never exception text.
    console.error(
        JSON.stringify({
            success: false,
            sqliteError: Number.isSafeInteger(error?.errcode) ? error.errcode : null,
            categories: [
                'locked',
                'no such table',
                'disk I/O',
                'malformed',
                'readonly',
                'unable to open',
            ].filter((value) => String(error?.message ?? '').includes(value)),
            code: [
                'SQLITE_BUSY',
                'SQLITE_CANTOPEN',
                'SQLITE_ERROR',
                'ERR_SQLITE_ERROR',
                'ENOENT',
                'EACCES',
                'EPERM',
            ].includes(error?.code)
                ? error.code
                : null,
            locations: [
                ...String(error?.stack ?? '').matchAll(/(profile|fixture|cdp)\.mjs:(\d+):(\d+)/g),
            ]
                .slice(0, 4)
                .map((match) => ({
                    file: match[1],
                    line: Number(match[2]),
                    column: Number(match[3]),
                })),
        }),
    )
    process.exitCode = 1
})
