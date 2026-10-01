import assert from 'node:assert/strict'
import { execFile, spawnSync } from 'node:child_process'
import { readFile, writeFile, mkdir, stat } from 'node:fs/promises'
import { randomBytes, createHash } from 'node:crypto'
import { createServer } from 'node:net'
import { promisify } from 'node:util'
import path from 'node:path'
import { cutDeviceNetwork } from '../../scripts/phase3AndroidSmoke.mjs'
import { REALM_BLOCKED_URL_PATTERNS } from '../../scripts/realmBlocklist.mjs'

const options = Object.fromEntries(process.argv.slice(2).map(arg => {
    const split = arg.indexOf('=')
    assert.ok(arg.startsWith('--') && split > 2, 'Use --name=value')
    return [arg.slice(2, split), arg.slice(split + 1)]
}))
for (const key of Object.keys(options)) assert.ok(['adb', 'apk', 'health', 'output', 'device', 'serial', 'cdp-port'].includes(key), 'Unknown option')
assert.ok(options.adb && options.apk && options.health, '--adb, --apk and --health are required')
assert.ok(!options.device || ['api34', 'api35'].includes(options.device), 'Unknown synthetic device')
assert.ok(!options.serial || options.serial === 'emulator-5640', 'Unexpected alternate synthetic serial')
const serial = options.serial ?? (options.device === 'api35' ? 'emulator-5556' : 'emulator-5554')
const avd = options.device === 'api35' ? 'risunest_buffer_api35_synthetic' : 'risunest_vm_retest'
const configuredIdentifier = 'io.github.rsyumi.risunest'
const packageName = 'io.github.rsyumi.risunest.pluginreview'
const title = 'RisuNest synthetic plugin review'
assert.ok(options['cdp-port'] === undefined || /^\d+$/.test(options['cdp-port']), 'Invalid CDP port')
const port = Number(options['cdp-port'] ?? 19371)
assert.ok(Number.isInteger(port) && port >= 1024 && port <= 65535, 'Invalid CDP port')
const execute = promisify(execFile)
const delay = ms => new Promise(resolve => setTimeout(resolve, ms))
const apk = path.resolve(options.apk)
const output = path.resolve(options.output ?? '.tmp/plugin-review/android-result.json')
const token = randomBytes(16).toString('hex')
let client, launched = false, forwarded = false
async function run(args, timeout = 10000, allowFailure = false) {
    try { return await execute(options.adb, ['-s', serial, ...args], { windowsHide: true, timeout, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024 }) }
    catch (error) { if (allowFailure) return { stdout: '' }; throw new Error(`Synthetic ADB ${args[0]} failed`, { cause: error }) }
}
async function connect() {
    let target
    const deadline = Date.now() + 30000
    while (Date.now() < deadline) {
        const targets = await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(3000) }).then(res => res.json()).catch(() => [])
        target = targets.find(item => item.type === 'page' && item.title === title && item.url.startsWith('http://tauri.localhost'))
        if (target) break
        await delay(250)
    }
    assert.ok(target, 'Synthetic plugin WebView unavailable')
    const ws = new WebSocket(target.webSocketDebuggerUrl)
    await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('CDP connection timeout')), 10000)
        ws.onopen = () => { clearTimeout(timer); resolve() }
        ws.onerror = () => { clearTimeout(timer); reject(new Error('CDP connection failed')) }
    })
    let sequence = 0
    const pending = new Map()
    ws.onmessage = ({ data }) => {
        const message = JSON.parse(data)
        const entry = pending.get(message.id)
        if (!entry) return
        clearTimeout(entry.timer); pending.delete(message.id)
        if (message.error) entry.reject(new Error('CDP command failed'))
        else entry.resolve(message.result)
    }
    const call = (method, params = {}, timeout = 30000) => new Promise((resolve, reject) => {
        const id = ++sequence
        const timer = setTimeout(() => { pending.delete(id); reject(new Error('CDP timeout')) }, timeout)
        pending.set(id, { resolve, reject, timer }); ws.send(JSON.stringify({ id, method, params }))
    })
    const evaluate = async (expression, timeout) => {
        const result = await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, timeout)
        assert.ok(!result.exceptionDetails, 'Synthetic evaluation failed')
        return result.result.value
    }
    const close = () => { for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(new Error('CDP closed')) }; pending.clear(); ws.close() }
    try {
        await call('Network.enable')
        await call('Network.setBlockedURLs', { urls: [...REALM_BLOCKED_URL_PATTERNS, '*update.rsyumi.workers.dev/translator/prompt-presets.json*'] })
        assert.equal(await evaluate('document.title'), title)
        assert.equal(await evaluate("window.__TAURI_INTERNALS__.invoke('plugin:app|identifier')"), configuredIdentifier)
        return { call, evaluate, close }
    } catch (error) { close(); throw error }
}
async function main() {
    assert.equal(process.env.ADB_SERVER_SOCKET, 'tcp:127.0.0.1:15037', 'Unexpected ADB server')
    assert.equal((await run(['emu', 'avd', 'name'])).stdout.split('\n')[0].trim(), avd, 'Unsafe AVD')
    assert.equal((await run(['shell', 'getprop', 'sys.boot_completed'])).stdout.trim(), '1')
    const health = JSON.parse(await readFile(options.health, 'utf8'))
    assert.ok(health.Serial === serial && health.Seconds >= 120 && health.Samples >= 8 && health.Failures === 0 && health.Passed === true
        && health.HashMatch === true && health.Push === true && health.Pull === true && health.ConsolePing === true, 'Sustained synthetic ADB health proof required')
    assert.ok(Date.now() - (await stat(options.health)).mtimeMs < 30 * 60000, 'Stale ADB health proof')
    const aapt = path.resolve(path.dirname(options.adb), '../build-tools/36.0.0/aapt.exe')
    const metadata = await execute(aapt, ['dump', 'badging', apk], { windowsHide: true, timeout: 10000, encoding: 'utf8' })
    assert.equal(/^package: name='([^']+)'/m.exec(metadata.stdout)?.[1], packageName, 'Refusing non-plugin-review APK before installation')
    await cutDeviceNetwork(serial, { run(target, command, { allowFailure = false } = {}) {
        assert.equal(target, serial)
        const result = spawnSync(options.adb, ['-s', target, ...command], { windowsHide: true, timeout: 5000, encoding: 'utf8' })
        if (!allowFailure) assert.equal(result.status, 0, 'Network cutoff failed')
        return result
    }, sleep: delay })
    assert.match((await run(['install', '-r', '-t', apk], 120000)).stdout, /Success/)
    assert.match((await run(['shell', 'pm', 'clear', packageName])).stdout, /Success/)
    await run(['shell', 'run-as', packageName, 'mkdir', '-p', 'files'])
    // The token is fixed-format hex and the destination is fixed inside this synthetic package.
    await run(['shell', 'run-as', packageName, 'sh', '-c', `'echo ${token} > files/risunest-plugin-review-owner'`])
    await run(['shell', 'am', 'start', '-n', `${packageName}/io.github.rsyumi.risunest.MainActivity`])
    launched = true
    let pid = ''
    const deadline = Date.now() + 30000
    while (!/^\d+$/.test(pid) && Date.now() < deadline) {
        pid = (await run(['shell', 'pidof', packageName], 5000, true)).stdout.trim()
        if (!/^\d+$/.test(pid)) await delay(250)
    }
    assert.match(pid, /^\d+$/)
    await new Promise((resolve, reject) => {
        const server = createServer()
        server.once('error', () => reject(new Error('CDP port unavailable')))
        server.listen(port, '127.0.0.1', () => server.close(resolve))
    })
    await run(['forward', '--no-rebind', `tcp:${port}`, `localabstract:webview_devtools_remote_${pid}`]); forwarded = true
    client = await connect()
    assert.equal(await client.evaluate('window.__pluginReview?.marker'), 'synthetic-plugin-review-v1')
    const environment = await client.evaluate('({userAgent:navigator.userAgent,visible:document.visibilityState === "visible"})')
    assert.equal(environment.visible, true)
    const result = await client.evaluate(`window.__pluginReview.run(${JSON.stringify(token)}).catch(error => ({passed:false,assertion:error.message}))`, 180000)
    const report = { timestamp: new Date().toISOString(), synthetic: true, platform: 'android-emulator', serial, avd,
        androidApi: Number((await run(['shell', 'getprop', 'ro.build.version.sdk'])).stdout.trim()),
        apkSha256: createHash('sha256').update(await readFile(apk)).digest('hex'), environment, ...result }
    await mkdir(path.dirname(output), { recursive: true })
    await writeFile(output, JSON.stringify(report, null, 2) + '\n')
    console.log(JSON.stringify(report))
    assert.equal(result.passed, true, 'Plugin Android acceptance failed')
}
main().catch(error => { console.error(error.message); process.exitCode = 1 }).finally(async () => {
    client?.close()
    if (launched) await run(['shell', 'am', 'force-stop', packageName], 10000, true)
    if (forwarded) await run(['forward', '--remove', `tcp:${port}`], 10000, true)
})
