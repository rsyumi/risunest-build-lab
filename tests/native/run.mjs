import { spawn, spawnSync } from 'node:child_process'
import { mkdirSync, existsSync, readFileSync, writeFileSync, rmSync, lstatSync } from 'node:fs'
import { resolve, join, dirname, basename, isAbsolute } from 'node:path'
import { randomUUID, createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import { verifyReport } from './report.mjs'
import { startOfficialAccountFixtureServer } from '../../benchmarks/official-account/server.mjs'
import { startOllamaFixture } from './ollamaFixture.mjs'
import { REALM_BLOCKED_URL_PATTERNS } from '../../scripts/realmBlocklist.mjs'
import { createServer } from 'node:net'

const root = resolve(import.meta.dirname, '../..')
const smallAssets = process.argv.includes('--asset-benchmark-small')
const officialAccount = process.argv.includes('--official-account-benchmark')
const assetBenchmark = officialAccount || smallAssets || process.argv.includes('--asset-benchmark')
const debug = process.argv.includes('--debug')
const reuseBuild = process.argv.includes('--reuse-build')
const appLockUnavailable = process.argv.includes('--expect-app-lock-unavailable')
const binaryInput = process.argv.find(arg => arg.startsWith('--binary='))?.slice(9)
if (binaryInput && !reuseBuild) throw new Error('An explicit binary requires --reuse-build')
if (appLockUnavailable && (process.platform !== 'win32' || assetBenchmark || !binaryInput)) throw new Error('App-lock setup failure verification requires an explicit Windows contract binary')
const repetitions = Number(process.argv.find(arg => arg.startsWith('--runs='))?.slice(7) ?? 1)
if ((!assetBenchmark && ((reuseBuild && !binaryInput) || repetitions !== 1)) || !Number.isSafeInteger(repetitions) || repetitions < 1 || repetitions > 10) {
    throw new Error('Contract build reuse requires an explicit binary; repeated runs require an asset benchmark with 1 to 10 runs')
}
const require = createRequire(import.meta.url)
function command(file, args, env = process.env) {
    return new Promise((accept, reject) => {
        const child = spawn(file, args, { cwd: root, env, stdio: 'inherit', windowsHide: true })
        child.on('error', reject)
        child.on('exit', (code, signal) => code === 0 ? accept() : reject(new Error(`${file}: ${signal ?? code}`)))
    })
}
const commonResult = spawnSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: root, encoding: 'utf8', windowsHide: true })
if (commonResult.status !== 0) throw new Error('Cannot locate shared Cargo target')
const target = resolve(commonResult.stdout.trim(), '../src-tauri/target')
if (process.env.CARGO_TARGET_DIR && resolve(process.env.CARGO_TARGET_DIR) !== target) throw new Error('Incompatible shared Cargo target')
const env = { ...process.env, CARGO_TARGET_DIR: target, TAURI_CONFIG: JSON.stringify({ build: { frontendDist: [join(root, '.tmp/test-results/native/dist')] } }) }
if (!reuseBuild) {
    await command(process.execPath, [join(dirname(require.resolve('vite/package.json')), 'bin/vite.js'), 'build', '--mode', 'agent', '--config', 'tests/native/vite.config.ts'])
    // Contract runs compile the app crate unoptimized; its dependencies and measured builds keep the release optimization.
    const contractProfile = assetBenchmark || debug ? [] : ['--config', 'profile.release.package.risunest.opt-level=0']
    await command('cargo', ['build', ...(debug ? [] : ['--release']), ...contractProfile, '--locked', '--manifest-path', 'tests/native/native/Cargo.toml'], env)
}
const executable = binaryInput ? resolve(binaryInput) : join(target, debug ? 'debug' : 'release', `risunest-persistence-boundary${process.platform === 'win32' ? '.exe' : ''}`)
const binaryHash = createHash('sha256').update(readFileSync(executable)).digest('hex')
// A contract run leaves its unoptimized binary at the same path, so a reused measurement build must still be the last one built.
const measuredBuild = `${executable}.asset-benchmark`
if (assetBenchmark && !binaryInput) {
    if (!reuseBuild) writeFileSync(measuredBuild, binaryHash)
    else if (!existsSync(measuredBuild) || readFileSync(measuredBuild, 'utf8') !== binaryHash) throw new Error('Rebuild the asset benchmark; this binary was not built for it')
}
const phaseGroups = assetBenchmark ? [[officialAccount ? 'official-account-benchmark' : smallAssets ? 'asset-benchmark-small' : 'asset-benchmark']] : [['write', 'read'], ['abort', 'read-abort'], ['contract', 'read-contract']]
// The repeated-close escape exists where closing the window quits; SIGTERM asks only Linux to save.
if (!assetBenchmark && (process.platform === 'win32' || process.platform === 'linux')) phaseGroups.push(['close-escape'])
if (!assetBenchmark && process.platform === 'linux') phaseGroups.push(['session-end', 'read-session-end'], ['session-end-unanswered'])
// Native generation reaches a loopback Ollama fixture; only WebView2 lets the runner crash the renderer.
if (!assetBenchmark) phaseGroups.push(['generation'], ['generation-reload'])
if (!assetBenchmark && process.platform === 'win32') phaseGroups.push(['generation-lost'])
const accountServer = officialAccount ? await startOfficialAccountFixtureServer() : null
const ollamaFixture = assetBenchmark ? null : await startOllamaFixture({ control: true })
const freePort = () => new Promise((accept, reject) => {
    const server = createServer()
    server.on('error', reject)
    server.listen(0, '127.0.0.1', () => {
        const { port } = server.address()
        server.close(error => error ? reject(error) : accept(port))
    })
})
// Ends the hidden WebView's renderer process the way a renderer crash does.
async function crashRenderer(port) {
    const deadline = Date.now() + 30_000
    for (;;) {
        let socket
        try {
            const targets = await (await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(1_000) })).json()
            const page = targets.find(value => value.type === 'page' && /^https?:\/\/tauri\.localhost/.test(value.url))
            if (page) {
                socket = new WebSocket(page.webSocketDebuggerUrl)
                await new Promise((accept, reject) => {
                    socket.addEventListener('open', accept, { once: true })
                    socket.addEventListener('error', reject, { once: true })
                })
                const replies = new Map()
                socket.addEventListener('message', event => {
                    const message = JSON.parse(String(event.data))
                    replies.get(message.id)?.(message)
                })
                const send = (id, method, params = {}) => new Promise((accept, reject) => {
                    const timer = setTimeout(() => reject(new Error(`CDP ${method} timeout`)), 5_000)
                    replies.set(id, message => { clearTimeout(timer); message.error ? reject(new Error(`CDP ${method} failed`)) : accept() })
                    socket.send(JSON.stringify({ id, method, params }))
                })
                await send(1, 'Network.enable')
                await send(2, 'Network.setBlockedURLs', { urls: REALM_BLOCKED_URL_PATTERNS })
                // The renderer exits before it can answer.
                socket.send(JSON.stringify({ id: 3, method: 'Page.crash', params: {} }))
                await new Promise(resolve => { socket.addEventListener('close', resolve, { once: true }); setTimeout(resolve, 3_000) })
                return
            }
        } catch (error) {
            if (Date.now() > deadline) throw error
        } finally { socket?.close() }
        if (Date.now() > deadline) throw new Error('Renderer debugging endpoint unavailable')
        await new Promise(resolve => setTimeout(resolve, 100))
    }
}
try {
for (const phases of Array.from({ length: repetitions }, () => phaseGroups).flat()) {
    // Linux registers the identifier as a D-Bus name, whose elements cannot start with a digit.
    const runId = `a${randomUUID().replaceAll('-', '').slice(1)}`
    const identifier = `io.github.rsyumi.risunest.boundary.${runId}`
    const dataHome = process.platform === 'win32' ? process.env.APPDATA
        : process.platform === 'darwin' ? join(process.env.HOME, 'Library/Application Support')
        : process.platform === 'linux' ? (process.env.XDG_DATA_HOME || join(process.env.HOME, '.local/share')) : null
    if (!dataHome) throw new Error('Desktop host required')
    const ownedRoot = resolve(dataHome, identifier)
    const cleanupRoot = resolve(dataHome, `${identifier}-cleanup`)
    const ownedProfiles = [[ownedRoot, identifier], [cleanupRoot, `${identifier}-cleanup`]]
    for (const [directory] of ownedProfiles) {
        if (existsSync(directory)) throw new Error('Run identity already exists')
    }
    for (const [directory] of ownedProfiles) {
        mkdirSync(directory)
        writeFileSync(join(directory, 'boundary-owner'), runId, { flag: 'wx' })
    }
    const output = join(root, '.tmp/test-results/native', runId)
    mkdirSync(output, { recursive: true })
    try {
        for (const phase of phases) {
            const reportPath = join(output, `${phase}.json`)
            const memoryPath = join(output, `${phase}.memory.jsonl`)
            let memoryDone = Promise.resolve(null)
            const debugPort = phase === 'generation-lost' ? await freePort() : null
            const status = await new Promise((accept, reject) => {
                // The generation phases read the Ollama fixture's origin from the same field.
                const origin = accountServer?.origin ?? (phase.startsWith('generation') ? ollamaFixture?.origin : undefined)
                const child = spawn(executable, [], { cwd: root, windowsHide: true, stdio: 'inherit', detached: process.platform !== 'win32', env: {
                    ...env, RISUNEST_BOUNDARY_ID: runId, RISUNEST_BOUNDARY_ROOT: ownedRoot,
                    RISUNEST_BOUNDARY_APP_LOCK_UNAVAILABLE: appLockUnavailable ? '1' : '0',
                    ...(origin ? { RISUNEST_BOUNDARY_ACCOUNT_ORIGIN: origin } : {}),
                    ...(debugPort ? { WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort}` } : {}),
                    RISUNEST_BOUNDARY_PHASE: phase, RISUNEST_BOUNDARY_REPORT: reportPath,
                } })
                if (officialAccount && process.platform === 'win32' && child.pid) {
                    const monitor = spawn('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(root, 'tests/native/windowsMemory.ps1'),
                        '-RootProcessId', String(child.pid), '-OutputPath', memoryPath], { windowsHide: true, stdio: 'ignore' })
                    memoryDone = new Promise(resolve => {
                        monitor.on('error', () => resolve('unavailable'))
                        monitor.on('exit', code => resolve(code === 0 ? null : `exit-${code}`))
                    })
                }
                let timedOut = false
                let crashError
                const terminate = phase.startsWith('session-end') || debugPort ? setInterval(() => {
                    if (!existsSync(`${reportPath}.ready`)) return
                    clearInterval(terminate)
                    if (!debugPort) return child.kill('SIGTERM')
                    crashRenderer(debugPort).catch(error => {
                        crashError = error
                        spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true })
                    })
                }, 50) : undefined
                const timeout = setTimeout(() => {
                    timedOut = true
                    if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true })
                    else process.kill(-child.pid, 'SIGKILL')
                }, officialAccount ? 600_000 : assetBenchmark ? 300_000 : 90_000)
                child.on('error', error => { clearTimeout(timeout); clearInterval(terminate); reject(error) })
                child.on('close', code => {
                    clearTimeout(timeout)
                    clearInterval(terminate)
                    crashError ? reject(crashError) : timedOut ? reject(new Error(`Native ${phase} timed out`)) : accept(code)
                })
            })
            const report = JSON.parse(readFileSync(reportPath, 'utf8'))
            verifyReport(report, runId, phase, status)
            const memoryError = await memoryDone
            if (officialAccount && process.platform === 'win32') {
                const memory = existsSync(memoryPath) ? readFileSync(memoryPath, 'utf8').replace(/^\uFEFF/, '').trim().split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line)) : []
                report.processMemory = { sampleCount: memory.length, error: memoryError,
                    maximumSampledPrivateBytes: memory.length ? Math.max(...memory.map(sample => sample.privateBytes)) : null,
                    note: 'Windows native process plus its WebView descendants, sampled about every 500 ms. Private bytes are committed memory; working sets can double-count shared pages. Short-lived peaks may be missed.' }
                for (const group of ['samples', 'nativeLedgerSamples', 'presenceSamples', 'manifestSamples']) for (const sample of report.measurements?.[group] ?? []) {
                    const window = memory.filter(value => value.timestampMs >= sample.startedAt && value.timestampMs <= sample.finishedAt)
                    const before = memory.filter(value => value.timestampMs <= sample.startedAt).at(-1)
                    sample.processMemory = { samples: window.length, beforePrivateBytes: before?.privateBytes ?? null,
                        maximumSampledPrivateBytes: window.length ? Math.max(...window.map(value => value.privateBytes)) : null }
                }
            }
            if (assetBenchmark) writeFileSync(reportPath, JSON.stringify({ ...report, binaryHash, buildMode: debug ? 'debug' : 'release', reusedBuild: reuseBuild }, null, 2))
            console.log(`Native persistence ${phase}: passed (${process.platform})`)
            if (assetBenchmark) console.log(`Asset measurement report: ${reportPath}`)
        }
    } finally {
        for (const [directory, name] of ownedProfiles) {
            const marker = join(directory, 'boundary-owner')
            const metadata = lstatSync(directory)
            const markerMetadata = lstatSync(marker)
            if (!isAbsolute(directory) || dirname(directory) !== resolve(dataHome)
                || basename(directory) !== name || directory !== resolve(dataHome, name)
                || !metadata.isDirectory() || metadata.isSymbolicLink()
                || !markerMetadata.isFile() || markerMetadata.isSymbolicLink()
                || readFileSync(marker, 'utf8') !== runId) throw new Error('Owned cleanup validation failed')
        }
        for (const [directory] of ownedProfiles) {
            rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 })
        }
    }
}
} finally {
    await accountServer?.close()
    await ollamaFixture?.close()
}
