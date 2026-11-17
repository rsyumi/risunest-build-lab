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
    if (!allowFailure && (result.error || result.status !== 0)) throw new Error(`ADB failed during ${stage} (${args.slice(0,2).join(' ')}): ${result.error?.code ?? result.status} ${String(result.stderr).trim()}`)
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
    await waitFor(async () => (await nativeHierarchy('pin-before-input', true)).includes('class="android.webkit.WebView"'), 'Native gate did not become interactive')
    for (const digit of '123456') {
        await waitFor(() => client.evaluate(`(() => {
            const key = [...document.querySelectorAll('.app-lock-screen .keypad button')].find(key => key.textContent.trim() === ${JSON.stringify(digit)});
            if (!key || key.disabled) return false;
            key.click();
            return true;
        })()`), 'PIN keypad did not become interactive')
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
    await client.evaluate(`document.querySelector('button.absolute.top-3.left-0.border-borderc')?.click()`)
    await waitFor(() => client.evaluate(`!!document.querySelector('[data-char-id="synthetic-0"]')`), 'Synthetic character missing')
    await client.evaluate(`document.querySelector('[data-char-id="synthetic-0"]').click()`)
    await waitFor(() => client.evaluate(`!!document.querySelector('textarea.text-input-area')`), 'Synthetic conversation missing')
    await client.evaluate(`(() => {
        document.getElementById('synthetic-app-lock-marker')?.remove();
        const marker = document.createElement('div');
        marker.id = 'synthetic-app-lock-marker';
        marker.style.cssText = 'position:fixed;left:80px;top:120px;width:120px;height:120px;background:rgb(0,255,0);z-index:1000;pointer-events:none';
        document.querySelector('main').append(marker);
    })()`)
    await delay(250)
}
function greenPixels(file, minimumGreen = 230, bounds) {
    assert.ok(options.ffmpeg, '--ffmpeg is required for frame verification')
    const result = spawnSync(options.ffmpeg, ['-v', 'error', '-i', file, ...(bounds ? ['-vf', `crop=${bounds[2]-bounds[0]}:${bounds[3]-bounds[1]}:${bounds[0]}:${bounds[1]}`] : []), '-fps_mode', 'passthrough', '-f', 'rawvideo', '-pix_fmt', 'rgb24', 'pipe:1'], {
        windowsHide: true, maxBuffer: 600 * 1024 * 1024, timeout: 30_000,
    })
    assert.equal(result.status, 0, 'Frame decoding failed')
    let green = 0
    for (let i = 0; i < result.stdout.length; i += 3) {
        if (result.stdout[i] < 25 && result.stdout[i + 1] > minimumGreen && result.stdout[i + 2] < 25) green++
    }
    return green
}
async function returnVideo() {
    const homeComponent = adb(['shell', 'cmd', 'package', 'resolve-activity', '--brief', '-a', 'android.intent.action.MAIN', '-c', 'android.intent.category.HOME']).stdout.trim().split(/\r?\n/).at(-1)
    assert.ok(homeComponent.includes('/'), 'Synthetic launcher could not be resolved')
    const homePackage = homeComponent.split('/')[0]
    await waitFor(() => adb(['shell', 'dumpsys', 'window']).stdout.split('\n').some(line => line.includes('mCurrentFocus') && line.includes(homePackage)), 'Home did not take focus before return recording')
    assert.ok((await nativeHierarchy('home-before-return')).includes(`package="${homePackage}"`), 'Home did not present before return recording')
    results.busyHomeObservedAtMs = Number(adb(['shell', 'cat', '/proc/uptime']).stdout.split(' ')[0]) * 1000
    const remote = '/sdcard/risunest-app-lock-synthetic.mp4'
    const file = path.join(output, 'busy-return.mp4')
    const recorder = spawn(android.adb, android.args(['shell', 'screenrecord', '--time-limit', '12', '--size', '480x800', remote]), {
        env: android.env, windowsHide: true, stdio: 'ignore',
    })
    let recorderEnded = false
    const completed = new Promise((resolve, reject) => {
        recorder.once('error', reject)
        recorder.once('exit', code => { recorderEnded = true; code === 0 ? resolve() : reject(new Error('Screen recording failed')) })
    })
    await delay(250)
    results.busyClockBeforeWallMs = Number(adb(['shell', 'date', '+%s%3N']).stdout.trim())
    results.busyReturnRequestedAtMs = Number(adb(['shell', 'cat', '/proc/uptime']).stdout.split(' ')[0]) * 1000
    results.busyReturnRequestedWallMs = Number(adb(['shell', 'date', '+%s%3N']).stdout.trim())
    adb(['shell', 'am', 'start', '-W', '-n', component])
    results.busyReturnCompletedWallMs = Number(adb(['shell', 'date', '+%s%3N']).stdout.trim())
    await waitFor(gated, 'Busy return did not create PIN gate')
    await waitFor(async () => (await nativeHierarchy('busy-return-pin', true)).includes('class="android.webkit.WebView"'), 'Busy return did not present PIN gate')
    results.busyPinPresentedWallMs = Number(adb(['shell', 'date', '+%s%3N']).stdout.trim())
    assert.equal(recorderEnded, false, 'Recording ended before native PIN handover')
    await screenshot('busy-return-pin')
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

async function cancelOwnedPicker() {
    const focus = () => adb(['shell', 'dumpsys', 'window']).stdout.split('\n').find(line => line.includes('mCurrentFocus')) ?? ''
    const appFocused = () => focus().includes(packageName + '/' + packageName + '.MainActivity')
    for (let attempt = 0; attempt < 10; attempt++) {
        if (appFocused()) return
        assert.ok(focus().includes('documentsui'), 'Owned picker cancellation lost its target window')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
        try { await waitFor(appFocused, 'Owned picker did not dismiss', 2000); return }
        catch (error) { if (attempt === 9) throw error }
    }
}
function nativeFocus() {
    return adb(['shell', 'dumpsys', 'window']).stdout.split('\n').find(line => line.includes('mCurrentFocus')) ?? ''
}
function credentialFocused() {
    return nativeFocus().includes(results.apiLevel === 29 ? 'com.android.settings/com.android.settings.password.ConfirmLockPassword' : 'BiometricPrompt')
}
async function cancelNativePrompt() {
    const focus = nativeFocus
    const appFocused = () => focus().includes(packageName + '/' + packageName + '.MainActivity')
    for (let attempt = 0; attempt < 4; attempt++) {
        if (appFocused()) return
        assert.ok(credentialFocused(), 'Native auth cancellation lost its target window')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
        try { await waitFor(appFocused, 'Native authentication did not dismiss', 3000); return }
        catch (error) { if (attempt === 3) throw error }
    }
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
    await waitFor(credentialFocused, 'Automatic native credential prompt did not open')
    results.nativePromptFocused = adb(['shell', 'dumpsys', 'window']).stdout.split('\n').find(line => line.includes('mCurrentFocus'))?.trim()
    await cancelNativePrompt()
    await waitFor(() => client.evaluate(`[...document.querySelectorAll('.app-lock-screen .keypad button')].some(button => !button.disabled)`), 'Native cancellation did not restore PIN input')
    assert.equal(await gated(), true, 'Cancelling native auth removed PIN gate')
    await screenshot('native-auth-cancelled')
    if (results.apiLevel === 29) await stableCancelledGate('initial')
    if (options['exercise-auth-success'] && syntheticOsPin) {
        await client.evaluate(`document.querySelector('.app-lock-screen .native').click()`)
        await waitFor(credentialFocused, 'Native credential prompt did not open')
        await enterNativePin('native-auth-ui')
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Device PIN did not unlock the app')
        results.nativeAuthSuccess = true
    } else await unlock()
    results.nativeAuthCancellation = true
    if (results.apiLevel === 29 && options['exercise-auth-success'] && syntheticOsPin) await android29Continuation()
    await saveRecord(record())
}

async function enterNativePin(name) {
    assert.ok(credentialFocused(), 'PIN target is not the verified native prompt')
    const fieldId = results.apiLevel === 29 ? 'com.android.settings:id/password_entry' : 'com.android.systemui:id/lockPassword'
    await tapNativeNode(node => node.includes(`resource-id="${fieldId}"`) && node.includes('class="android.widget.EditText"'), name)
    adb(['shell', 'input', 'text', '1234'])
    adb(['shell', 'input', 'keyevent', 'KEYCODE_ENTER'])
}
async function stableCancelledGate(name) {
    await waitFor(() => client.evaluate(`!!document.querySelector('.app-lock-screen') && [...document.querySelectorAll('.app-lock-screen .keypad button')].some(button => !button.disabled)`), 'Cancellation did not leave usable common PIN')
    await delay(2200)
    assert.ok(nativeFocus().includes(`${packageName}/${packageName}.MainActivity`), 'Cancelled native prompt reopened')
    assert.equal(await gated(), true, 'Cancellation removed the common PIN gate')
    assert.equal(await client.evaluate(`[...document.querySelectorAll('.app-lock-screen .keypad button')].some(button => !button.disabled)`), true)
    ;(results.api29StableCancellations ??= []).push(name)
}
async function android29Continuation() {
    stage = 'Android 10 native continuation'
    const home = adb(['shell', 'cmd', 'package', 'resolve-activity', '--brief', '-a', 'android.intent.action.MAIN', '-c', 'android.intent.category.HOME']).stdout.trim().split(/\r?\n/).at(-1).split('/')[0]
    await client.evaluate(`window.__syntheticAppLockEvents=[];window.addEventListener('risunest-app-lock', event => window.__syntheticAppLockEvents.push(event.detail));true`)
    async function ordinaryHome(name) {
        const before = await client.evaluate(`window.__syntheticAppLockEvents.length`)
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        await waitFor(() => nativeFocus().includes(home), 'Ordinary Home did not reach launcher')
        adb(['shell', 'am', 'start', '-W', '-n', component])
        await waitFor(credentialFocused, 'Ordinary departure did not relock with native prompt')
        await cancelNativePrompt()
        await stableCancelledGate(name)
        const events = await client.evaluate(`window.__syntheticAppLockEvents.slice(${before})`)
        assert.ok(events.some(event => !event.active), 'Normal departure remained exempt after authentication ended')
        return events
    }
    results.api29AfterSuccessEvents = await ordinaryHome('after ordinary success')
    results.api29PendingOutcomes = []
    for (const key of ['KEYCODE_HOME', 'KEYCODE_APP_SWITCH']) {
        const before = await client.evaluate(`window.__syntheticAppLockEvents`)
        const epoch = before.at(-1).epoch
        await client.evaluate(`window.__syntheticAuthOutcome=null;window.__TAURI_INTERNALS__.invoke('app_lock_authenticate',{reason:'Synthetic native continuation verification'}).then(outcome=>window.__syntheticAuthOutcome=outcome,()=>window.__syntheticAuthOutcome='rejected');true`)
        await waitFor(credentialFocused, 'Public native command did not open credential activity')
        const latestTask = () => adb(['shell', 'dumpsys', 'activity', 'recents']).stdout.match(/Recent #\d+: TaskRecord\{[^\n]*?#(\d+) A=com\.android\.settings\.workmode/)?.[1]
        const task = latestTask()
        assert.ok(task, 'Pending credential task missing')
        adb(['shell', 'input', 'keyevent', key])
        await waitFor(() => nativeFocus().includes(home), 'Pending native request did not reach launcher')
        const awayOutcome = await client.evaluate(`window.__syntheticAuthOutcome`)
        assert.equal(awayOutcome, null, 'Native outcome completed while caller was away')
        let xml = await nativeHierarchy(`${key}-overview`)
        if (!xml.includes(`${home}:id/overview_panel`)) {
            adb(['shell', 'input', 'keyevent', 'KEYCODE_APP_SWITCH'])
            xml = await nativeHierarchy(`${key}-overview-open`)
        }
        assert.ok(xml.includes(`${home}:id/overview_panel`), 'Overview panel missing')
        assert.equal(latestTask(), task, 'Different credential task became most recent')
        const cards = [...xml.matchAll(/<node\s[^>]*>/g)].map(match => match[0]).filter(node => node.includes(`package="${home}"`) && node.includes('class="android.widget.FrameLayout"') && node.includes('resource-id=""') && node.includes('clickable="true"')).map(nodeBounds).filter(([x1,y1,x2,y2]) => x2-x1>300 && y2-y1>700)
        assert.equal(cards.length, 1, 'Ambiguous credential task card')
        tapBounds(cards[0])
        await waitFor(credentialFocused, 'Overview did not restore credential activity')
        const focused = adb(['shell', 'dumpsys', 'window']).stdout.split('\n').find(line => line.includes('mFocusedApp') && line.includes('ConfirmLockPassword'))
        assert.ok(focused && new RegExp(` t${task}[^0-9]`).test(focused), 'Restored credential task did not match pending request')
        await enterNativePin(`${key}-credential`)
        await waitFor(() => !credentialFocused(), 'Credential activity did not close')
        const beforeCallerResume = await client.evaluate(`window.__syntheticAuthOutcome`)
        if (!nativeFocus().includes(`${packageName}/${packageName}.MainActivity`)) {
            assert.equal(beforeCallerResume, null, 'Native outcome completed before caller regained focus')
            assert.ok(nativeFocus().includes(home), 'Unexpected activity after credential dismissal')
            adb(['shell', 'am', 'start', '-W', '-n', component])
        }
        await waitFor(() => client.evaluate(`window.__syntheticAuthOutcome !== null`), 'Native outcome did not settle after caller returned')
        const outcome = await client.evaluate(`window.__syntheticAuthOutcome`)
        assert.ok(['success', 'cancelled'].includes(outcome), 'Unexpected native continuation outcome')
        await stableCancelledGate(key)
        const events = await client.evaluate(`window.__syntheticAppLockEvents.slice(${before.length})`)
        assert.ok(events.every(event => event.active && event.epoch === epoch), 'Pending API29 authentication changed the departure episode')
        results.api29PendingOutcomes.push({key,task,epoch,awayOutcome,beforeCallerResume,outcome,events})
        // Public native invocation checks the adapter result; common PIN still owns this UI flow.
        await unlock()
        results.api29PendingOutcomes.at(-1).nextOrdinaryHomeEvents = await ordinaryHome(`after ${key}`)
    }
    await unlock()
}

async function nativeHierarchy(name, allowUnavailable = false, compressed = true) {
    const dump = adb(['shell', 'uiautomator', 'dump', ...(compressed ? ['--compressed'] : []), '/sdcard/risunest-app-lock-review.xml'])
    if (!dump.stdout.includes('dumped to:')) {
        await writeFile(path.join(output, `${name}-dump.txt`), dump.stdout + dump.stderr)
        if (allowUnavailable) {
            (results.unavailableHierarchyRoots ??= []).push(name)
            return ''
        }
        throw new Error(`Native hierarchy unavailable: ${name}`)
    }
    const xml = adb(['shell', 'cat', '/sdcard/risunest-app-lock-review.xml']).stdout
    adb(['shell', 'rm', '/sdcard/risunest-app-lock-review.xml'])
    await writeFile(path.join(output, `${name}.xml`), xml)
    return xml
}
async function tapNativeNode(predicate, name) {
    const xml = await nativeHierarchy(name)
    const node = [...xml.matchAll(/<node\s[^>]*>/g)].map(match => match[0]).find(predicate)
    assert.ok(node, `Native UI action unavailable: ${name}`)
    const bounds = node.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/)
    assert.ok(bounds)
    adb(['shell', 'input', 'tap', String((Number(bounds[1]) + Number(bounds[3])) / 2), String((Number(bounds[2]) + Number(bounds[4])) / 2)])
}
async function presentationPolicyFlow() {
    stage = 'same-epoch presentation policy transition'
    const outcomes = await client.evaluate(`(async () => {
        const invoke = window.__TAURI_INTERNALS__.invoke;
        const policy = {enabled:true,timing:'immediate',gateRequired:true,hideRecentsPreview:true,backgroundColor:'#111827'};
        const snapshot = await invoke('app_lock_set_policy', {policy});
        const epoch = snapshot.epoch;
        const pairs = [];
        for (let attempt = 0; attempt < 20; attempt++) {
            await invoke('app_lock_set_policy', {policy});
            if (!await invoke('app_lock_ready', {epoch,locked:true})) throw new Error('Initial locked frame was rejected');
            const pending = invoke('app_lock_ready', {epoch,locked:true});
            const changing = invoke('app_lock_set_policy', {policy:{...policy,gateRequired:false}});
            const end = performance.now() + 60;
            while (performance.now() < end) {}
            const changed = await changing;
            const oldReady = await pending;
            const currentReady = await invoke('app_lock_ready', {epoch,locked:false});
            pairs.push({oldReady,currentReady,epoch:changed.epoch});
            if (!currentReady || !oldReady) break;
        }
        return {epoch,pairs};
    })()`)
    results.presentationPolicyTransition = outcomes
    assert.ok(outcomes.pairs.every(pair => pair.epoch === outcomes.epoch), 'Policy changed the lifecycle epoch')
    assert.ok(outcomes.pairs.every(pair => pair.currentReady), 'Stale locked presentation replaced the current unlocked policy')
    assert.ok(outcomes.pairs.some(pair => !pair.oldReady), 'No pending presentation overlap was observed')
    results.presentationUnchangedPolicies = await client.evaluate(`(async () => {
        const invoke = window.__TAURI_INTERNALS__.invoke;
        const policy = {enabled:true,timing:'immediate',gateRequired:false,hideRecentsPreview:true,backgroundColor:'#111827'};
        const {epoch} = await invoke('app_lock_set_policy', {policy});
        const outcomes = [];
        for (const backgroundColor of ['#111827', '#222222']) {
            const pending = invoke('app_lock_ready', {epoch,locked:false});
            const unchanged = invoke('app_lock_set_policy', {policy:{...policy,backgroundColor}});
            const end = performance.now() + 60;
            while (performance.now() < end) {}
            await unchanged;
            outcomes.push(await pending);
        }
        return outcomes;
    })()`)
    assert.deepEqual(results.presentationUnchangedPolicies, [true, true], 'Unchanged gate policy cancelled current presentation')
    assert.ok((await nativeHierarchy('presentation-current-frame')).includes('class="android.webkit.WebView"'), 'Current frame did not restore WebView accessibility')
}
async function waitForStoppedTask(name) {
    await waitFor(async () => {
        const dump = adb(['shell', 'dumpsys', 'activity', 'activities']).stdout
        const roots = dump.split(/(?=^  \* Task\{)/m)
        const task = roots.find(block => block.split('\n')[0].includes(packageName))
        const states = [...(task ?? '').matchAll(/^[ \t]*state=(\w+)/gm)].map(match => match[1])
        if (!states.length || states.some(state => state !== 'STOPPED')) return false
        await writeFile(path.join(output, name + '.txt'), dump)
        return true
    }, 'Owned task did not reach a stopped background state')
    await delay(250)
}

async function pickerFlows() {
    stage = 'owned picker cancellation'
    await client.evaluate(`window.__syntheticAppLockEvents = []; window.addEventListener('risunest-app-lock', event => window.__syntheticAppLockEvents.push(event.detail)); window.RisuSafBridge.pickLegacyBackupSource(crypto.randomUUID()); true`)
    await waitFor(() => /mResumedActivity|topResumedActivity/.test(adb(['shell', 'dumpsys', 'activity', 'activities']).stdout.split('\n').filter(line => line.includes('documentsui')).join('')), 'Owned picker did not open')
    await delay(300)
    await screenshot('owned-picker')
    await cancelOwnedPicker()
    await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Owned picker cancellation triggered lock')
    results.pickerCancellationEvents = await client.evaluate(`window.__syntheticAppLockEvents`)
    assert.ok(results.pickerCancellationEvents.every(event => event.active), 'Owned picker was classified as departure')
    await marker()
    await waitFor(async () => greenPixels(await screenshot('owned-picker-return')) > 1000, 'Owned picker cover did not leave')
    stage = 'owned picker departure'
    await client.evaluate(`window.__syntheticAppLockEvents = []; window.RisuSafBridge.pickLegacyBackupSource(crypto.randomUUID()); true`)
    await delay(500)
    adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
    await delay(500)
    adb(['shell', 'am', 'start', '-W', '-n', component])
    // Restoring the task may retain the picker above the root activity.
    await cancelOwnedPicker()
    await waitFor(async () => (await nativeHierarchy('rapid-picker-presented', true)).includes('class="android.webkit.WebView"'), 'Rapid picker return did not present a current frame')
    results.rapidPickerDepartureEvents = await client.evaluate(`window.__syntheticAppLockEvents`)
    results.rapidPickerDepartureObserved = results.rapidPickerDepartureEvents.some(event => !event.active)
    if (results.rapidPickerDepartureObserved) {
        await waitFor(gated, 'Observed rapid picker departure did not lock')
        await unlock()
    } else {
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Unobserved picker continuation did not resume')
    }
    if (results.apiLevel >= 32) {
        stage = 'observable owned picker departure'
        await client.evaluate(`window.__syntheticAppLockEvents = []; window.RisuSafBridge.pickLegacyBackupSource(crypto.randomUUID()); true`)
        await waitFor(() => /mResumedActivity|topResumedActivity/.test(adb(['shell', 'dumpsys', 'activity', 'activities']).stdout.split('\n').filter(line => line.includes('documentsui')).join('')), 'Owned picker did not reopen')
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        await waitForStoppedTask('picker-home-tasks')
        results.pickerReturnRequestedAtMs = Number(adb(['shell', 'cat', '/proc/uptime']).stdout.split(' ')[0]) * 1000
        adb(['shell', 'am', 'start', '-W', '-n', component])
        await cancelOwnedPicker()
        await waitFor(gated, 'Observed picker departure did not lock')
        results.pickerDepartureEvents = await client.evaluate(`window.__syntheticAppLockEvents`)
        assert.ok(results.pickerDepartureEvents.some(event => !event.active && event.departedAtMs < results.pickerReturnRequestedAtMs), 'Sustained picker departure was not recorded before return')
        await unlock()
    }
}

function nodeBounds(node) {
    const match = node?.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/)
    assert.ok(match, 'Native node bounds missing')
    return match.slice(1).map(Number)
}
function tapBounds([x1, y1, x2, y2]) {
    adb(['shell', 'input', 'tap', String((x1+x2)/2), String((y1+y2)/2)])
}
async function appPreview(name) {
    const home = adb(['shell', 'cmd', 'package', 'resolve-activity', '--brief', '-a', 'android.intent.action.MAIN', '-c', 'android.intent.category.HOME']).stdout.trim().split(/\r?\n/).at(-1).split('/')[0]
    adb(['shell', 'input', 'keyevent', 'KEYCODE_APP_SWITCH'])
    for (let attempt = 0; attempt < 12; attempt++) {
        let xml
        await waitFor(async () => {
            xml = await nativeHierarchy(`${name}-overview-${attempt}`, false, false)
            return xml.includes(`resource-id="${home}:id/overview_panel"`)
        }, 'Overview panel did not appear')
        const nodes = [...xml.matchAll(/<node\s[^>]*>/g)].map(match => match[0])
        const icon = nodes.find(node => node.includes(`resource-id="${home}:id/icon"`) && node.includes('clickable="true"'))
        if (icon) {
            tapBounds(nodeBounds(icon))
            const menu = await nativeHierarchy(`${name}-identity-${attempt}`)
            const identity = [...menu.matchAll(/<node\s[^>]*>/g)].map(match => match[0]).find(node => node.includes(`resource-id="${home}:id/task_name"`))
            assert.ok(identity, 'Overview app identity menu missing')
            const app = identity.match(/text="([^"]*)"/)?.[1]
            adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK'])
            if (app === 'RisuNest') {
                const restored = await nativeHierarchy(`${name}-confirmed-card`, false, false)
                assert.ok(restored.includes(`resource-id="${home}:id/overview_panel"`) && !restored.includes(`resource-id="${home}:id/task_name"`), 'Overview menu did not dismiss')
                const snapshots = [...restored.matchAll(/<node\s[^>]*>/g)].map(match => match[0]).filter(node => node.includes(`resource-id="${home}:id/snapshot"`)).map(nodeBounds)
                const bounds = snapshots.sort((a,b) => (b[2]-b[0]) - (a[2]-a[0]))[0]
                assert.ok(bounds && bounds[2]-bounds[0] > 300, 'Confirmed app snapshot was not visible')
                const file = await screenshot(name)
                return { app, bounds, greenPixels: greenPixels(file, 230, bounds) }
            }
        }
        const panel = nodes.find(node => node.includes(`resource-id="${home}:id/overview_panel"`))
        const [x1,y1,x2,y2] = nodeBounds(panel)
        adb(['shell', 'input', 'swipe', String(x1+(x2-x1)*0.8), String((y1+y2)/2), String(x1+(x2-x1)*0.2), String((y1+y2)/2), '350'])
    }
    throw new Error('RisuNest task card was not found in Overview')
}
async function previewFlows() {
    stage = 'preview and screenshot'
    for (const hidden of [true, false]) {
        await saveRecord(record(false, hidden, hidden ? 'immediate' : 'launch'))
        await restart()
        await waitFor(gated, 'Preview-mode launch gate missing')
        await unlock()
        await marker()
        const suffix = hidden ? 'hidden' : 'visible'
        await waitFor(async () => greenPixels(await screenshot(`screenshot-preview-${suffix}`)) > 1000, 'Preview content marker did not reach the native screen')
        const setting = JSON.parse(await client.evaluate(`window.__TAURI_INTERNALS__.invoke('app_lock_read_record')`))
        assert.equal(setting.hideRecentsPreview, hidden)
        assert.equal(setting.timing, hidden ? 'immediate' : 'launch')
        await writeFile(path.join(output, `preview-${suffix}-task-before.txt`), adb(['shell', 'dumpsys', 'activity', 'activities']).stdout)
        const preview = await appPreview(`recents-preview-${suffix}`)
        results[hidden ? 'previewHiddenCard' : 'previewVisibleCard'] = preview
        results[hidden ? 'previewHiddenGreenPixels' : 'previewVisibleGreenPixels'] = preview.greenPixels
        if (hidden) {
            results.screenshotAllowedWithPreviewHidden = true
            assert.equal(preview.greenPixels, 0, 'Hidden app preview exposed content')
            await foreground()
            await unlock()
        } else {
            assert.ok(preview.greenPixels > 100, 'Preview option did not restore app preview')
            adb(['shell', 'am', 'start', '-W', '-n', component])
            await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Launch-only return locked')
            results.launchOnlyReturn = true
        }
    }
    if (options['exercise-native-auth']) await nativeAuthCancellation()
    await saveRecord(record())
}

async function reviewFlows() {
    await unlock()
    if (options.phase === 'review-preview') { await previewFlows(); return }
    if (options.phase === 'review-picker') {
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        await foreground()
        await unlock()
        await pickerFlows()
        return
    }
    if (options.phase === 'review-departure' || options.phase === 'review-permission-home') { await permissionDepartureFlows(); return }
    if (options.phase === 'review-auth') { await authOverviewFlow(); return }
    await marker()
    stage = 'foreground native select'
    await client.evaluate(`window.__syntheticAppLockEvents = []; window.addEventListener('risunest-app-lock', event => window.__syntheticAppLockEvents.push(event.detail)); (() => {
        const select = document.createElement('select'); select.id = 'synthetic-native-select';
        select.style.cssText = 'position:fixed;left:10px;top:70px;width:180px;height:40px;z-index:999999';
        select.innerHTML = '<option value="one">Fixture one</option><option value="two">Fixture two</option>';
        document.body.append(select);
    })()`)
    const point = await client.evaluate(`(() => {const r=document.getElementById('synthetic-native-select').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`)
    await client.call('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] })
    await client.call('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await delay(350)
    results.selectPopupGreenPixels = greenPixels(await screenshot('native-select-open'), 100)
    assert.ok(results.selectPopupGreenPixels > 1000, 'Focus-only native select concealed app content')
    await tapNativeNode(node => node.includes('text="Fixture two"'), 'native-select-ui')
    await waitFor(() => client.evaluate(`document.getElementById('synthetic-native-select').value === 'two'`), 'Native select value did not commit')
    assert.equal(await gated(), false)
    results.selectEvents = await client.evaluate(`window.__syntheticAppLockEvents`)
    assert.ok(results.selectEvents.every(event => event.active), 'Native select counted as departure')
    await client.evaluate(`document.getElementById('synthetic-native-select').remove()`)
    results.selectContinuation = true
    if (options.phase === 'review-focus') return

    for (const grant of [false, true]) {
        stage = `camera permission ${grant ? 'grant' : 'deny'}`
        await client.evaluate(`window.__syntheticAppLockEvents = []; window.__syntheticPermission = null; window.__TAURI_INTERNALS__.invoke('plugin:barcode-scanner|request_permissions').then(value => window.__syntheticPermission = value, error => window.__syntheticPermission = {error:String(error)}); true`)
        await waitFor(() => adb(['shell', 'dumpsys', 'window']).stdout.split('\n').some(line => line.includes('mCurrentFocus') && line.includes('GrantPermissionsActivity')), 'Camera permission dialog missing')
        await screenshot(grant ? 'permission-grant-open' : 'permission-deny-open')
        await tapNativeNode(node => node.includes(grant ? 'permission_allow_foreground_only_button' : 'permission_deny_button'), grant ? 'permission-grant-ui' : 'permission-deny-ui')
        await waitFor(() => client.evaluate(`window.__syntheticPermission !== null`), 'Permission result missing')
        await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Permission result unexpectedly locked the app')
        const events = await client.evaluate(`window.__syntheticAppLockEvents`)
        assert.ok(events.every(event => event.active), 'Permission dialog counted as departure')
        await waitFor(async () => greenPixels(await screenshot(grant ? 'permission-grant-return' : 'permission-deny-return')) > 1000, 'Permission cover did not release')
        results[grant ? 'permissionGrant' : 'permissionDeny'] = { events, outcome: await client.evaluate(`window.__syntheticPermission`) }
    }
    if (options.phase === 'review-permission') return
    stage = 'native cover accessibility'
    assert.ok((await nativeHierarchy('accessibility-before')).includes('class="android.webkit.WebView"'), 'WebView was not exposed to native accessibility before cover')
    const policy = { enabled: true, timing: 'immediate', gateRequired: true, hideRecentsPreview: true, backgroundColor: '#111827' }
    const snapshot = await client.evaluate(`window.__TAURI_INTERNALS__.invoke('app_lock_set_policy', {policy:${JSON.stringify(policy)}})`)
    assert.ok(!(await nativeHierarchy('accessibility-covered')).includes('class="android.webkit.WebView"'), 'Native cover left WebView actions accessible')
    assert.equal(await client.evaluate(`window.__TAURI_INTERNALS__.invoke('app_lock_ready', {epoch:${snapshot.epoch},locked:true})`), true)
    await client.evaluate(`window.__TAURI_INTERNALS__.invoke('app_lock_set_policy', {policy:${JSON.stringify({...policy,gateRequired:false})}})`)
    assert.ok((await nativeHierarchy('accessibility-restored')).includes('class="android.webkit.WebView"'), 'WebView accessibility did not restore after cover')
    results.nativeCoverAccessibility = true
    if (options.phase === 'review-accessibility') return
    await permissionDepartureFlows()
    await authOverviewFlow()
}
async function permissionDepartureFlows() {
    if (options.phase !== 'review-permission-home') {
        stage = 'notification permission preparation'
        adb(['shell', 'pm', 'revoke', packageName, 'android.permission.POST_NOTIFICATIONS'])
        adb(['shell', 'pm', 'set-permission-flags', packageName, 'android.permission.POST_NOTIFICATIONS', 'user-fixed'])
        await restart()
        await waitFor(gated, 'Notification fixture gate missing')
        await unlock()
        await marker()
        adb(['shell', 'pm', 'clear-permission-flags', packageName, 'android.permission.POST_NOTIFICATIONS', 'user-fixed', 'user-set'])
        for (const grant of [false, true]) {
            stage = `notification permission ${grant ? 'grant' : 'deny'}`
            await client.evaluate(`window.__syntheticAppLockEvents = []; window.addEventListener('risunest-app-lock', event => window.__syntheticAppLockEvents.push(event.detail)); window.__syntheticPermission = null; window.RisuGenerationKeepAlive.requestNotifications().then(value => window.__syntheticPermission = value); true`)
            await waitFor(() => adb(['shell', 'dumpsys', 'window']).stdout.split('\n').some(line => line.includes('mCurrentFocus') && line.includes('GrantPermissionsActivity')), 'Notification permission dialog missing')
            await tapNativeNode(node => node.includes(grant ? 'permission_allow_button' : 'permission_deny_button'), grant ? 'notification-grant-ui' : 'notification-deny-ui')
            await waitFor(() => client.evaluate(`window.__syntheticPermission !== null`), 'Notification result missing')
            await waitFor(() => client.evaluate(`!document.querySelector('.app-lock-screen,.app-lock-cover')`), 'Notification result locked the app')
            const events = await client.evaluate(`window.__syntheticAppLockEvents`)
            assert.ok(events.every(event => event.active), 'Notification permission counted as departure')
            results[grant ? 'notificationGrant' : 'notificationDeny'] = {events, outcome:await client.evaluate(`window.__syntheticPermission`)}
        }
    }
    results.permissionHome = {}
    for (const sustained of [false, true]) {
        const mode = sustained ? 'sustained' : 'rapid'
        stage = 'camera permission Home ' + mode
        const observation = results.permissionHome[mode] = {}
        adb(['shell', 'pm', 'revoke', packageName, 'android.permission.CAMERA'], {allowFailure:true})
        adb(['shell', 'pm', 'clear-permission-flags', packageName, 'android.permission.CAMERA', 'user-fixed', 'user-set'])
        await restart()
        await waitFor(gated, 'Permission departure fixture gate missing')
        await unlock()
        await marker()
        const homeComponent = adb(['shell', 'cmd', 'package', 'resolve-activity', '--brief', '-a', 'android.intent.action.MAIN', '-c', 'android.intent.category.HOME']).stdout.trim().split(/\r?\n/).at(-1)
        assert.ok(homeComponent.includes('/'), 'Synthetic launcher could not be resolved')
        await client.evaluate(`window.__syntheticAppLockEvents = []; window.addEventListener('risunest-app-lock', event => window.__syntheticAppLockEvents.push(event.detail)); window.__TAURI_INTERNALS__.invoke('plugin:barcode-scanner|request_permissions'); true`)
        await waitFor(() => adb(['shell', 'dumpsys', 'window']).stdout.split('\n').some(line => line.includes('mCurrentFocus') && line.includes('GrantPermissionsActivity')), 'Departure permission dialog missing')
        observation.startedAtMs = Number(adb(['shell', 'cat', '/proc/uptime']).stdout.split(' ')[0]) * 1000
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        if (sustained) await waitForStoppedTask('permission-sustained-home-tasks')
        else await delay(1800)
        observation.focus = adb(['shell', 'dumpsys', 'window']).stdout.split('\n').find(line => line.includes('mCurrentFocus'))?.trim()
        assert.ok(observation.focus?.includes(homeComponent.split('/')[0]), 'Home did not take focus during the permission test')
        observation.returningAtMs = Number(adb(['shell', 'cat', '/proc/uptime']).stdout.split(' ')[0]) * 1000
        adb(['shell', 'am', 'start', '-W', '-n', component])
        if (adb(['shell', 'dumpsys', 'window']).stdout.split('\n').some(line => line.includes('mCurrentFocus') && line.includes('GrantPermissionsActivity'))) {
            await tapNativeNode(node => node.includes('permission_deny_button'), 'permission-' + mode + '-return-ui')
        }
        await waitFor(async () => (await nativeHierarchy('permission-' + mode + '-presented', true)).includes('class="android.webkit.WebView"'), 'Permission return did not present a current frame')
        observation.events = await client.evaluate(`window.__syntheticAppLockEvents`)
        observation.departureObserved = observation.events.some(event => !event.active)
        if (sustained) assert.ok(observation.events.some(event => !event.active && event.departedAtMs < observation.returningAtMs), 'Observed permission departure was not recorded before return')
        if (observation.departureObserved) {
            await waitFor(gated, 'Observed permission departure did not lock')
            assert.equal(await client.evaluate(`window.__TAURI_INTERNALS__.invoke('app_lock_ready', {epoch:0,locked:true})`), false, 'Stale permission-era frame was accepted')
            await unlock()
        } else {
            assert.equal(await client.evaluate(`!!document.querySelector('.app-lock-screen,.app-lock-cover')`), false, 'Unobserved permission continuation did not resume')
        }
    }
}

async function authOverviewFlow() {
    await nativeAuthCancellation()
    await restart()
    await waitFor(gated, 'Manual-auth fixture gate missing')
    await unlock()
    stage = 'native auth Overview departure'
    await client.evaluate(`window.__syntheticAppLockEvents = []; window.addEventListener('risunest-app-lock', event => window.__syntheticAppLockEvents.push(event.detail)); window.__syntheticAuthOutcome = null; window.__TAURI_INTERNALS__.invoke('app_lock_authenticate', {reason:'Synthetic app-lock verification'}).then(value => window.__syntheticAuthOutcome = value); true`)
    await waitFor(() => adb(['shell', 'dumpsys', 'window']).stdout.split('\n').some(line => line.includes('mCurrentFocus') && line.includes('BiometricPrompt')), 'Native authentication prompt missing')
    results.authOverviewStartedAtMs = Number(adb(['shell', 'cat', '/proc/uptime']).stdout.split(' ')[0]) * 1000
    adb(['shell', 'input', 'keyevent', 'KEYCODE_APP_SWITCH'])
    const homeComponent = adb(['shell', 'cmd', 'package', 'resolve-activity', '--brief', '-a', 'android.intent.action.MAIN', '-c', 'android.intent.category.HOME']).stdout.trim().split(/\r?\n/).at(-1)
    assert.ok(homeComponent.includes('/'), 'Synthetic launcher could not be resolved')
    const homePackage = homeComponent.split('/')[0]
    await waitFor(() => adb(['shell', 'dumpsys', 'window']).stdout.split('\n').some(line => line.includes('mCurrentFocus') && line.includes(homePackage)), 'Overview did not take focus')
    await delay(600)
    await screenshot('native-auth-overview')
    results.authOverviewReturningAtMs = Number(adb(['shell', 'cat', '/proc/uptime']).stdout.split(' ')[0]) * 1000
    adb(['shell', 'am', 'start', '-W', '-n', component])
    results.authOverviewEvents = await client.evaluate(`window.__syntheticAppLockEvents`)
    assert.ok(results.authOverviewEvents.some(event => !event.active && event.departedAtMs < results.authOverviewReturningAtMs), 'Overview departure was not recorded before return')
    // The credential prompt may remain above the app on return. Cancel it before checking PIN.
    await cancelNativePrompt()
    await waitFor(gated, 'Overview return after native auth did not lock')
    await waitFor(() => client.evaluate(`window.__syntheticAuthOutcome !== null`), 'Departed native authentication remained pending')
    results.authOverviewOutcome = await client.evaluate(`window.__syntheticAuthOutcome`)
    assert.equal(results.authOverviewOutcome, 'cancelled', 'Departed native auth result was accepted')
    await unlock()
}

await mkdir(output, { recursive: true })
try {
    android.assertAvd(adb(['emu', 'avd', 'name']).stdout)
    await cutDeviceNetwork(android.serial, { run: networkRun })
    results.apiLevel = Number(adb(['shell', 'getprop', 'ro.build.version.sdk']).stdout.trim())
    if (options.phase?.startsWith('review')) assert.equal(results.apiLevel, 34, 'Review phases require an Android 14 (API 34) synthetic AVD')
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
    if (options.phase === 'review-presentation') {
        await presentationPolicyFlow()
    } else if (options.phase?.startsWith('review')) {
        await reviewFlows()
    } else if (options.phase === 'seed') {
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
        await client.evaluate(`window.__syntheticReturnEvents = []; window.addEventListener('risunest-app-lock', event => window.__syntheticReturnEvents.push(event.detail)); setTimeout(() => { window.__syntheticBusy = {startWallMs:Date.now()}; const end = performance.now() + 10000; while (performance.now() < end) {} window.__syntheticBusy.endWallMs = Date.now(); }, 100); true`)
        await delay(150)
        adb(['shell', 'input', 'keyevent', 'KEYCODE_HOME'])
        const video = await returnVideo()
        await waitFor(gated, 'Busy return did not lock')
        results.busyJavascript = await client.evaluate(`window.__syntheticBusy`)
        results.busyLifecycleEvents = await client.evaluate(`window.__syntheticReturnEvents`)
        assert.ok(results.busyLifecycleEvents.some(event => !event.active && event.departedAtMs <= results.busyHomeObservedAtMs), 'Native departure did not precede observed Home')
        const nativeReturn = results.busyLifecycleEvents.find(event => event.active && event.epoch > 0 && event.atMs >= results.busyReturnRequestedAtMs)
        assert.ok(nativeReturn, 'Native return event missing after foreground request')
        const nativeReturnOffset = nativeReturn.atMs - results.busyReturnRequestedAtMs
        results.busyNativeReturnWallRange = [results.busyClockBeforeWallMs + nativeReturnOffset, results.busyReturnRequestedWallMs + nativeReturnOffset]
        results.busyNativeReturnMarginsMs = [results.busyNativeReturnWallRange[0] - results.busyJavascript.startWallMs, results.busyJavascript.endWallMs - results.busyNativeReturnWallRange[1]]
        assert.ok(results.busyNativeReturnMarginsMs.every(margin => margin > 250), 'Actual native return was not safely inside the stalled WebView interval')
        assert.ok(results.busyJavascript.startWallMs < results.busyReturnRequestedWallMs && results.busyReturnCompletedWallMs < results.busyJavascript.endWallMs, 'Return did not overlap the busy WebView interval')
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
        if (options.phase !== 'standard') await pickerFlows()
        await previewFlows()
    }
    results.success = true
} catch (error) {
    results.success = false
    results.failedStage = stage
    results.error = error.message
    results.finalNativeState = await client?.evaluate(`({events:window.__syntheticAppLockEvents,gated:!!document.querySelector('.app-lock-screen'),covered:!!document.querySelector('.app-lock-cover')})`).catch(() => undefined)
    await screenshot('failure').catch(() => {})
    process.exitCode = 1
} finally {
    client?.close()
    if (syntheticOsPin) adb(['shell', 'locksettings', 'clear', '--old', '1234'], { allowFailure: true })
    adb(['forward', '--remove', `tcp:${cdpPort}`], { allowFailure: true })
    await writeFile(path.join(output, 'result.json'), `${JSON.stringify(results, null, 2)}\n`)
    console.log(JSON.stringify(results))
}
