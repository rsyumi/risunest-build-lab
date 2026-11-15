import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'
import { nativeClient, oracleClient } from './client.mjs'
import { cases, escapeProbes, unicodeCases, controlledCases, controlledBounds, rule, units } from './fixtures.mjs'

const [binary, receiptFile] = process.argv.slice(2)
if (!binary || !receiptFile) throw Error('Usage: node verify.mjs <native-binary> <receipt.json>')
const fixtures = cases()
const rawOnly = process.argv.includes('--raw-only')
const controlledOnly = process.argv.includes('--controlled-only')
assert.ok(!(rawOnly && controlledOnly), 'Select one verification lane')
const controlledFixtures = [...escapeProbes(), ...unicodeCases().filter(fixture => ['g', 'gu'].includes(fixture.rules[0].flags)), ...controlledCases()]
const controlBounds = controlledBounds()
const provenance = await readFile(new URL('../../crates/regress/UPSTREAM.toml', import.meta.url), 'utf8')
const forkFiles = [...provenance.matchAll(/^"([^"\r\n]+)" = "[a-f0-9]{64}"$/gm)].map(match => match[1])
assert.equal(forkFiles.length, 39, 'Pinned upstream file manifest')
const localFiles = /local_files = \[([^\]]*)\]/.exec(provenance)?.[1].match(/"[^"]+"/g)?.map(value => JSON.parse(value)) ?? []
assert.deepEqual(localFiles, ['src/control.rs'], 'Authored fork source manifest')
forkFiles.push(...localFiles, 'UPSTREAM.toml', 'Cargo.lock')
const forkHash = createHash('sha256')
for (const file of forkFiles.sort()) {
    const bytes = await readFile(new URL(`../../crates/regress/${file}`, import.meta.url))
    forkHash.update(`${file}\0${bytes.length}\0`).update(bytes)
}
const result = {
    schema: 1, completed: false, mode: controlledOnly ? 'controlledFirstMatch' : rawOnly ? 'rawEngine' : 'compatibility', route: controlledOnly ? 'nativeRustRegressControlledFirstMatch' : 'nativeRustRegress', productRouting: false,
    engine: { name: 'regress', version: '0.12.0', patch: 'risunest-controlled-matching-2', forkSha256: forkHash.digest('hex'), upstreamArchiveSha256: '32eef8b209c3c1c15dbad02c1f30f9539f00dc7253e0cbcdaae442a50a09d7c1', backend: 'classicalbacktrack', input: 'UTF-16/UCS-2' },
    oracle: { node: process.version, v8: process.versions.v8 },
    binarySha256: createHash('sha256').update(await readFile(binary)).digest('hex'),
    sourceSha256: Object.fromEntries(await Promise.all(['./verify.mjs', './fixtures.mjs', './client.mjs', './oracle.worker.mjs', './native/src/main.rs', './native/Cargo.toml', './native/Cargo.lock', '../../crates/regex-compat-experimental/src/lib.rs', '../../crates/regex-compat-experimental/src/js.rs', '../../crates/regex-compat-experimental/Cargo.toml', '../../crates/regex-compat-experimental/Cargo.lock'].map(async file => [file, createHash('sha256').update(await readFile(new URL(file, import.meta.url))).digest('hex')]))),
    fixtureSha256: createHash('sha256').update(JSON.stringify(fixtures)).digest('hex'),
    controlledInputSha256: createHash('sha256').update(JSON.stringify({ controlledFixtures, controlBounds })).digest('hex'),
    constructorControlled: false, controlledMatches: [], controlledConstructorRejections: 0, controlledBoundaries: [],
    compared: 0, nativeSuccess: 0, requiredNative: 0, unsupported: 0, engineRejected: 0,
    semanticMismatch: [], rawEngineProbes: [], lifecycleChecks: 0, boundaries: [], watchdog: [],
}
const oracle = await oracleClient(), native = nativeClient(binary)
const prepare = (client, rules, input, limits) => client.send({ op: 'prepare', rules, input, ...(limits ? { limits } : {}) })
const execute = (client, prepared, extra = {}, timeout) => client.send({ op: 'execute', generation: prepared.generation, ...extra }, timeout)
const lifecycle = (actual, expected) => { assert.deepEqual(actual, expected); result.lifecycleChecks++ }
try {
    for (const fixture of rawOnly || controlledOnly ? [] : fixtures) {
        const expected = await oracle.send(fixture)
        assert.notEqual(expected.status, 'externalTimeout', fixture.id)
        if (fixture.reject) assert.equal(expected.status, 'jsRejected', fixture.id)
        const prepared = await prepare(native, fixture.rules, fixture.input)
        assert.equal(prepared.route, 'nativeRustRegress', fixture.id)
        if (fixture.unsupported) {
            assert.equal(prepared.status, 'unsupported', fixture.id)
            assert.equal(prepared.code, fixture.unsupported, fixture.id)
            result.unsupported++
            continue
        }
        if (expected.status === 'jsRejected') {
            assert.equal(prepared.status, 'engineRejected', fixture.id)
            assert.equal(fixture.required, undefined, fixture.id)
            result.engineRejected++
            continue
        }
        assert.equal(prepared.status, 'prepared', fixture.id)
        const actual = await execute(native, prepared)
        assert.equal(actual.route, 'nativeRustRegress', fixture.id)
        assert.equal(actual.status, 'nativeSuccess', fixture.id)
        result.nativeSuccess++
        result.compared++
        if (fixture.required) result.requiredNative++
        if (JSON.stringify(actual.output) !== JSON.stringify(expected.output)) result.semanticMismatch.push(fixture.id)
    }
    const rawCases = controlledOnly ? [] : rawOnly ? [...escapeProbes(), ...unicodeCases().filter(fixture => ['g', 'gu'].includes(fixture.rules[0].flags))] : escapeProbes()
    for (const probe of rawCases) {
        const expected = await oracle.send({ ...probe, probe: true })
        const actual = await native.send({ op: 'probe', rule: probe.rules[0], input: probe.input })
        assert.equal(actual.route, 'nativeRustRegressRawProbe', probe.id)
        if (probe.reject) assert.equal(expected.status, 'jsRejected', probe.id)
        if (expected.status === 'jsRejected') {
            assert.equal(probe.required, undefined, probe.id)
            assert.equal(actual.status, 'engineRejected', probe.id)
            result.rawEngineProbes.push({ id: probe.id, status: 'nativeRejection' })
        } else {
            assert.equal(expected.status, 'probeSuccess', probe.id)
            assert.equal(actual.status, 'probeSuccess', probe.id)
            assert.deepEqual(actual.range, expected.range, probe.id)
            result.rawEngineProbes.push({ id: probe.id, status: 'nativeMatch', jsRange: expected.range, nativeRange: actual.range })
        }
    }
    if (!rawOnly && !controlledOnly) {
    const rules = [rule('(a)\\1', 'g', 'b'), rule('(?<=b)b', 'g', 'c')], input = units('aaaa')
    const prepared = await prepare(native, rules, input)
    const expected = await oracle.send({ rules, input })
    for (let repeat = 0; repeat < 3; repeat++) lifecycle((await execute(native, prepared)).output, expected.output)
    lifecycle((await execute(native, prepared, { cancelled: true })).status, 'cancelled')
    lifecycle((await execute(native, prepared, { deadline_ms: 0 })).status, 'deadlineExceeded')
    lifecycle((await execute(native, prepared)).output, expected.output)
    lifecycle((await prepare(native, [rule('a', 'v')], input)).status, 'unsupported')
    lifecycle((await execute(native, prepared)).output, expected.output)
    const replacement = await prepare(native, [rule('a', 'g', 'z')], input)
    lifecycle((await execute(native, prepared)).status, 'staleGeneration')
    lifecycle((await native.send({ op: 'release', generation: prepared.generation })).status, 'staleGeneration')
    lifecycle((await execute(native, replacement)).output, units('zzzz'))
    lifecycle((await native.send({ op: 'release', generation: replacement.generation })).status, 'released')
    lifecycle((await execute(native, replacement)).status, 'notPrepared')
    lifecycle(await native.send({ op: 'stats' }), { status: 'stats', plans: 0, rules: 0, sourceUnits: 0, inputUnits: 0, route: 'nativeRustRegress' })
    for (const [name, limits] of Object.entries({ rules: { rules: 0 }, pattern_units: { pattern_units: 0 }, source_units: { source_units: 1 }, input_units: { input_units: 1 }, output_units: { output_units: 3 }, matches: { matches: 1 } })) {
        let response = await prepare(native, [rule('a', 'g', 'XX')], units('aa'), limits)
        if (response.status === 'prepared') response = await execute(native, response)
        assert.equal(response.status, 'resourceLimit', name)
        assert.equal(response.code, name)
        assert.equal(response.output, undefined)
        result.boundaries.push(name)
    }
    for (const input of [[-1], [65536], ['a'], [0.5]]) {
        lifecycle((await prepare(native, [], input)).status, 'invalidRequest')
    }
    const fresh = await prepare(native, [rule('a', 'g', 'b')], units('a'))
    lifecycle((await execute(native, fresh)).output, units('b'))
    await native.send({ op: 'release', generation: fresh.generation })
    const adversarial = { rules: [rule('(a+)+$', 'g', 'X')], input: units('a'.repeat(64) + '!') }
    const js = await oracleClient(), child = nativeClient(binary)
    try {
        assert.equal((await child.send({ op: 'stats' })).status, 'stats')
        const plan = await prepare(child, adversarial.rules, adversarial.input)
        assert.equal(plan.status, 'prepared')
        const nativeOutcome = await execute(child, plan, { deadline_ms: 5 }, 300)
        result.watchdog.push({ route: 'nativeRustRegress', status: nativeOutcome.status, inProcessDeadlineMs: 5, externalWatchdogMs: 300, safeInProcessInterruption: false })
        const jsOutcome = await js.send(adversarial, 300)
        result.watchdog.push({ route: 'nodeV8Oracle', status: jsOutcome.status, externalWatchdogMs: 300 })
        assert.ok(['externalTimeout', 'deadlineExceeded'].includes(nativeOutcome.status), 'Adversarial workload did not exercise a deadline')
        assert.equal(nativeOutcome.output, undefined)
    } finally { await child.close(); await js.close() }
    assert.equal(result.semanticMismatch.length, 0, 'Admitted native output mismatches the JavaScript oracle')
    assert.equal(result.requiredNative, fixtures.filter(value => value.required).length, 'Required native corpus coverage')
    }
    if (controlledOnly) {
        for (const fixture of controlledFixtures) {
            const expected = await oracle.send({ ...fixture, controlledProbe: true })
            const actual = await native.send({ op: 'controlledProbe', rule: fixture.rules[0], input: fixture.input, start: fixture.start ?? 0 })
            assert.equal(actual.route, 'nativeRustRegressControlledFirstMatch', fixture.id)
            assert.equal(actual.constructorControlled, false, fixture.id)
            if (fixture.reject) assert.equal(expected.status, 'jsRejected', fixture.id)
            if (expected.status === 'jsRejected') {
                assert.equal(fixture.required, undefined, fixture.id)
                assert.equal(actual.status, 'engineRejected', fixture.id)
                result.controlledConstructorRejections++
                result.controlledMatches.push({ id: fixture.id, status: 'uncontrolledConstructorRejection' })
                continue
            }
            assert.equal(expected.status, 'controlledSuccess', fixture.id)
            assert.equal(actual.status, 'controlledSuccess', fixture.id)
            assert.deepEqual(actual.range, expected.range, fixture.id)
            assert.deepEqual(actual.captures, expected.captures, fixture.id)
            const named = Object.create(null)
            for (const [slot, name] of (actual.captureNames ?? []).entries()) {
                if (name !== null && (named[name] == null || actual.captures[slot] !== null)) named[name] = actual.captures[slot]
            }
            assert.deepEqual({ ...named }, expected.namedCaptures, fixture.id)
            assert.equal(actual.usage.activeLookaroundDepth, 0, fixture.id)
            assert.equal(actual.usage.observationOverflow, false, fixture.id)
            result.controlledMatches.push({ id: fixture.id, status: 'controlledMatch', range: actual.range,
                captures: actual.captures, captureNames: actual.captureNames, namedCaptures: expected.namedCaptures, usage: actual.usage })
        }
        for (const fixture of controlBounds) {
            const actual = await native.send({ op: 'controlledProbe', rule: fixture.rules[0], input: fixture.input,
                start: fixture.start ?? 0, ...(fixture.limits ? { limits: fixture.limits } : {}),
                ...(fixture.cancelled ? { cancelled: true } : {}), ...(fixture.deadline_ms === undefined ? {} : { deadline_ms: fixture.deadline_ms }) })
            assert.equal(actual.route, 'nativeRustRegressControlledFirstMatch', fixture.id)
            assert.equal(actual.constructorControlled, false, fixture.id)
            assert.equal(actual.status, 'matchAborted', fixture.id)
            assert.equal(actual.reason, fixture.abort, fixture.id)
            assert.equal(actual.range, undefined, fixture.id)
            assert.equal(actual.captures, undefined, fixture.id)
            assert.equal(actual.usage.activeLookaroundDepth, 0, fixture.id)
            result.controlledBoundaries.push({ id: fixture.id, reason: actual.reason, usage: actual.usage })
        }
        assert.equal(result.controlledMatches.length, controlledFixtures.length)
        assert.equal(result.controlledConstructorRejections, 30, 'Retained raw syntax classifications')
    }
    result.completed = true
} finally {
    await native.close(); await oracle.close()
    await writeFile(receiptFile, JSON.stringify(result, null, 2) + '\n')
}
console.log(JSON.stringify({ ...result, sourceSha256: undefined, rawEngineProbes: result.rawEngineProbes.length, controlledMatches: result.controlledMatches.length, controlledBoundaries: result.controlledBoundaries.length }))
