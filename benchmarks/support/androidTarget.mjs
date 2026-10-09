import assert from 'node:assert/strict'
import path from 'node:path'

export function runnerOptions(argv, flags = []) {
    return Object.fromEntries(argv.map(argument => {
        if (flags.includes(argument)) return [argument.slice(2), true]
        const equal = argument.indexOf('=')
        assert.ok(argument.startsWith('--') && equal > 2, 'Use --name=value arguments')
        return [argument.slice(2, equal), argument.slice(equal + 1)]
    }))
}

export function androidTarget(options, env = process.env, platform = process.platform) {
    const serial = options.serial
    const avd = options.avd
    assert.match(serial ?? '', /^emulator-\d+$/, '--serial must select a disposable emulator')
    assert.ok(typeof avd === 'string' && /^[A-Za-z0-9_.-]+$/.test(avd), '--avd must name the disposable synthetic AVD')
    assert.match(options['adb-port'] ?? '', /^\d+$/, 'Explicit --adb-port required')
    const port = Number(options['adb-port'])
    assert.ok(port >= 1024 && port <= 65535, '--adb-port must be between 1024 and 65535')
    const paths = platform === 'win32' ? path.win32 : path.posix
    const suffix = platform === 'win32' ? '.exe' : ''
    const sdk = env.ANDROID_HOME || env.ANDROID_SDK_ROOT
    assert.ok(options.adb || sdk, 'Set ANDROID_HOME or pass --adb=<executable>')
    const adb = options.adb || paths.join(sdk, 'platform-tools', `adb${suffix}`)
    const aapt = options.aapt || paths.resolve(paths.dirname(adb), '../build-tools/36.0.0', `aapt${suffix}`)
    return {
        adb, aapt, serial, avd,
        env: { ...env, ANDROID_ADB_SERVER_PORT: String(port), ADB_SERVER_SOCKET: `tcp:127.0.0.1:${port}` },
        args(command, target = serial) {
            assert.equal(target, serial, 'Unexpected Android target')
            return ['-H', '127.0.0.1', '-P', String(port), '-s', serial, ...command]
        },
        assertAvd(output) {
            assert.equal(output.trim().split(/\r?\n/)[0], avd, 'Unsafe Android AVD')
        },
    }
}
