import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'
import vm from 'node:vm'
import ts from 'typescript'
import { loadConfigFromFile } from 'vite'
import { createManifest, defaultLedger, executionProfileProbe, instrumentSource, repositoryRoot } from './probe.mjs'
import { assertNoJsProcessing, installExecutionProfile } from './profile.mjs'

const fixtureModule = 'src/fixture.ts'
const fixtureSource = `
function sendChat(value: unknown) { return value }
async function work(gate: Promise<string>) { return await gate }
function fail(value: unknown) { throw value }
function strict() { 'use strict'; return this }
class Tokenizer { prefix = 'count:'; work(value: string) { return this.prefix + value } }
class Other { work(value: string) { return value } }
function* sequence() { yield 'first'; return 'last' }
function trigger(mode: string) { return mode }
function script(mode: string) { if (mode === 'editdisplay') trigger('display'); return mode }
globalThis.subject = { sendChat, work, fail, strict, Tokenizer, Other, sequence, script, trigger }
`
const rule = (id, symbol, extra = {}) => ({ id, module: fixtureModule, kind: 'function', symbol, class: 'must-be-native', ...extra })
const fixtureLedger = {
    schema: defaultLedger.schema, scenario: 'observer-contract',
    entries: [rule('send.entry', 'sendChat', { class: 'allowed-webview' }), rule('work', 'work'), rule('fail', 'fail'),
        rule('strict', 'strict'), rule('method', 'work', { kind: 'method', owner: 'Tokenizer' }), rule('generator', 'sequence'),
        { ...defaultLedger.entries.find(entry => entry.id === 'scripts.mode'), id: 'script', module: fixtureModule, symbol: 'script' },
        { ...defaultLedger.entries.find(entry => entry.id === 'triggers.mode'), id: 'trigger', module: fixtureModule, symbol: 'trigger' }],
}
const fixtureManifest = () => createManifest(fixtureLedger, { [fixtureModule]: fixtureSource })
function runtime({ bootstrap = true, identity } = {}) {
    const manifest = fixtureManifest()
    const context = vm.createContext({})
    if (bootstrap) vm.runInContext(`(${installExecutionProfile.toString()})(${JSON.stringify({ ...manifest, identity: identity ?? manifest.identity })})`, context)
    const transformed = instrumentSource(fixtureSource, fixtureModule, manifest)
    const output = ts.transpileModule(transformed.code, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText
    vm.runInContext(output, context)
    const observer = context.__risunestExecutionProfile
    if (bootstrap) observer.begin(identity ?? manifest.identity)
    return { manifest, observer, subject: context.subject }
}

// The ledger names production definitions, including the owning tokenizer class.
test('every first-route ledger target transforms actual product source with frozen fingerprints', () => {
    const modules = [...new Set(defaultLedger.entries.map(entry => entry.module))]
    const sources = Object.fromEntries(modules.map(module => [module, readFileSync(resolve(repositoryRoot, module), 'utf8')]))
    const manifest = createManifest(defaultLedger, sources)
    let entries = 0
    for (const module of modules) {
        assert.equal(manifest.sources[module], createHash('sha256').update(sources[module]).digest('hex'))
        const transformed = instrumentSource(sources[module], module, manifest)
        entries += [...transformed.code.matchAll(/__risunestExecutionProfile\.enter\(/g)].length
        const result = ts.transpileModule(transformed.code, { reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } })
        assert.deepEqual(result.diagnostics.filter(item => item.category === ts.DiagnosticCategory.Error), [])
    }
    assert.equal(entries, defaultLedger.entries.length)
    assert.notEqual(createManifest({ ...defaultLedger, scenario: 'another-scenario' }, sources).identity, manifest.identity)
    assert.equal(defaultLedger.entries.filter(entry => entry.kind === 'method').every(entry => entry.owner === 'ChatTokenizer'), true)
})

test('counts async entry before its suspended body returns and preserves its result', async () => {
    const { subject, observer, manifest } = runtime()
    let release
    const gate = new Promise(resolve => { release = resolve })
    assert.equal(subject.sendChat('submitted'), 'submitted')
    const pending = subject.work(gate)
    assert.equal(observer.snapshot(manifest.identity).counts.work['must-be-native'], 1)
    assert.throws(() => assertNoJsProcessing(observer.snapshot(manifest.identity), manifest), /js-processing:work/)
    release('finished')
    assert.equal(await pending, 'finished')
})

test('preserves directive prologues, throw identity, method receivers and generator timing', () => {
    const { subject, observer, manifest } = runtime()
    assert.equal(subject.strict.call(undefined), undefined)
    const sentinel = {}
    assert.throws(() => subject.fail(sentinel), error => error === sentinel)
    assert.equal(new subject.Other().work('plain'), 'plain')
    assert.equal(observer.snapshot(manifest.identity).counts.method['must-be-native'], 0)
    assert.equal(new subject.Tokenizer().work('value'), 'count:value')
    assert.equal(observer.snapshot(manifest.identity).counts.method['must-be-native'], 1)
    const sequence = subject.sequence()
    assert.equal(observer.snapshot(manifest.identity).counts.generator['must-be-native'], 0)
    assert.equal(sequence.next().value, 'first')
    assert.equal(observer.snapshot(manifest.identity).counts.generator['must-be-native'], 1)
    assert.equal(sequence.next().value, 'last')
})

test('allows input and display work while recording processing modes as JS engine execution', () => {
    const { subject, observer, manifest } = runtime()
    subject.sendChat(true)
    assert.equal(subject.script('editinput'), 'editinput')
    assert.equal(subject.script('editdisplay'), 'editdisplay')
    assert.equal(subject.trigger('input'), 'input')
    const allowed = observer.end(manifest.identity)
    assert.equal(allowed.counts.script['allowed-webview'], 2)
    assert.equal(allowed.counts.trigger['allowed-webview'], 2)
    assert.equal(assertNoJsProcessing(allowed, manifest), allowed)
    observer.begin(manifest.identity)
    subject.sendChat(true)
    subject.script('editprocess')
    subject.script('editoutput')
    assert.equal(observer.snapshot(manifest.identity).counts.script['must-be-native'], 2)
    assert.throws(() => assertNoJsProcessing(observer.end(manifest.identity), manifest), /js-processing:script/)
    observer.begin(manifest.identity)
    subject.sendChat(true)
    subject.trigger('start')
    subject.trigger('output')
    assert.equal(observer.snapshot(manifest.identity).counts.trigger['must-be-native'], 2)
    assert.throws(() => assertNoJsProcessing(observer.end(manifest.identity), manifest), /js-processing:trigger/)
})

test('missing observers and mismatched ledger identities fail at an instrumented entry', () => {
    assert.throws(() => runtime({ bootstrap: false }).subject.sendChat(true))
    assert.throws(() => runtime({ identity: 'wrong-ledger' }).subject.sendChat(true), /identity-mismatch/)
    assert.throws(() => installExecutionProfile({}, {}), /manifest-missing/)
    const { observer, manifest } = runtime()
    assert.throws(() => observer.snapshot('wrong-observer'), /identity-mismatch/)
    assert.throws(() => observer.enter(manifest.identity, 'unlisted'), /unknown-entry/)
})

test('rejects unstarted, incomplete, foreign and invalid counter observations', () => {
    const manifest = fixtureManifest()
    const observer = installExecutionProfile(manifest, {})
    assert.throws(() => observer.snapshot(manifest.identity), /not-started/)
    observer.begin(manifest.identity)
    assert.throws(() => assertNoJsProcessing(observer.snapshot(manifest.identity), manifest), /send-not-observed/)
    observer.enter(manifest.identity, 'send.entry')
    const snapshot = observer.end(manifest.identity)
    for (const corrupt of [
        value => { delete value.identity }, value => { delete value.ledgerSha256 },
        value => { value.sources[fixtureModule] = 'changed' }, value => { delete value.instrumentation }, value => { delete value.counts.work },
        value => { value.counts.work['must-be-native'] = -1 }, value => { value.counts.work['web-only'] = 1 },
    ]) {
        const copy = structuredClone(snapshot)
        corrupt(copy)
        assert.throws(() => assertNoJsProcessing(copy, manifest))
    }
    assert.equal(assertNoJsProcessing(snapshot, manifest), snapshot)
})

test('refuses missing, duplicate and wrongly scoped target definitions', () => {
    const source = fixtureSource.replace('function sendChat', 'function renamedSend')
    assert.throws(() => instrumentSource(source, fixtureModule, createManifest(fixtureLedger, { [fixtureModule]: source })), /target-count:send.entry:0/)
    const duplicate = fixtureSource + '\nfunction sendChat() {}'
    assert.throws(() => instrumentSource(duplicate, fixtureModule, createManifest(fixtureLedger, { [fixtureModule]: duplicate })), /target-count:send.entry:2/)
    const wrongOwner = { ...fixtureLedger, entries: [rule('method', 'work', { kind: 'method', owner: 'Missing' })] }
    assert.throws(() => instrumentSource(fixtureSource, fixtureModule, createManifest(wrongOwner, { [fixtureModule]: fixtureSource })), /target-count:method:0/)
    const wrongMode = { ...fixtureLedger, entries: [rule('mode', 'script', { modeParameter: 'absent', allowedModes: [] })] }
    assert.throws(() => instrumentSource(fixtureSource, fixtureModule, createManifest(wrongMode, { [fixtureModule]: fixtureSource })), /mode-parameter/)
    assert.throws(() => createManifest({ ...fixtureLedger, entries: [fixtureLedger.entries[0], fixtureLedger.entries[0]] }, { [fixtureModule]: fixtureSource }), /invalid-entry/)
})

test('build plugin requires every target module and emits the matching source manifest', () => {
    const probe = executionProfileProbe()
    const assets = []
    const output = { emitFile: asset => assets.push(asset) }
    probe.buildStart()
    assert.throws(() => probe.generateBundle.call(output), /module-not-instrumented/)
    for (const module of new Set(defaultLedger.entries.map(entry => entry.module))) {
        const source = readFileSync(resolve(repositoryRoot, module), 'utf8')
        assert.throws(() => probe.transform(source + '\n', resolve(repositoryRoot, module)), /source-changed/)
        assert.ok(probe.transform(source, resolve(repositoryRoot, module) + '?verification'))
    }
    probe.generateBundle.call(output)
    const manifest = JSON.parse(assets[0].source)
    const context = vm.createContext({})
    const [bootstrap] = probe.transformIndexHtml.handler()
    vm.runInContext(bootstrap.children, context)
    assert.equal(context.__risunestExecutionProfile.identity, manifest.identity)
    assert.equal(assets[0].fileName, 'execution-profile-manifest.json')
})

test('verification config keeps agent protections and uses a separate full-UI build directory', async () => {
    const path = resolve(repositoryRoot, 'tests/native/execution-profile/vite.config.ts')
    const loaded = await loadConfigFromFile({ command: 'build', mode: 'agent' }, path, repositoryRoot)
    assert.equal(resolve(loaded.config.root), resolve(repositoryRoot))
    assert.equal(resolve(loaded.config.build.outDir), resolve(repositoryRoot, '.tmp/test-results/execution-profile/dist'))
    const names = loaded.config.plugins.flat(Infinity).filter(Boolean).map(plugin => plugin.name)
    assert.ok(names.includes('risunest-block-realm-endpoints'))
    assert.ok(names.includes('risunest-execution-profile'))
    assert.match(readFileSync(resolve(loaded.config.root, 'index.html'), 'utf8'), /src\/main\.ts/)
    await assert.rejects(loadConfigFromFile({ command: 'build', mode: 'production' }, path, repositoryRoot), /agent-build-required/)
})
