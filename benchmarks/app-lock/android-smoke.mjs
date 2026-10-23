import assert from 'node:assert/strict'
import { spawn, spawnSync } from 'node:child_process'
import { pbkdf2Sync, createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { androidTarget, runnerOptions } from '../support/androidTarget.mjs'
import { connectSyntheticAndroid, delay, waitForInteractive } from '../startup/cdp.mjs'
import { seedExpression } from '../startup/fixture.mjs'
import { cutDeviceNetwork } from '../../scripts/phase3AndroidSmoke.mjs'

const options = runnerOptions(process.argv.slice(2), ['--reset-synthetic-profile', '--exercise-native-auth', '--exercise-auth-success'])
const android = androidTarget(options)
assert.ok(options.output && options['cdp-port'], 'Explicit --output and --cdp-port required')
const output = path.resolve(options.output)
const cdpPort = Number(options['cdp-port'])
assert.ok(Number.isInteger(cdpPort) && cdpPort >= 1024 && cdpPort <= 65535)
const packageName = 'io.github.rsyumi.risunest'
const component = `${packageName}/.MainActivity`
let client
let syntheticOsPin = false
let stage = 'identity'
const results = { phase: options.phase ?? 'all' }

function adb(args, { binary = false, allowFailure = false, timeout = 30_000 } = {}) {
    const result = spawnSync(android.adb, android.args(args), {
        env: android.env, encoding: binary ? null : 'utf8', timeout,
        windowsHide: true, maxBuffer: 80 * 1024 * 1024,
    })
    if (!allowFailure && (result.error || result.status !== 0)) throw new Error(`ADB failed during ${stage}`)
    return result
}
const networkRun = (serial, args, opts) => {
    assert.equal(serial, android.serial)
    return adb(args, opts)
}
async function waitFor(predicate, message, timeoutMs = 20_000) {
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
        if (await predicate()) return
        await delay(80)
    }
    throw new Error(message)
}
async function attach() {
    const pid = adb(['shell', 'pidof', packageName]).stdout.trim().split(/\s/)[0]
    assert.match(pid, /^\d+$/)
    adb(['forward', `tcp:${cdpPort}`, `localabstract:webview_devtools_remote_${pid}`])
    client = await connectSyntheticAndroid(cdpPort, android)
    await client.evaluate(`window.__syntheticFocusCalls = []; const originalFocus = HTMLElement.prototype.focus; HTMLElement.prototype.focus = function (...args) { window.__syntheticFocusCalls.push({tag:this.tagName,className:this.className,atMs:performance.now()}); return originalFocus.apply(this,args); }; window.__syntheticFocusEvents = []; document.addEventListener('focusin', event => window.__syntheticFocusEvents.push({tag:event.target.tagName,className:event.target.className,atMs:performance.now()})); true`)
    return pid
}
async function restart() {
    client?.close()
    client = undefined
    adb(['shell', 'am', 'force-stop', packageName])
    adb(['shell', 'am', 'start', '-W', '-n', component])
    return attach()
}
async function screenshot(name) {
    const file = path.join(output, `${name}.png`)
    const pixels = adb(['exec-out', 'screencap', '-p'], { binary: true }).stdout
    assert.ok(pixels.length > 100, 'Native screenshot was empty')
    await writeFile(file, pixels)
    return file
}
async function gated() {
    return client.evaluate(`!!document.querySelector('.app-lock-screen')`)
}
async function unlock() {
    for (const digit of '123456') {
        await client.evaluate(`(() => {
            const key = [...document.querySelectorAll('.app-lock-screen .keypad button')].find(key => key.textContent.trim() === ${JSON.stringify(digit)});
            if (!key || key.disabled) throw new Error('Key unavailable');
            key.click();
        })()`)
    }
    await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'PIN did not unlock')
    await client.evaluate(`(() => {
        if (!document.body.textContent.includes('To use RisuNest, accept')) return;
        const accept = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Accept');
        if (accept) accept.click();
    })()`);
    await delay(300)
}
async function foreground() {
    adb(['shell', 'am', 'start', '-W', '-n', component])
    await waitFor(gated, 'Return did not lock')
    await delay(300)
}
function record(nativeUnlock = false, hideRecentsPreview = true, timing = 'immediate') {
    const salt = Buffer.alloc(16, 7)
    return {
        schema: 'risunest.app-lock/v1', enabled: true,
        pin: { algorithm: 'PBKDF2-SHA256', iterations: 100_000, salt: salt.toString('base64'),
            hash: pbkdf2Sync('123456', salt, 100_000, 32, 'sha256').toString('base64') },
        timing, nativeUnlock, hideRecentsPreview,
    }
}
async function saveRecord(value) {
    await client.evaluate(`window.__TAURI_INTERNALS__.invoke('pds_set_device_setting', {key: 'risuNestAppLock', value: ${JSON.stringify(JSON.stringify(value))}})`)
}
async function marker() {
    await client.evaluate(`(() => {
        document.getElementById('synthetic-app-lock-marker')?.remove();
        const marker = document.createElement('div');
        marker.id = 'synthetic-app-lock-marker';
        marker.style.cssText = 'position:fixed;left:80px;top:120px;width:120px;height:120px;background:rgb(0,255,0);z-index:1000;pointer-events:none';
        document.querySelector('main').append(marker);
    })()`)
    await delay(250)
}
function greenPixels(file) {
    assert.ok(options.ffmpeg, '--ffmpeg is required for frame verification')
    const result = spawnSync(options.ffmpeg, ['-v', 'error', '-i', file, '-f', 'rawvideo', '-pix_fmt', 'rgb24', 'pipe:1'], {
        windowsHide: true, maxBuffer: 200 * 1024 * 1024, timeout: 30_000,
    })
    assert.equal(result.status, 0, 'Frame decoding failed')
    let green = 0
    for (let i = 0; i < result.stdout.length; i += 3) {
        if (result.stdout[i] < 25 && result.stdout[i + 1] > 230 && result.stdout[i + 2] < 25) green++
    }
    return green
}
async function returnVideo() {
    const remote = '/sdcard/risunest-app-lock-synthetic.mp4'
    const file = path.join(output, 'busy-return.mp4')
    const recorder = spawn(android.adb, android.args(['shell', 'screenrecord', '--time-limit', '5', '--size', '480x800', remote]), {
        env: android.env, windowsHide: true, stdio: 'ignore',
    })
    const completed = new Promise((resolve, reject) => {
        recorder.once('error', reject)
        recorder.once('exit', code => code === 0 ? resolve() : reject(new Error('Screen recording failed')))
    })
    await delay(250)
    adb(['shell', 'am', 'start', '-W', '-n', component])
    await completed
    adb(['pull', remote, file])
    adb(['shell', 'rm', remote])
    return file
}

async function captureSettings() {
    await marker()
    await waitFor(async () => greenPixels(await screenshot('settings-content-ready')) > 1000, 'Native cover did not release content before settings navigation')
    await client.evaluate(`document.getElementById('synthetic-app-lock-marker').remove()` )
    await client.evaluate(`(async () => {
        const visible = selector => [...document.querySelectorAll(selector)].filter(element => element.getClientRects().length);
        let settings = visible('svg.lucide-settings')[0];
        if (!settings) {
            visible('button').find(button => button.querySelector('svg.lucide-arrow-right') && !button.closest('.rs-sidebar'))?.click();
            await new Promise(resolve => setTimeout(resolve, 600));
            settings = visible('svg.lucide-settings')[0];
        }
        if (!settings) {
            visible('svg.lucide-list')[0]?.closest('button')?.click();
            await new Promise(resolve => setTimeout(resolve, 600));
            settings = visible('svg.lucide-settings')[0];
        }
        if (!settings) throw new Error('Settings action missing');
        settings.closest('button').click();
    })()`)
    await waitFor(() => client.evaluate(`[...document.querySelectorAll('button')].some(button => button.getClientRects().length && button.textContent.trim() === 'RisuNest')`), 'RisuNest settings menu missing')
    await client.evaluate(`[...document.querySelectorAll('button')].find(button => button.getClientRects().length && button.textContent.trim() === 'RisuNest').click()`)
    await waitFor(() => client.evaluate(`!!document.querySelector('#risunest-app-lock')`), 'App lock settings missing')
    await delay(700)
    await waitFor(async () => client.evaluate(`(() => { const group = document.querySelector('#risunest-app-lock'); group.scrollIntoView({block:'start',behavior:'instant'}); const box = group.getBoundingClientRect(); return box.top >= 0 && box.top < innerHeight / 3; })()`), 'App lock settings did not scroll into view')
    await delay(1000)
    results.settingsBounds = await client.evaluate(`(() => {const rect = document.querySelector('#risunest-app-lock').getBoundingClientRect(); return {top:rect.top,height:rect.height,viewport:innerHeight};})()`)
    await screenshot('settings')
    results.settingsCaptured = true
}

async function nativeAuthCancellation() {
    stage = 'native authentication cancellation'
    if (results.capabilities.auth.status === 'notEnrolled') {
        const enrolled = adb(['shell', 'locksettings', 'set-pin', '1234']).stdout
        assert.ok(enrolled.includes('Pin set'), 'Synthetic device PIN enrollment failed')
        syntheticOsPin = true
    }
    await saveRecord(record(true))
    await restart()
    await waitFor(gated, 'Native-enabled launch gate missing')
    await delay(600)
    results.enrolledCapabilities = await client.evaluate(`window.__TAURI_INTERNALS__.invoke('app_lock_check')`)
    assert.equal(results.enrolledCapabilities.auth.status, 'available')
    results.nativePromptFocused = adb(['shell', 'dumpsys', 'window']).stdout.split('\n').find(line => line.includes('mCurrentFocus'))?.trim()
    for (let attempt = 0; attempt < 3; attempt++) {
        adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
        await delay(400)
        const focus = adb(['shell', 'dumpsys', 'window']).stdout.split('\n').find(line => line.includes('mCurrentFocus')) ?? ''
        if (focus.includes(packageName + '/' + packageName + '.MainActivity')) break
    }
    await waitFor(() => client.evaluate(`[...document.querySelectorAll('.app-lock-screen .keypad button')].some(button => !button.disabled)`), 'Native cancellation did not restore PIN input')
    assert.equal(await gated(), true, 'Cancelling native auth removed PIN gate')
    await screenshot('native-auth-cancelled')
    if (options['exercise-auth-success'] && syntheticOsPin) {
        await client.evaluate(`document.querySelector('.app-lock-screen .native').click()`)
        await delay(500)
        adb(['shell', 'uiautomator', 'dump', '--compressed', '/sdcard/risunest-app-lock-auth.xml'])
        adb(['pull', '/sdcard/risunest-app-lock-auth.xml', path.join(output, 'native-auth-ui.xml')])
        adb(['shell', 'rm', '/sdcard/risunest-app-lock-auth.xml'])
        adb(['shell', 'input', 'text', '1234'])
        adb(['shell', 'input', 'keyevent', 'KEYCODE_ENTER'])
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Device PIN did not unlock the app')
        results.nativeAuthSuccess = true
    } else await unlock()
    results.nativeAuthCancellation = true
    await saveRecord(record())
}

await mkdir(output, { recursive: true })
try {
    android.assertAvd(adb(['emu', 'avd', 'name']).stdout)
    await cutDeviceNetwork(android.serial, { run: networkRun })
    results.apiLevel = Number(adb(['shell', 'getprop', 'ro.build.version.sdk']).stdout.trim())
    if (options['reset-synthetic-profile']) {
        assert.ok(options.apk, '--apk required to reset the synthetic profile')
        stage = 'install'
        const apk = path.resolve(options.apk)
        results.apkSha256 = createHash('sha256').update(await readFile(apk)).digest('hex')
        adb(['install', '-r', apk], { timeout: 120_000 })
        adb(['shell', 'pm', 'clear', packageName])
        adb(['shell', 'pm', 'grant', packageName, 'android.permission.POST_NOTIFICATIONS'], { allowFailure: true })
        await restart()
        await waitForInteractive(client)
        stage = 'seed'
        await client.evaluate(seedExpression({ characters: 1, bytes: 10_000, compatibility: false, pluginBytes: 0, locale: 'en' }))
        await saveRecord(record())
    }
    stage = 'launch gate'
    const pid = await restart()
    await waitFor(gated, 'Cold launch did not show PIN gate')
    results.capabilities = await client.evaluate(`window.__TAURI_INTERNALS__.invoke('app_lock_check')`)
    await delay(700)
    results.initialFocus = await client.evaluate(`({tag:document.activeElement?.tagName,className:document.activeElement?.className,events:window.__syntheticFocusEvents,calls:window.__syntheticFocusCalls})`)
    await screenshot('launch-gate')
    if (options.phase === 'seed') {
        results.seed = true
    } else if (options.phase === 'keyboard') {
        assert.ok(results.initialFocus.className?.includes('app-lock-screen'), 'Initial focus is not the gate root')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_TAB'])
        await waitFor(() => client.evaluate(`document.activeElement?.textContent.trim() === '1'`), 'First Tab did not focus digit1')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_ENTER'])
        await waitFor(() => client.evaluate(`document.querySelectorAll('.app-lock-screen .dot.filled').length === 1`), 'Enter did not activate focused digit')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_DEL'])
        await waitFor(() => client.evaluate(`document.querySelectorAll('.app-lock-screen .dot.filled').length === 0`), 'Hardware Backspace did not clear digit')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_1', 'KEYCODE_2', 'KEYCODE_3', 'KEYCODE_4', 'KEYCODE_5', 'KEYCODE_6'])
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Hardware PIN did not unlock')
        results.hardwarePin = true
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        await foreground()
        await waitFor(() => client.evaluate(`document.activeElement?.classList.contains('app-lock-screen')`), 'Return focus did not remain on gate root')
        await screenshot('keyboard-return-gate')
        results.returnFocus = true
        await unlock()
    } else if (options.phase === 'settings') {
        await unlock()
        await captureSettings()
    } else if (options.phase === 'timed') {
        await unlock()
        await saveRecord(record(false, false, 'after-1m'))
        await restart()
        await waitFor(gated, 'Timed-mode launch gate missing')
        await unlock()
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        await delay(1500)
        adb(['shell', 'am', 'start', '-W', '-n', component])
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Short timed return did not restore content')
        results.returnBeforeThreshold = true
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        console.log('Synthetic one-minute away interval started')
        await delay(31_000)
        console.log('Synthetic one-minute away interval continuing')
        await delay(31_000)
        await foreground()
        results.returnAfterThreshold = true
        await screenshot('timed-return-gate')
        await unlock()
        await saveRecord(record())
    } else if (options.phase === 'native-auth') {
        await unlock()
        await nativeAuthCancellation()
    } else {
        stage = 'PIN unlock'
        await unlock()
        await marker()
        results.unlockedGreenPixels = greenPixels(await screenshot('unlocked'))
        assert.ok(results.unlockedGreenPixels > 1000, 'Synthetic content marker not visible')
        stage = 'busy WebView return'
        // Busy rendering starts after this command replies; native cover must not depend on JS.
        await client.evaluate(`setTimeout(() => { const end = performance.now() + 3500; while (performance.now() < end) {} }, 100); true`)
        await delay(150)
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        const video = await returnVideo()
        await waitFor(gated, 'Busy return did not lock')
        results.busyReturnGreenPixels = greenPixels(video)
        assert.equal(results.busyReturnGreenPixels, 0, 'Previous app content appeared during busy return')
        await screenshot('return-gate')
        stage = 'gated Back without history'
        const initialHistory = await client.evaluate(`({length: history.length, hash: location.hash})`)
        adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
        const activities = adb(['shell', 'dumpsys', 'activity', 'activities']).stdout
        assert.ok(!activities.split('\n').some(line => /mResumedActivity|topResumedActivity/.test(line) && line.includes(packageName)), 'Gated Back did not background task')
        assert.equal(adb(['shell', 'pidof', packageName]).stdout.trim().split(/\s/)[0], pid, 'Gated Back exited process')
        await foreground()
        await unlock()
        assert.deepEqual(await client.evaluate(`({length: history.length, hash: location.hash})`), initialHistory)
        results.backWithoutHistory = true
        stage = 'gated Back with history'
        await client.call('Runtime.evaluate', { expression: `history.pushState({synthetic:true}, '', '#app-lock-synthetic-history')`, userGesture: true })
        const retainedHistory = await client.evaluate(`({length: history.length, hash: location.hash})`)
        assert.ok(retainedHistory.length > initialHistory.length, 'Synthetic WebView history not created')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        await foreground()
        adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
        await foreground()
        await unlock()
        assert.deepEqual(await client.evaluate(`({length: history.length, hash: location.hash})`), retainedHistory)
        results.backWithHistory = true
        if (options.phase !== 'standard') {
        stage = 'owned picker cancellation'
        await client.evaluate(`window.__syntheticAppLockEvents = []; window.addEventListener('risunest-app-lock', event => window.__syntheticAppLockEvents.push(event.detail)); window.RisuSafBridge.pickLegacyBackupSource(crypto.randomUUID()); true`)
        await waitFor(() => /mResumedActivity|topResumedActivity/.test(adb(['shell', 'dumpsys', 'activity', 'activities']).stdout.split('\n').filter(line => line.includes('documentsui')).join('')), 'Owned picker did not open')
        await delay(300)
        await screenshot('owned-picker')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Owned picker cancellation triggered lock')
        results.pickerCancellationEvents = await client.evaluate(`window.__syntheticAppLockEvents`)
        assert.ok(results.pickerCancellationEvents.every(event => event.active), 'Owned picker was classified as departure')
        await marker()
        assert.ok(greenPixels(await screenshot('owned-picker-return')) > 1000, 'Owned picker cover did not leave')
        stage = 'owned picker departure'
        await client.evaluate(`window.__syntheticAppLockEvents = []; window.RisuSafBridge.pickLegacyBackupSource(crypto.randomUUID()); true`)
        await delay(500)
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        await delay(500)
        await foreground()
        // Restoring the task may retain the picker above the root activity.
        adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
        adb(['shell', 'am', 'start', '-W', '-n', component])
        await waitFor(gated, 'Leaving an owned picker did not lock')
        results.pickerDepartureEvents = await client.evaluate(`window.__syntheticAppLockEvents`)
        assert.ok(results.pickerDepartureEvents.some(event => !event.active), 'Owned picker departure was not observed')
        await unlock()
        }
        stage = 'preview and screenshot'
        await marker()
        results.screenshotAllowedWithPreviewHidden = greenPixels(await screenshot('screenshot-preview-hidden')) > 1000
        assert.equal(results.screenshotAllowedWithPreviewHidden, true)
        adb(['shell', 'input', 'keyevent', 'KEYCODE_APP_SWITCH'])
        await delay(700)
        results.previewHiddenGreenPixels = greenPixels(await screenshot('recents-preview-hidden'))
        assert.equal(results.previewHiddenGreenPixels, 0)
        await foreground()
        await unlock()
        await saveRecord(record(false, false, 'launch'))
        await restart()
        await waitFor(gated, 'Launch-only gate missing')
        await unlock()
        await marker()
        adb(['shell', 'input', 'keyevent', 'KEYCODE_APP_SWITCH'])
        await delay(700)
        results.previewVisibleGreenPixels = greenPixels(await screenshot('recents-preview-visible'))
        assert.ok(results.previewVisibleGreenPixels > 100, 'Preview option did not restore app preview')
        adb(['shell', 'am', 'start', '-W', '-n', component])
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Launch-only return locked')
        results.launchOnlyReturn = true
        if (options['exercise-native-auth']) await nativeAuthCancellation()
    }
    results.success = true
} catch (error) {
    results.success = false
    results.failedStage = stage
    results.error = error.message
    await screenshot('failure').catch(() => {})
    process.exitCode = 1
} finally {
    client?.close()
    if (syntheticOsPin) adb(['shell', 'locksettings', 'clear', '--old', '1234'], { allowFailure: true })
    adb(['forward', '--remove', `tcp:${cdpPort}`], { allowFailure: true })
    await writeFile(path.join(output, 'result.json'), `${JSON.stringify(results, null, 2)}\n`)
    console.log(JSON.stringify(results))
}
