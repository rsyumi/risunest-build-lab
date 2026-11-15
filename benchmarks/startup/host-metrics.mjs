import { spawn, execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { once } from 'node:events'
import { createInterface } from 'node:readline'
import { stat } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { assertSyntheticProfile } from './fixture.mjs'

const execute = promisify(execFile)

export async function observeWal(root, identifier, profile) {
    assertSyntheticProfile(root, identifier, profile)
    const file = path.join(root, 'persistent/persistent.sqlite-wal')
    const read = async () => {
        try {
            return (await stat(file)).size
        } catch (error) {
            if (error.code === 'ENOENT') return 0
            throw error
        }
    }
    const startBytes = await read()
    let peakBytes = startBytes
    let samples = 1
    let failure
    let result
    let pending = Promise.resolve()
    const timer = setInterval(() => {
        pending = pending.then(async () => {
            try {
                peakBytes = Math.max(peakBytes, await read())
                samples++
            } catch (error) {
                failure = error
            }
        })
    }, 100)
    return async () => {
        if (result) return result
        clearInterval(timer)
        await pending
        if (failure) throw failure
        const endBytes = await read()
        result = {
            startBytes,
            endBytes,
            peakBytes: Math.max(peakBytes, endBytes),
            samples: samples + 1,
        }
        return result
    }
}

const sampledFields = ['nativeWorkingSetBytes', 'nativePrivateBytes', 'treeWorkingSetBytes', 'treePrivateBytes', 'processCount']

export async function readWindowsMemory(pid) {
    if (!Number.isSafeInteger(pid) || pid <= 0) throw new Error('Invalid benchmark PID')
    const { stdout } = await execute('powershell.exe', ['-NoProfile', '-NonInteractive', '-File',
        fileURLToPath(new URL('./host-memory.ps1', import.meta.url)), '-RootProcessId', String(pid)],
        { windowsHide: true, timeout: 30_000 })
    const raw = JSON.parse(stdout)
    if (raw.complete !== true) throw new Error('Incomplete owned process memory sample')
    const result = {}
    for (const key of [...sampledFields, 'nativePeakWorkingSetBytes']) {
        if (!Number.isSafeInteger(raw[key]) || raw[key] < 0) throw new Error('Invalid memory sample')
        result[key] = raw[key]
    }
    return result
}

function validateSample(sample) {
    const clocks = ['requestAtMs', 'atMs', 'nativeStartMs', 'snapshotStartMs', 'snapshotEndMs', 'nativeEndMs']
    if (clocks.some(key => !Number.isFinite(sample[key]) || sample[key] < 0)
        || sample.atMs < sample.requestAtMs || sample.snapshotStartMs < sample.nativeStartMs
        || sample.snapshotEndMs < sample.snapshotStartMs || sample.nativeEndMs < sample.snapshotEndMs
        || !Number.isFinite(sample.enumerationMs) || sample.enumerationMs < 0
        || sample.enumerationMs > sample.nativeEndMs - sample.nativeStartMs
        || !Array.isArray(sample.errors) || sample.errors.some(error => !/^[a-z-]{1,40}$/.test(error.code)
            || !Number.isSafeInteger(error.win32Error) || error.win32Error < 0)
        || ['knownCount', 'queriedCount', 'exitedCount', 'identityRefusedCount', 'snapshotCount'].some(key => !Number.isSafeInteger(sample[key]) || sample[key] < 0)) {
        throw new Error('Invalid owned process sample diagnostics')
    }
    if (sample.complete === true) {
        if (sample.errors.length || sampledFields.some(key => !Number.isSafeInteger(sample[key]) || sample[key] < 0)
            || sample.processCount < 1 || sample.knownCount !== sample.queriedCount) throw new Error('Invalid complete memory sample')
    } else if (sample.complete !== false || !sample.errors.length || sampledFields.some(key => sample[key] !== null)) {
        throw new Error('Incomplete memory sample must not publish partial counters')
    }
}

export function summarizeMemorySamples(samples, intervalMs, lifecycle = {}) {
    if (!samples.length && !lifecycle.terminalFailure) throw new Error('No owned process memory samples')
    for (const sample of samples) validateSample(sample)
    const responseGaps = samples.slice(1).map((sample, index) => sample.atMs - samples[index].atMs)
    const nativeGaps = samples.slice(1).map((sample, index) => sample.nativeStartMs - samples[index].nativeStartMs)
    if ([...responseGaps, ...nativeGaps].some(gap => gap < 0)) throw new Error('Memory sample clock moved backwards')
    const complete = samples.length > 0 && samples.every(sample => sample.complete) && !lifecycle.terminalFailure
    const valid = samples.filter(sample => sample.complete)
    const observedMax = valid.length
        ? Object.fromEntries(sampledFields.map(key => [key, Math.max(...valid.map(sample => sample[key]))])) : null
    return {
        intervalMs, samples: samples.length, completeSamples: samples.filter(sample => sample.complete).length,
        coverageComplete: complete, ...lifecycle,
        first: samples[0] ?? null, settled: samples.at(-1) ?? null,
        firstObservationBoundsMs: samples.length ? { earliest: samples[0].requestAtMs, latest: samples[0].atMs } : null,
        sampledPeak: complete ? observedMax : null,
        observedCompleteSweepMax: observedMax,
        maxGapMs: responseGaps.length ? Math.max(...responseGaps) : null,
        maxNativeStartGapMs: nativeGaps.length ? Math.max(...nativeGaps) : null,
        maxSweepMs: samples.length ? Math.max(...samples.map(sample => sample.nativeEndMs - sample.nativeStartMs)) : null,
        maxEnumerationMs: samples.length ? Math.max(...samples.map(sample => sample.enumerationMs)) : null,
        observedMs: samples.length ? samples.at(-1).atMs - samples[0].atMs : null,
        series: samples,
        limitations: [
            'Preparation precedes a launch, but spawn-to-bind and first-sweep delays remain; sampled maxima are not the true startup peak.',
            'Node request/response bounds and native QPC intervals have distinct origins; UTC process creation values are used for identity only.',
            'Each tree total sums current counters within one reported sweep window, not an atomic multi-process snapshot or a sum of process lifetime peaks.',
            'Short-lived undiscovered children, shared/detached external processes and total GPU allocation are not fully covered.',
            'Any incomplete sweep invalidates sampledPeak for the whole observation; complete sweep values and their observed maximum remain lower-bound observations.',
            'Failed counters are never replaced by zero or cached values.',
            'Working-set totals may double-count shared pages; private bytes are committed virtual memory, not unique physical footprint.',
            'Settled is the final observation after the runner condition, not proof of allocator reclamation or resource release.',
        ],
    }
}

export async function prepareWindowsMemory({ intervalMs = 100, spawnProcess = spawn, now = () => performance.now(), timeoutMs = 30_000 } = {}) {
    if (!Number.isSafeInteger(intervalMs) || intervalMs < 10) throw new Error('Invalid memory observer interval')
    const prepareStartedAt = now()
    const child = spawnProcess('powershell.exe', ['-NoProfile', '-NonInteractive', '-File',
        fileURLToPath(new URL('./host-memory.ps1', import.meta.url)), '-Stream'],
        { windowsHide: true, stdio: ['pipe', 'pipe', 'ignore'] })
    const exited = once(child, 'exit')
    exited.catch(() => {})
    let readyResolve, readyReject
    const ready = new Promise((resolve, reject) => { readyResolve = resolve; readyReject = reject })
    let readyAt, nativeFrequency, pending, failure, failureCode, failurePhase, stopping = false, closing, bound = false, timer, busy
    const fail = (code = 'sampler-failed') => {
        failureCode ??= code
        failurePhase ??= pending?.kind ?? (readyAt === undefined ? 'preparation' : 'process')
        failure ??= new Error('Owned process memory sampler failed')
        clearInterval(timer)
        readyReject(failure)
        pending?.reject(failure)
        pending = undefined
        queueMicrotask(() => { void close().catch(() => {}) })
    }
    child.on('error', () => fail('helper-error'))
    child.stdin.on('error', () => fail('write-failed'))
    child.on('exit', () => { if (!stopping) fail('helper-exited') })
    const lines = createInterface({ input: child.stdout })
    lines.on('line', line => {
        try {
            const raw = JSON.parse(line)
            if (raw.kind === 'failure') { fail('helper-reported-failure'); return }
            if (raw.kind === 'ready') {
                if (readyAt !== undefined || raw.protocol !== 2 || !Number.isSafeInteger(raw.nativeFrequency) || raw.nativeFrequency <= 0) throw new Error()
                readyAt = now()
                nativeFrequency = raw.nativeFrequency
                readyResolve()
            } else {
                if (!pending || raw.kind !== pending.kind) throw new Error()
                const waiting = pending
                pending = undefined
                waiting.resolve({ raw, receivedAt: now() })
            }
        } catch { fail('protocol-invalid') }
    })
    const send = (text, kind) => new Promise((resolve, reject) => {
        if (failure || stopping || pending) { reject(failure ?? new Error('Invalid sampler protocol state')); return }
        const sentAt = now()
        const deadline = setTimeout(() => fail('response-timeout'), timeoutMs)
        pending = {
            kind, resolve: value => { clearTimeout(deadline); resolve({ ...value, sentAt }) },
            reject: error => { clearTimeout(deadline); reject(error) },
        }
        child.stdin.write(text + '\n')
    })
    const close = () => {
        closing ??= (async () => {
            stopping = true
            clearInterval(timer)
            child.stdin.end('stop\n')
            let deadline
            try {
                const [exitCode] = await Promise.race([exited, new Promise((resolve, reject) => {
                    deadline = setTimeout(() => { child.kill(); reject(new Error('Owned memory sampler did not stop')) }, timeoutMs)
                })])
                if (exitCode !== 0) throw new Error('Owned memory sampler exited unsuccessfully')
            } finally { clearTimeout(deadline); lines.close() }
        })()
        return closing
    }
    const readyDeadline = setTimeout(() => fail('ready-timeout'), timeoutMs)
    try { await ready }
    catch (error) { await close().catch(() => {}); throw error }
    finally { clearTimeout(readyDeadline) }

    return {
        dispose: close,
        preparationMs: readyAt - prepareStartedAt,
        async bind(pid, { launchAtMs = now(), anchor = 'existing-process' } = {}) {
            if (bound || !Number.isSafeInteger(pid) || pid <= 0 || pid > 0xffffffff || !Number.isFinite(launchAtMs)) throw new Error('Invalid owned root binding')
            if (anchor === 'launch' && readyAt > launchAtMs) throw new Error('Memory helper was not ready before launch')
            bound = true
            const samples = []
            let binding, terminalFailure, cleanupFailure
            const terminal = (code, phase) => { terminalFailure ??= { code, phase } }
            const poll = () => {
                if (busy) return busy
                busy = (async () => {
                    const reply = await send('sample', 'sample')
                    const raw = reply.raw
                    const sample = {
                        requestAtMs: reply.sentAt - launchAtMs, atMs: reply.receivedAt - launchAtMs,
                        nativeStartMs: raw.nativeStartMs, snapshotStartMs: raw.snapshotStartMs,
                        snapshotEndMs: raw.snapshotEndMs, nativeEndMs: raw.nativeEndMs,
                        enumerationMs: raw.enumerationMs, snapshotCount: raw.snapshotCount,
                        complete: raw.complete, errors: raw.errors,
                    }
                    for (const key of [...sampledFields, 'knownCount', 'queriedCount', 'exitedCount', 'identityRefusedCount']) sample[key] = raw[key]
                    validateSample(sample)
                    if (samples.length >= 10_000) { fail('retention-limit'); throw failure }
                    samples.push(sample)
                })().finally(() => { busy = undefined })
                return busy
            }
            try {
                binding = await send('bind ' + pid + ' ' + process.pid, 'bound')
                await poll()
            } catch {
                terminal(failureCode ?? 'bind-failed', failurePhase ?? (binding ? 'sample' : 'bound'))
                try { await close() }
                catch { cleanupFailure = { code: 'shutdown-failed', phase: 'shutdown' } }
            }
            if (!terminalFailure) timer = setInterval(() => { void poll().catch(() => fail('sample-invalid')) }, intervalMs)
            let finalizing
            return () => {
                finalizing ??= (async () => {
                    clearInterval(timer)
                    if (!failure && !terminalFailure) {
                        try { if (busy) await busy; await poll() }
                        catch { terminal(failureCode ?? 'sample-invalid', failurePhase ?? 'sample') }
                    }
                    if (failure) terminal(failureCode, failurePhase)
                    try { await close() }
                    catch { cleanupFailure = { code: 'shutdown-failed', phase: 'shutdown' }; terminal(cleanupFailure.code, cleanupFailure.phase) }
                    if (failure) terminal(failureCode, failurePhase)
                    return summarizeMemorySamples(samples, intervalMs, {
                        anchor, preparationMs: readyAt - prepareStartedAt,
                        preparedBeforeLaunch: anchor === 'launch' && readyAt <= launchAtMs,
                        readyBeforeAnchorMs: launchAtMs - readyAt,
                        bindRequestMs: binding ? binding.sentAt - launchAtMs : null,
                        bindRoundTripMs: binding ? binding.receivedAt - binding.sentAt : null,
                        nativeFrequency, terminalFailure: terminalFailure ?? null, cleanupFailure: cleanupFailure ?? null,
                    })
                })()
                return finalizing
            }
        },
    }
}

export async function observeWindowsMemory(pid, options = {}) {
    if (!Number.isSafeInteger(pid) || pid <= 0 || pid > 0xffffffff) throw new Error('Invalid benchmark PID')
    const prepared = await prepareWindowsMemory(options)
    try { return await prepared.bind(pid) }
    catch (error) { await prepared.dispose().catch(() => {}); throw error }
}
