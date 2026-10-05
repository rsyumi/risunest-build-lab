import { spawn, spawnSync } from 'node:child_process'
import { createWriteStream } from 'node:fs'
import { mkdir, writeFile } from 'node:fs/promises'
import { availableParallelism, cpus, freemem, loadavg, platform, totalmem } from 'node:os'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
if (args.includes('--help')) {
    console.log('Usage: node scripts/captureVitestRun.mjs [ordinary vitest run arguments]')
    console.log('Runs installed Vitest once, preserving scheduling and assertions. Set RISUNEST_TEST_WORKLOAD to describe concurrent validation work.')
    console.log('Unique output: .tmp/test-results/aggregate-*/context.json, runner.log, cases.json, phases.jsonl')
} else {
    if (args.some(arg => /^(?:--reporter|--outputFile|--watch|--ui)(?:=|$)/.test(arg))) {
        throw new Error('Capture owns reporters/output and supports one non-watch run')
    }
    const output = resolve(root, '.tmp/test-results', `aggregate-${new Date().toISOString().replace(/[:.]/g, '-')}-${process.pid}`)
    await mkdir(output, { recursive: true })
    const git = (...command) => {
        const result = spawnSync('git', ['-c', `safe.directory=${root.replaceAll('\\', '/')}`, ...command], { cwd: root, encoding: 'utf8', windowsHide: true })
        return result.status === 0 ? result.stdout.trim() : null
    }
    const system = () => ({
        at: new Date().toISOString(), freeMemoryBytes: freemem(), totalMemoryBytes: totalmem(),
        availableParallelism: availableParallelism(), cpuCount: cpus().length,
        loadAverage: platform() === 'win32' ? null : loadavg(),
    })
    const runnerArgs = [
        resolve(root, 'node_modules/vitest/vitest.mjs'), 'run', ...args,
        '--reporter=default', '--reporter=json', `--outputFile.json=${resolve(output, 'cases.json')}`,
        `--reporter=${resolve(root, 'tests/support/loadFailureReporter.mjs')}`,
    ]
    const context = {
        command: [process.execPath, ...runnerArgs], cwd: root,
        revision: git('rev-parse', 'HEAD'),
        dirtyStatus: git('status', '--porcelain'),
        nodeVersion: process.version, platform: platform(),
        concurrentWorkload: process.env.RISUNEST_TEST_WORKLOAD ?? 'not recorded',
        start: system(), end: null, exitCode: null, signal: null, runnerErrorCode: null,
    }
    const contextPath = resolve(output, 'context.json')
    await writeFile(contextPath, JSON.stringify(context, null, 2) + '\n')
    console.log(`Synthetic test capture: ${output}`)
    const log = createWriteStream(resolve(output, 'runner.log'), { flags: 'wx' })
    const child = spawn(process.execPath, runnerArgs, {
        cwd: root, windowsHide: true, stdio: ['inherit', 'pipe', 'pipe'],
        env: { ...process.env, RISUNEST_TEST_PHASE_OUTPUT: resolve(output, 'phases.jsonl') },
    })
    child.stdout.pipe(log, { end: false })
    child.stderr.pipe(log, { end: false })
    child.stdout.pipe(process.stdout)
    child.stderr.pipe(process.stderr)
    child.on('error', error => { context.runnerErrorCode = error.code ?? 'spawn-error' })
    const [code, signal] = await new Promise(resolveClose => child.once('close', (...values) => resolveClose(values)))
    await new Promise((resolveLog, rejectLog) => {
        log.on('error', rejectLog)
        log.end(resolveLog)
    })
    context.end = system()
    context.exitCode = code
    context.signal = signal
    await writeFile(contextPath, JSON.stringify(context, null, 2) + '\n')
    process.exitCode = code ?? 1
}
