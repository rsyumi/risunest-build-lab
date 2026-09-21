import { spawn, spawnSync } from 'node:child_process'
import { mkdirSync, existsSync, readFileSync, writeFileSync, rmSync, lstatSync } from 'node:fs'
import { resolve, join, dirname } from 'node:path'
import { randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'
import { verifyReport } from './report.mjs'

const root = resolve(import.meta.dirname, '../..')
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
await command(process.execPath, [join(dirname(require.resolve('vite/package.json')), 'bin/vite.js'), 'build', '--mode', 'agent', '--config', 'tests/native/vite.config.ts'])
await command('cargo', ['build', '--release', '--locked', '--manifest-path', 'tests/native/native/Cargo.toml'], env)
for (const phases of [['write', 'read'], ['abort', 'read-abort']]) {
    const runId = randomUUID().replaceAll('-', '')
    const identifier = `io.github.rsyumi.risunest.boundary.${runId}`
    const dataHome = process.platform === 'win32' ? process.env.APPDATA
        : process.platform === 'darwin' ? join(process.env.HOME, 'Library/Application Support')
        : process.platform === 'linux' ? (process.env.XDG_DATA_HOME || join(process.env.HOME, '.local/share')) : null
    if (!dataHome) throw new Error('Desktop host required')
    const ownedRoot = join(dataHome, identifier)
    if (existsSync(ownedRoot)) throw new Error('Run identity already exists')
    mkdirSync(ownedRoot)
    writeFileSync(join(ownedRoot, 'boundary-owner'), runId, { flag: 'wx' })
    const output = join(root, '.tmp/test-results/native', runId)
    mkdirSync(output, { recursive: true })
    const executable = join(target, 'release', `risunest-persistence-boundary${process.platform === 'win32' ? '.exe' : ''}`)
    try {
        for (const phase of phases) {
            const reportPath = join(output, `${phase}.json`)
            const status = await new Promise((accept, reject) => {
                const child = spawn(executable, [], { cwd: root, windowsHide: true, stdio: 'inherit', detached: process.platform !== 'win32', env: {
                    ...env, RISUNEST_BOUNDARY_ID: runId, RISUNEST_BOUNDARY_ROOT: ownedRoot,
                    RISUNEST_BOUNDARY_PHASE: phase, RISUNEST_BOUNDARY_REPORT: reportPath,
                } })
                let timedOut = false
                const timeout = setTimeout(() => {
                    timedOut = true
                    if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true })
                    else process.kill(-child.pid, 'SIGKILL')
                }, 90_000)
                child.on('error', error => { clearTimeout(timeout); reject(error) })
                child.on('exit', code => {
                    clearTimeout(timeout)
                    timedOut ? reject(new Error(`Native ${phase} timed out`)) : accept(code)
                })
            })
            verifyReport(JSON.parse(readFileSync(reportPath, 'utf8')), runId, phase, status)
            console.log(`Native persistence ${phase}: passed (${process.platform})`)
        }
    } finally {
        if (dirname(ownedRoot) !== resolve(dataHome) || !ownedRoot.endsWith(identifier)
            || lstatSync(ownedRoot).isSymbolicLink()
            || readFileSync(join(ownedRoot, 'boundary-owner'), 'utf8') !== runId) throw new Error('Owned cleanup validation failed')
        rmSync(ownedRoot, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 })
    }
}
