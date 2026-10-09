import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import test from 'node:test'
import { androidTarget, runnerOptions } from './androidTarget.mjs'

const selection = { serial: 'emulator-5690', avd: 'portable-synthetic', 'adb-port': '16222' }

test('requires an explicit emulator, AVD, and valid local server port', () => {
    const options = { ...selection, adb: 'adb' }
    for (const key of ['serial', 'avd', 'adb-port']) {
        assert.throws(() => androidTarget({ ...options, [key]: undefined }, {}))
    }
    for (const serial of ['physical-device', '', '-e']) {
        assert.throws(() => androidTarget({ ...options, serial }, {}), /disposable emulator/)
    }
    for (const port of ['0', '1023', '65536', 'NaN', '16222x']) {
        assert.throws(() => androidTarget({ ...options, 'adb-port': port }, {}), /adb-port/)
    }
    assert.throws(() => androidTarget(selection, {}), /ANDROID_HOME/)
})

test('selects platform SDK tools and scopes conflicting ADB settings to children', () => {
    for (const [platform, sdk, adb, aapt] of [
        ['win32', 'Q:\\SDK With Spaces', 'Q:\\SDK With Spaces\\platform-tools\\adb.exe', 'Q:\\SDK With Spaces\\build-tools\\36.0.0\\aapt.exe'],
        ['linux', '/opt/android sdk', '/opt/android sdk/platform-tools/adb', '/opt/android sdk/build-tools/36.0.0/aapt'],
        ['darwin', '/opt/android sdk', '/opt/android sdk/platform-tools/adb', '/opt/android sdk/build-tools/36.0.0/aapt'],
    ]) {
        const env = { ANDROID_HOME: sdk, ADB_SERVER_SOCKET: 'tcp:elsewhere:9999', ANDROID_ADB_SERVER_PORT: '9999' }
        const android = androidTarget(selection, env, platform)
        assert.equal(android.adb, adb)
        assert.equal(android.aapt, aapt)
        assert.equal(android.env.ADB_SERVER_SOCKET, 'tcp:127.0.0.1:16222')
        assert.equal(android.env.ANDROID_ADB_SERVER_PORT, '16222')
        assert.equal(env.ADB_SERVER_SOCKET, 'tcp:elsewhere:9999')
        assert.deepEqual(android.args(['shell', 'true']), ['-H', '127.0.0.1', '-P', '16222', '-s', 'emulator-5690', 'shell', 'true'])
        android.assertAvd('portable-synthetic\r\nOK\r\n')
        assert.throws(() => android.assertAvd('another-profile\nOK'), /Unsafe Android AVD/)
        assert.throws(() => android.args(['shell', 'true'], 'emulator-5692'), /Unexpected Android target/)
    }
    const custom = androidTarget({ ...selection, adb: '/custom/adb', aapt: '/custom/aapt' }, {}, 'linux')
    assert.equal(custom.adb, '/custom/adb')
    assert.equal(custom.aapt, '/custom/aapt')
    assert.equal(androidTarget(selection, { ANDROID_SDK_ROOT: '/sdk' }, 'linux').adb, '/sdk/platform-tools/adb')
})

test('preserves equals and spaces in option values and accepts only named flags', () => {
    assert.deepEqual(runnerOptions(['--output=/tmp/a=b c.json', '--ui'], ['--ui']), { output: '/tmp/a=b c.json', ui: true })
    assert.throws(() => runnerOptions(['--serial']), /name=value/)
    assert.throws(() => runnerOptions(['--ui']), /name=value/)
})

test('Android entry points reject a different AVD before any mutation', async t => {
    const root = fileURLToPath(new URL('../../', import.meta.url))
    const parent = path.join(root, '.tmp', 'android-target-tests')
    mkdirSync(parent, { recursive: true })
    const directory = mkdtempSync(path.join(parent, 'run-'))
    const preload = path.join(directory, 'fake-adb.mjs')
    const log = path.join(directory, 'commands.jsonl')
    writeFileSync(preload, `
import childProcess from 'node:child_process'
import { appendFileSync } from 'node:fs'
import { syncBuiltinESMExports } from 'node:module'
import { promisify } from 'node:util'
function execute(executable, args, options) {
    appendFileSync(process.env.RISUNEST_ANDROID_TARGET_TEST_LOG, JSON.stringify({ executable, args,
        socket: options.env.ADB_SERVER_SOCKET, port: options.env.ANDROID_ADB_SERVER_PORT }) + '\\n')
    if (args.slice(-3).join(' ') !== 'emu avd name') throw new Error('Unexpected device command')
    return 'another-profile\\nOK\\n'
}
childProcess.execFileSync = execute
childProcess.spawnSync = (...args) => ({ status: 0, stdout: execute(...args), stderr: '' })
const asynchronous = () => { throw new Error('Expected promisified execFile') }
asynchronous[promisify.custom] = async (...args) => ({ stdout: execute(...args), stderr: '' })
childProcess.execFile = asynchronous
syncBuiltinESMExports()
`)
    try {
        for (const runner of ['device-backup/android-smoke', 'startup/android-smoke', 'streaming/android-smoke',
            'plugin-review/android-smoke', 'sync-server/android-probe', 'sync-server/android-runtime', 'startup/cdp']) {
            await t.test(runner, () => {
                writeFileSync(log, '')
                const args = runner === 'startup/cdp'
                    ? ['--input-type=module', '-e', `
import { androidTarget } from './benchmarks/support/androidTarget.mjs';
import { connectSyntheticAndroid } from './benchmarks/startup/cdp.mjs';
await connectSyntheticAndroid(19366, androidTarget(${JSON.stringify({ ...selection, adb: 'fake-adb' })}));`]
                    : [`benchmarks/${runner}.mjs`, '--adb=fake-adb', ...Object.entries(selection).map(([key, value]) => `--${key}=${value}`),
                        '--apk=synthetic.apk', '--health=synthetic-health.json', `--output=${path.join(directory, 'result.json')}`]
                const result = spawnSync(process.execPath, ['--import', pathToFileURL(preload).href, ...args], {
                    cwd: root, encoding: 'utf8', timeout: 20000, windowsHide: true,
                    env: { ...process.env, ADB_SERVER_SOCKET: 'tcp:elsewhere:9999', ANDROID_ADB_SERVER_PORT: '9999', RISUNEST_ANDROID_TARGET_TEST_LOG: log },
                })
                assert.equal(result.status, 1, result.stderr)
                const commands = readFileSync(log, 'utf8').trim().split('\n').map(line => JSON.parse(line))
                assert.deepEqual(commands, [{ executable: 'fake-adb', args: ['-H', '127.0.0.1', '-P', '16222', '-s', 'emulator-5690', 'emu', 'avd', 'name'],
                    socket: 'tcp:127.0.0.1:16222', port: '16222' }], result.stderr)
            })
        }
    } finally {
        assert.equal(path.dirname(directory), parent)
        rmSync(directory, { recursive: true, force: true })
    }
})
