import assert from 'node:assert/strict'
import { spawn, execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:net'
import { dirname, join, resolve } from 'node:path'
import { promisify } from 'node:util'
import { setTimeout as delay } from 'node:timers/promises'
import { REALM_BLOCKED_URL_PATTERNS } from '../../../scripts/realmBlocklist.mjs'
import { startOllamaFixture } from '../ollamaFixture.mjs'
import { UiDriver, runScenario, startScenario, scenarios, uncovered } from './driver.mjs'
import { launchSpecification, ownedCommandLinePattern, ownedProcessScript } from './launch.mjs'
import { nativeEntryScenarios, nativeGateCases, createNativeControlState, cleanupNativeControls } from './nativeControls.mjs'

const exec = promisify(execFile)
const q = JSON.stringify
const root = resolve(import.meta.dirname, '../../..')
const argument = name => process.argv.find(value => value.startsWith(`--${name}=`))?.slice(name.length + 3)
const required = name => {
    const value = argument(name)
    if (!value) throw new Error(`ollama-ui:missing---${name}`)
    return value
}
const sha256 = file => createHash('sha256').update(readFileSync(file)).digest('hex')
const normalize = path => resolve(path).toLowerCase()
if (process.platform !== 'win32') throw new Error('ollama-ui:Windows-only')
assert.ok(process.env.LOCALAPPDATA && process.env.APPDATA)
const inputBinary = resolve(required('binary'))
const launchMode = argument('launch-mode') ?? 'binary'
assert.ok(['binary', 'test-executable'].includes(launchMode), 'Known native UI launch mode')
assert.equal(sha256(inputBinary), required('binary-sha256'), 'Qualified executable hash')
const build = JSON.parse(readFileSync(required('build-manifest'), 'utf8'))
const manifest = JSON.parse(readFileSync(required('execution-manifest'), 'utf8'))
assert.equal(build.schema, 'risunest.ollama-ui/v1')
assert.equal(build.mode, 'agent')
assert.equal(build.entry, 'src/main.ts')
for (const [file, hash] of Object.entries({ ...build.sourceHashes, ...manifest.sources, ...manifest.instrumentation })) {
    assert.equal(sha256(resolve(root, file)), hash, `Build source mismatch: ${file}`)
}
const diagnostic = argument('diagnostic')
assert.ok(diagnostic === undefined || diagnostic === 'startup-only', 'Known diagnostic mode')
const diagnosticOnly = diagnostic === 'startup-only'
const selected = (argument('scenarios') ?? 'SUCC,JS,UNSUPPORTED').split(',')
if (launchMode === 'test-executable') assert.ok(selected.every(name => nativeEntryScenarios.includes(name)), 'Explicit native entry scenario allowlist')
else assert.ok(selected.every(name => !Object.hasOwn(nativeGateCases, name)), 'Native gates require test-executable mode')
if (diagnosticOnly) assert.deepEqual(selected, ['SUCC'], 'Startup diagnostic performs only SUCC setup')
assert.ok(selected.length > 0 && new Set(selected).size === selected.length && selected.every(name => scenarios.includes(name)))
const runId = randomUUID().replaceAll('-', '')
const output = resolve(root, '.tmp/test-results/ollama-route', runId)
mkdirSync(join(output, 'binary'), { recursive: true })
const executable = join(output, 'binary', `RisuNest-${sha256(inputBinary).slice(0, 16)}.exe`)
copyFileSync(inputBinary, executable)
const results = []
const run = { schema: 'risunest.ollama-ui-report/v1', runId, binaryHash: sha256(executable),
    build, executionManifest: manifest, launchMode, selected, results, diagnosticOnly, acceptedControls: [], startupDiagnostics: [], fullV1: false,
    uncovered: [...uncovered, ...scenarios.filter(name => !selected.includes(name)).map(name => `Not selected: ${name}`)],
    platform: 'windows', otherPlatforms: 'intentionally not executed' }
const report = () => writeFileSync(join(output, 'report.json'), JSON.stringify(run, null, 2))

async function freePort() {
    const server = createServer()
    await new Promise((accept, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', accept) })
    const port = server.address().port
    await new Promise((accept, reject) => server.close(error => error ? reject(error) : accept()))
    return port
}

async function connect(url) {
    const socket = new WebSocket(url)
    await new Promise((accept, reject) => {
        const timer = setTimeout(() => { socket.close(); reject(new Error('ollama-ui:CDP-open-timeout')) }, 5_000)
        socket.addEventListener('open', () => { clearTimeout(timer); accept() }, { once: true })
        socket.addEventListener('error', () => { clearTimeout(timer); reject(new Error('ollama-ui:CDP-open-error')) }, { once: true })
    })
    let sequence = 0
    const pending = new Map()
    let runtimeExceptionObserver
    socket.addEventListener('message', event => {
        const message = JSON.parse(String(event.data))
        if (message.method === 'Runtime.exceptionThrown' && runtimeExceptionObserver) {
            const details = message.params?.exceptionDetails ?? {}
            const bounded = value => String(value ?? '').slice(0, 512)
            runtimeExceptionObserver({ text: bounded(details.text), className: bounded(details.exception?.className),
                description: bounded(details.exception?.description?.split('\n')[0]), lineNumber: details.lineNumber,
                columnNumber: details.columnNumber,
                frames: (details.stackTrace?.callFrames ?? []).slice(0, 8).map(frame => ({
                    functionName: bounded(frame.functionName), url: bounded(String(frame.url ?? '').split(/[?#]/)[0]),
                    lineNumber: frame.lineNumber, columnNumber: frame.columnNumber,
                })) })
        }
        const item = pending.get(message.id)
        if (!item) return
        pending.delete(message.id)
        clearTimeout(item.timer)
        message.error ? item.reject(new Error(`ollama-ui:CDP-${item.method}`)) : item.accept(message.result)
    })
    socket.addEventListener('close', () => {
        for (const item of pending.values()) {
            clearTimeout(item.timer)
            item.reject(new Error('ollama-ui:CDP-closed'))
        }
        pending.clear()
    })
    const send = (method, params = {}, timeoutMs = 30_000) => new Promise((accept, reject) => {
        const id = ++sequence
        const timer = setTimeout(() => { pending.delete(id); reject(new Error(`ollama-ui:CDP-timeout-${method}`)) }, timeoutMs)
        pending.set(id, { accept, reject, timer, method })
        socket.send(q({ id, method, params }))
    })
    return { send, close: () => socket.close(), isOpen: () => socket.readyState === WebSocket.OPEN, async observeRuntimeExceptions(observer) {
        runtimeExceptionObserver = observer
        await send('Runtime.enable')
    }, async evaluate(expression) {
        const response = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture: true })
        if (response.exceptionDetails) throw new Error('ollama-ui:synthetic-evaluation-failed')
        return response.result?.value
    } }
}

// Matching both executable and profile prevents cleanup from touching another app.
async function ownedProcess(child, profile, action, creationIdentity) {
    const result = await exec('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', ownedProcessScript], {
        windowsHide: true, timeout: 10_000, env: { ...process.env, RISUNEST_UI_PID: String(child.pid),
            RISUNEST_UI_EXE: executable, RISUNEST_UI_PROFILE: profile,
            RISUNEST_UI_MODE: launchMode, RISUNEST_UI_COMMAND_PATTERN: ownedCommandLinePattern(executable, launchMode, profile),
            RISUNEST_UI_ACTION: action, RISUNEST_UI_CREATED: creationIdentity ?? '' },
    })
    return JSON.parse(result.stdout).created
}

async function gracefulClose(child, profile, exited, creationIdentity) {
    if (exited()) return
    await ownedProcess(child, profile, 'close', creationIdentity)
    const deadline = Date.now() + 30_000
    while (!exited() && Date.now() < deadline) await delay(100)
    if (!exited()) throw new Error('ollama-ui:owned-process-graceful-close-timeout')
}

for (const name of selected) {
    const profile = `agent-ollama-${runId.slice(0, 12)}-${selected.indexOf(name)}`
    assert.ok(/^[a-z0-9-]{1,32}$/.test(profile), 'Native profile name contract')
    const nativeData = resolve(process.env.LOCALAPPDATA, 'RisuNestProfiles-agent', profile, 'data')
    const configRoot = resolve(process.env.APPDATA, 'RisuNestProfiles-agent', profile)
    assert.equal(existsSync(dirname(nativeData)), false, 'Native profile must be fresh')
    assert.equal(existsSync(configRoot), false, 'Configuration profile must be fresh')
    const port = await freePort()
    const caseOutput = join(output, name)
    mkdirSync(caseOutput)
    let child
    let exited = true
    let launchError = null
    let creationIdentity
    let page
    let fixture
    let verified = false
    const nativeControl = launchMode === 'test-executable' ? createNativeControlState() : null
    const startup = { scenario: name, milestones: [], runtimeExceptions: [], droppedExceptions: 0 }
    run.startupDiagnostics.push(startup)
    const recordStartup = event => {
        if (startup.milestones.length < 40) startup.milestones.push({ event: event.event, at: new Date().toISOString(),
            ...(event.error ? { error: event.error } : {}) })
        if (event.dom) startup.timeoutDom = event.dom
        report()
    }
    async function launch() {
        assert.equal(exited, true)
        exited = false
        launchError = null
        verified = false
        const specification = launchSpecification(launchMode, profile, process.env)
        child = spawn(executable, specification.args, {
            cwd: dirname(executable), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
            env: { ...specification.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` },
        })
        child.on('exit', () => { exited = true })
        child.on('error', error => { launchError = error; exited = true })
        child.stdout.on('data', bytes => { if (verified) writeFileSync(join(caseOutput, 'stdout.log'), bytes, { flag: 'a' }) })
        child.stderr.on('data', bytes => { if (verified) writeFileSync(join(caseOutput, 'stderr.log'), bytes, { flag: 'a' }) })
        creationIdentity = await ownedProcess(child, profile, 'identity')
    }
    async function attach(previousToken) {
        const deadline = Date.now() + 45_000
        while (Date.now() < deadline && !exited) {
            let candidate
            try {
                const response = await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(1000) })
                const targets = await response.json()
                const target = targets.find(value => value.type === 'page' && /^https?:\/\/tauri\.localhost(?:\/|$)/.test(value.url))
                if (!target) { await delay(100); continue }
                candidate = await connect(target.webSocketDebuggerUrl)
                await candidate.send('Network.enable')
                await candidate.send('Network.setBlockedURLs', { urls: REALM_BLOCKED_URL_PATTERNS })
                const roots = await candidate.evaluate("globalThis.__TAURI_INTERNALS__.invoke('app_paths_roots')")
                assert.equal(normalize(roots.data), normalize(nativeData), 'Native data root isolation')
                const identity = await candidate.evaluate('globalThis.__risunestOllamaUi && ({build:globalThis.__risunestOllamaUi.build,token:globalThis.__risunestOllamaUi.token})')
                if (!identity || identity.token === previousToken) { candidate.close(); await delay(100); continue }
                assert.deepEqual(identity.build, build, 'Embedded real-UI agent build identity')
                assert.equal(await candidate.evaluate('globalThis.__risunestExecutionProfile.identity'), manifest.identity)
                page?.close()
                page = candidate
                verified = true
                recordStartup({ event: 'native-root-realm-build-identities-verified' })
                await page.observeRuntimeExceptions(exception => {
                    if (startup.runtimeExceptions.length < 20) startup.runtimeExceptions.push(exception)
                    else startup.droppedExceptions += 1
                    report()
                })
                recordStartup({ event: 'runtime-exceptions-subscribed' })
                return { driver: new UiDriver(page, manifest, recordStartup), token: identity.token, origin: new URL(target.url).origin }
            } catch (error) {
                candidate?.close()
                if (error?.code === 'ERR_ASSERTION') throw error
                await delay(100)
            }
        }
        throw launchError ?? new Error('ollama-ui:owned-app-unavailable')
    }
    async function restart() {
        await gracefulClose(child, profile, () => exited, creationIdentity)
        page?.close()
        await launch()
        return (await attach()).driver
    }
    async function crashProcess() {
        await ownedProcess(child, profile, 'crash', creationIdentity)
        const deadline = Date.now() + 10_000
        while (!exited && Date.now() < deadline) await delay(50)
        assert.equal(exited, true, 'Injected crash ends only the owned process')
        page?.close()
        await launch()
        return (await attach()).driver
    }
    async function crash(driver) {
        const token = await driver.evaluate('globalThis.__risunestOllamaUi.token')
        const loss = page.send('Page.crash', {}, 3_000).catch(() => {})
        const recovered = await attach(token)
        await loss
        return recovered.driver
    }
    try {
        await launch()
        const attached = await attach()
        fixture = await startOllamaFixture({ corsOrigin: attached.origin })
        await startScenario(attached.driver, name, fixture.origin)
        if (diagnosticOnly) {
            run.startupReady = true
            recordStartup({ event: 'startup-only-stopped-before-send' })
            console.log('Real UI startup diagnostic: ready, no control accepted')
        } else {
            results.push(await runScenario({ name, driver: attached.driver, fixture, manifest, restart, crash, crashProcess, nativeControl }))
            run.acceptedControls.push(name)
            report()
            console.log(`Real UI ${name}: passed`)
        }
    } catch (error) {
        run.success = false
        run.failure = { scenario: name, error: error instanceof Error ? error.message : 'unknown',
            nativeControl: nativeControl?.evidence() ?? null }
        report()
        if (verified && page?.isOpen() && !exited) {
            try { run.failure.diagnostics = await new UiDriver(page, manifest).captureFailure(fixture) }
            catch { run.failure.diagnostics = { captureFailed: true } }
        } else run.failure.diagnostics = { captureSkipped: 'no-current-verified-page' }
        try { report() } catch {}
        throw error
    } finally {
        try {
            await cleanupNativeControls(nativeControl, async () => {
                await page?.evaluate(`(() => {
                    const witness = globalThis.__risunestOllamaUi?.witness
                    if (witness?.snapshot().blocked) witness.resume()
                })()`).catch(() => {})
            }, () => fixture?.release(), async () => {
                if (child) await gracefulClose(child, profile, () => exited, creationIdentity)
            })
        }
        catch (error) {
            run.success = false
            run.cleanupFailure = { scenario: name, ownedPid: child.pid, error: String(error),
                nativeCleanup: error.nativeCleanup ?? null, nativeControl: nativeControl?.evidence() ?? null }
            report()
            throw error
        } finally {
            page?.close()
            await fixture?.close()
        }
    }
}
run.success = true
report()
console.log(`Real UI evidence: ${join(output, 'report.json')}`)

