import assert from 'node:assert/strict'
import { spawn, spawnSync } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { createServer } from 'node:net'
import { dirname, join, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { REALM_BLOCKED_URL_PATTERNS } from '../../../scripts/realmBlocklist.mjs'

if (process.platform !== 'win32') throw new Error('This renderer-loss runner requires Windows WebView2')
const root = resolve(import.meta.dirname, '../../..')
const require = createRequire(import.meta.url)
const outputRoot = join(root, '.tmp/test-results/renderer-loss')
const common = spawnSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: root, encoding: 'utf8', windowsHide: true })
assert.equal(common.status, 0)
const target = resolve(common.stdout.trim(), '../src-tauri/target')
if (process.env.CARGO_TARGET_DIR) assert.equal(resolve(process.env.CARGO_TARGET_DIR), target)
const env = {
    ...process.env, CARGO_TARGET_DIR: target,
    TAURI_CONFIG: JSON.stringify({ build: { frontendDist: [join(outputRoot, 'dist')] } }),
}
function command(file, args) {
    return new Promise((accept, reject) => {
        const child = spawn(file, args, { cwd: root, env, windowsHide: true, stdio: 'inherit' })
        child.on('error', reject)
        child.on('exit', code => code === 0 ? accept() : reject(new Error(`Verification build exited ${code}`)))
    })
}
let executable = process.argv.find(value => value.startsWith('--binary='))?.slice(9)
if (!executable) {
    await command(process.execPath, [
        join(dirname(require.resolve('vite/package.json')), 'bin/vite.js'),
        'build', '--mode', 'agent', '--config', 'tests/native/renderer-loss/vite.config.ts',
    ])
    await command('cargo', ['build', '--locked', '--jobs', '2', '--manifest-path', 'tests/native/native/Cargo.toml'])
    const built = join(target, 'debug/risunest-persistence-boundary.exe')
    const digest = createHash('sha256').update(readFileSync(built)).digest('hex')
    mkdirSync(join(outputRoot, 'binaries'), { recursive: true })
    executable = join(outputRoot, 'binaries', `renderer-loss-${digest.slice(0, 16)}.exe`)
    copyFileSync(built, executable)
    console.log(`Renderer-loss binary: ${executable}`)
}
executable = resolve(executable)
if (process.argv.includes('--build-only')) process.exit(0)

const runId = randomUUID().replaceAll('-', '')
const identifier = `io.github.rsyumi.risunest.boundary.${runId}`
assert.ok(process.env.APPDATA)
const ownedRoot = resolve(process.env.APPDATA, identifier)
assert.equal(existsSync(ownedRoot), false, 'Synthetic profile must be new')
mkdirSync(ownedRoot)
writeFileSync(join(ownedRoot, 'boundary-owner'), runId, { flag: 'wx' })
const output = join(outputRoot, runId)
mkdirSync(output, { recursive: true })
const port = await new Promise((accept, reject) => {
    const server = createServer()
    server.on('error', reject)
    server.listen(0, '127.0.0.1', () => {
        const address = server.address()
        server.close(error => error ? reject(error) : accept(address.port))
    })
})
const child = spawn(executable, [], {
    cwd: root, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: {
        ...env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
        RISUNEST_BOUNDARY_ID: runId, RISUNEST_BOUNDARY_ROOT: ownedRoot,
        RISUNEST_BOUNDARY_PHASE: 'renderer-loss',
        RISUNEST_BOUNDARY_REPORT: join(output, 'native-finish.json'),
    },
})
let processExited = false
const exit = new Promise((accept, reject) => {
    child.on('error', reject)
    child.on('exit', code => { processExited = true; accept(code) })
})
// Output belongs exclusively to the new synthetic profile.
child.stdout.on('data', data => writeFileSync(join(output, 'stdout.log'), data, { flag: 'a' }))
child.stderr.on('data', data => writeFileSync(join(output, 'stderr.log'), data, { flag: 'a' }))

async function connect(url) {
    const socket = new WebSocket(url)
    await new Promise((accept, reject) => {
        const timer = setTimeout(() => { socket.close(); reject(new Error('CDP connection timeout')) }, 5_000)
        socket.addEventListener('open', () => { clearTimeout(timer); accept() }, { once: true })
        socket.addEventListener('error', error => { clearTimeout(timer); reject(error) }, { once: true })
    })
    let nextId = 0
    const pending = new Map()
    socket.addEventListener('message', event => {
        const message = JSON.parse(String(event.data))
        const request = pending.get(message.id)
        if (!request) return
        pending.delete(message.id)
        clearTimeout(request.timer)
        message.error ? request.reject(new Error(`CDP ${request.method} failed`)) : request.accept(message.result)
    })
    socket.addEventListener('close', () => {
        for (const request of pending.values()) {
            clearTimeout(request.timer)
            request.reject(new Error('CDP renderer disconnected'))
        }
        pending.clear()
    })
    const send = (method, params = {}, timeoutMs = 10_000) => new Promise((accept, reject) => {
        const id = ++nextId
        const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP ${method} timeout`)) }, timeoutMs)
        pending.set(id, { accept, reject, timer, method })
        socket.send(JSON.stringify({ id, method, params }))
    })
    return {
        send,
        close: () => socket.close(),
        async evaluate(expression) {
            const response = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
            if (response.exceptionDetails) throw new Error('Synthetic renderer evaluation failed')
            return response.result?.value
        },
    }
}

let page
async function recover(previousToken) {
    const deadline = Date.now() + 45_000
    let candidate
    while (Date.now() < deadline && !processExited) {
        try {
            const targets = await (await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(1_000) })).json()
            const targetPage = targets.find(value => value.type === 'page' && /^https?:\/\/tauri\.localhost/.test(value.url))
            if (!targetPage) { await delay(100); continue }
            candidate = await connect(targetPage.webSocketDebuggerUrl)
            await candidate.send('Network.enable')
            await candidate.send('Network.setBlockedURLs', { urls: REALM_BLOCKED_URL_PATTERNS })
            const roots = await candidate.evaluate("window.__TAURI_INTERNALS__.invoke('app_paths_roots')")
            assert.equal(resolve(roots.data).toLowerCase(), ownedRoot.toLowerCase(), 'Native data root isolation')
            const token = await candidate.evaluate('window.__rendererLoss?.token')
            if (!token || token === previousToken) { candidate.close(); await delay(100); continue }
            page?.close()
            page = candidate
            return await page.evaluate(`window.__rendererLoss.begin(${JSON.stringify(ownedRoot)})`)
        } catch (error) {
            candidate?.close()
            if (error?.code === 'ERR_ASSERTION') throw error
            await delay(100)
        }
    }
    throw new Error('Native renderer did not recover within the bounded deadline')
}
const eventsFrom = logs => logs.filter(entry => entry.target === 'generation-diagnostic')
    .map(entry => JSON.parse(entry.message))
const canaries = []
const cases = []
try {
    let current = await recover()
    for (const scenario of [
        'send-preparation', 'awaiting-provider', 'parser', 'before-commit',
        'after-commit-before-js-ack', 'settled', 'document-reset',
    ]) {
        const before = scenario === 'document-reset'
            ? { documentSequence: current.documentSequence, committed: false }
            : await page.evaluate(`window.__rendererLoss.prepare(${JSON.stringify(scenario)})`)
        if (before.canary) canaries.push(before.canary)
        if (before.acknowledgement) {
            const acknowledgement = await page.evaluate('window.__rendererLoss.acknowledgement()')
            assert.equal(acknowledgement.callerObserved, false, 'Store caller has not observed commit acknowledgement before crash')
            assert.equal(acknowledgement.nativeRevision, before.revisionAfter)
        }
        const crash = page.send('Page.crash', {}, 3_000).catch(() => {})
        current = await recover(current.token)
        await crash
        assert.ok(current.documentSequence > before.documentSequence, 'Native document sequence advances')
        const logs = await page.evaluate("window.__TAURI_INTERNALS__.invoke('native_log_tail', {limit:500})")
        for (const canary of canaries) assert.equal(JSON.stringify(logs).includes(canary), false, 'Private canary absent')
        const events = eventsFrom(logs)
        const lost = events.findLast(event => event.kind === 'renderer-exit' && event.documentSequence === before.documentSequence)
        assert.ok(lost, 'Actual renderer-exit hook retains the crashed document')
        assert.equal(lost.reason.platform, 'windows')
        const recovered = events.findLast(event => event.kind === 'document-start' && event.documentSequence === current.documentSequence)
        assert.equal(recovered?.rendererRecovery, true)
        assert.ok(events.some(event => event.kind === 'renderer-reload' && event.documentSequence === before.documentSequence && event.outcome === 'requested'))
        if (scenario === 'document-reset') {
            assert.equal(lost.phase, null, 'New document clears previous phase')
            assert.equal(lost.commit, null, 'Old document commit is not assigned to a new document')
        } else {
            assert.equal(lost.phase?.phase, before.phase)
            assert.equal(lost.phase?.edge, before.edge)
            assert.equal(lost.phase?.savePath, before.savePath)
            assert.equal(lost.phase?.documentSequence, before.documentSequence)
            assert.ok(Number.isSafeInteger(lost.phaseAgeMs) && lost.phaseAgeMs >= 0)
            if (before.committed) {
                assert.equal(lost.commit?.outcome, 'ok')
                assert.equal(lost.commit?.expectedRevision, before.revisionBefore)
                assert.equal(lost.commit?.revision, before.revisionAfter)
                assert.equal(lost.commit?.revisionCorrelation, 'store-unknown')
                assert.ok(Number.isSafeInteger(lost.commitAgeMs) && lost.commitAgeMs >= 0)
                const readback = await page.evaluate('window.__rendererLoss.readback()')
                assert.equal(readback.revision, before.revisionAfter)
                assert.equal(readback.root.value.syntheticRendererLoss, before.canary)
            } else assert.equal(lost.commit, null)
        }
        cases.push({
            scenario, documentSequence: before.documentSequence, recoveredDocumentSequence: current.documentSequence,
            acknowledgement: before.acknowledgement ?? null, exit: lost,
        })
        writeFileSync(join(output, 'progress.json'), JSON.stringify(cases, null, 2))
        console.log(`Actual WebView2 renderer loss: ${scenario} passed`)
    }
    const report = {
        success: true, runId, platform: process.platform, binaryHash: createHash('sha256').update(readFileSync(executable)).digest('hex'),
        phaseSource: 'synthetic diagnostic states; real product diagnostic emitter, native handler, SQLite commit, renderer loss and recovery',
        excludes: 'Full UI send interruption, provider execution, and loss of the underlying native transport reply', cases,
    }
    await page.evaluate(`window.__TAURI_INTERNALS__.invoke('boundary_finish', {report:${JSON.stringify({ runId, phase: 'renderer-loss', success: true, cases: cases.map(value => value.scenario) })}})`).catch(() => {})
    const status = await Promise.race([exit, delay(10_000).then(() => { throw new Error('Boundary did not close') })])
    assert.equal(status, 0)
    writeFileSync(join(output, 'report.json'), JSON.stringify(report, null, 2))
    console.log(`Renderer-loss report: ${join(output, 'report.json')}`)
} catch (error) {
    writeFileSync(join(output, 'report.json'), JSON.stringify({
        success: false, runId, error: error instanceof Error ? error.message : 'Renderer-loss verification failed', cases,
    }, null, 2))
    throw error
} finally {
    if (!processExited) {
        await page?.evaluate(`window.__TAURI_INTERNALS__.invoke('boundary_record', {report:${JSON.stringify({ runId, phase: 'renderer-loss', success: false, cases: [] })}, exit:1})`).catch(() => {})
        await Promise.race([exit, delay(3_000)])
        if (!processExited) child.kill()
    }
    page?.close()
    console.log(`Synthetic profile retained: ${ownedRoot}`)
}
