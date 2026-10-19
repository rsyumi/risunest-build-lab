import { spawn, spawnSync } from 'node:child_process'
import { mkdirSync, existsSync, readFileSync, writeFileSync, rmSync, lstatSync } from 'node:fs'
import { resolve, join, dirname, basename, isAbsolute } from 'node:path'
import { randomUUID, createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import { verifyReport } from './report.mjs'

const root = resolve(import.meta.dirname, '../..')
const smallAssets = process.argv.includes('--asset-benchmark-small')
const assetBenchmark = smallAssets || process.argv.includes('--asset-benchmark')
const debug = process.argv.includes('--debug')
const reuseBuild = process.argv.includes('--reuse-build')
const binaryInput = process.argv.find(arg => arg.startsWith('--binary='))?.slice(9)
if (binaryInput && (!assetBenchmark || !reuseBuild)) throw new Error('An explicit benchmark binary requires --reuse-build')
const repetitions = Number(process.argv.find(arg => arg.startsWith('--runs='))?.slice(7) ?? 1)
if ((!assetBenchmark && (reuseBuild || repetitions !== 1)) || !Number.isSafeInteger(repetitions) || repetitions < 1 || repetitions > 10) {
    throw new Error('Build reuse and repeated runs require an asset benchmark with 1 to 10 runs')
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
const phaseGroups = assetBenchmark ? [[smallAssets ? 'asset-benchmark-small' : 'asset-benchmark']] : [['write', 'read'], ['abort', 'read-abort'], ['contract', 'read-contract']]
// The repeated-close escape exists where closing the window quits; SIGTERM asks only Linux to save.
if (!assetBenchmark && (process.platform === 'win32' || process.platform === 'linux')) phaseGroups.push(['close-escape'])
if (!assetBenchmark && process.platform === 'linux') phaseGroups.push(['session-end', 'read-session-end'], ['session-end-unanswered'])
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
            const status = await new Promise((accept, reject) => {
                const child = spawn(executable, [], { cwd: root, windowsHide: true, stdio: 'inherit', detached: process.platform !== 'win32', env: {
                    ...env, RISUNEST_BOUNDARY_ID: runId, RISUNEST_BOUNDARY_ROOT: ownedRoot,
                    RISUNEST_BOUNDARY_PHASE: phase, RISUNEST_BOUNDARY_REPORT: reportPath,
                } })
                let timedOut = false
                const terminate = phase.startsWith('session-end') ? setInterval(() => {
                    if (!existsSync(`${reportPath}.ready`)) return
                    clearInterval(terminate)
                    child.kill('SIGTERM')
                }, 50) : undefined
                const timeout = setTimeout(() => {
                    timedOut = true
                    if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true })
                    else process.kill(-child.pid, 'SIGKILL')
                }, assetBenchmark ? 300_000 : 90_000)
                child.on('error', error => { clearTimeout(timeout); clearInterval(terminate); reject(error) })
                child.on('close', code => {
                    clearTimeout(timeout)
                    clearInterval(terminate)
                    timedOut ? reject(new Error(`Native ${phase} timed out`)) : accept(code)
                })
            })
            const report = JSON.parse(readFileSync(reportPath, 'utf8'))
            verifyReport(report, runId, phase, status)
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
