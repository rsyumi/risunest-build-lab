import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, mkdtempSync, writeFileSync, unlinkSync, rmdirSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createHash } from 'node:crypto'
import { verifyRecoveryWitness, verifyRestart, verifyTerminalWitness, cancelHeldBody, verifyWireFailure, verifyWireProof, UiDriver, startScenario, verifyJsControl, verifyJsRestart, verifyUnsupportedReservation, verifyUnheldCompletion, qualifyLoreOracle, verifyLoreRequest, verifyLoreState, verifyLoreReceipts, verifyLoreModuleRows,
    qualifyTriggerOracle, verifyTriggerState, verifyTriggerReceipts, verifyTriggerRows, verifyJsStartTrigger, verifyTriggerStopControl, verifyTriggerStopRows } from './driver.mjs'
import { installCommandWitness } from './commandWitness.mjs'
import { installExecutionProfile, assertNoJsProcessing } from '../execution-profile/profile.mjs'
import { startOllamaFixture } from '../ollamaFixture.mjs'

const ticket = { epoch: 'synthetic-epoch', sequence: '7' }
const otherTickets = [{ ...ticket, epoch: 'other-epoch' }, { ...ticket, sequence: '8' }]
const status = { ticket, phase: 'completed', output: 'committed' }
const proof = { ticket, at: 'completed', receipts: { output: { receiptRevision: 9 } } }
const event = (command, result, state = 'resolved') => ({ command, state, result })
const terminal = () => [event('native_generation_status', status), event('native_generation_proof', proof)]
const before = () => ({ events: [event('native_generation_host_init', null),
    event('native_generation_reserve_ollama', { ticket }),
    event('native_generation_execute', { ticket, phase: 'running', dispatch: 'issued' }), ...terminal()], blocked: 'terminal-proof', active: true })
const recovered = () => ({ events: [event('native_generation_active', [{ ticket, phase: 'running' }]), ...terminal()], blocked: null, active: true })

test('active recovery needs this ticket terminal status and proof from the recovered renderer', () => {
    assert.equal(verifyRecoveryWitness(before(), recovered(), ticket, 'completed'), 'active-ticket recovery')
    for (const command of ['native_generation_status', 'native_generation_proof']) {
        const missing = recovered()
        missing.events = missing.events.filter(value => value.command !== command)
        assert.throws(() => verifyRecoveryWitness(before(), missing, ticket, 'completed'), /this ticket terminal/,
            'The withheld pre-loss proof and status cannot replace recovered observations')
        for (const other of otherTickets) {
            const unrelated = recovered()
            const target = unrelated.events.find(value => value.command === command)
            target.result = { ...target.result, ticket: other }
            assert.throws(() => verifyRecoveryWitness(before(), unrelated, ticket, 'completed'), /this ticket terminal/)
        }
        const pending = recovered()
        pending.events.find(value => value.command === command).state = 'pending'
        assert.throws(() => verifyRecoveryWitness(before(), pending, ticket, 'completed'), /this ticket terminal/)
    }
})

test('recovery rejects an extra execute even when it fails or sends no second provider request', () => {
    for (const committed of [false, true]) {
        for (const state of ['pending', 'resolved', 'rejected']) {
            const after = committed ? { events: [event('native_generation_active', [])] } : recovered()
            after.events.push(event('native_generation_execute', state === 'resolved' ? status : null, state))
            assert.throws(() => verifyRecoveryWitness(before(), after, ticket, 'completed', { committed }), /Recovery never executes/)
        }
    }
    const duplicateBefore = before()
    duplicateBefore.events.push(event('native_generation_execute', status))
    assert.throws(() => verifyRecoveryWitness(duplicateBefore, recovered(), ticket, 'completed'), /Only the real product Send/)
})

test('committed snapshot initialization observes the ticket absent without inventing a new terminal proof', () => {
    const after = { events: [event('native_generation_active', [])] }
    const rows = [{ chatId: 'earlier-question', role: 'user', data: 'Earlier question', metadata: { time: 1 } },
        { chatId: 'earlier-answer', role: 'char', data: 'Earlier answer' },
        { chatId: 'reply', role: 'char', data: 'Committed reply', generationInfo: { model: 'fixture' } }]
    const options = { committed: true, beforeRows: rows, recoveredRows: structuredClone(rows) }
    assert.equal(verifyRecoveryWitness(before(), after, ticket, 'completed', options), 'committed snapshot initialization')
    assert.throws(() => verifyRecoveryWitness(before(), after, ticket, 'completed', { committed: true }), /snapshot rows are captured/)
    for (const change of [values => { values[0].data = 'Changed earlier question' },
        values => { [values[0], values[1]] = [values[1], values[0]] },
        values => { values[0].metadata.time = 2 }]) {
        const recoveredRows = structuredClone(rows)
        change(recoveredRows)
        assert.throws(() => verifyRecoveryWitness(before(), after, ticket, 'completed', { ...options, recoveredRows }),
            /Full ordered rows survive committed snapshot initialization/)
    }
    for (const events of [[], [event('native_generation_active', [], 'pending')],
        [event('native_generation_active', [{ ticket }])], [event('native_generation_active', null)]]) {
        assert.throws(() => verifyRecoveryWitness(before(), { events }, ticket, 'completed', options), /committed ticket is inactive/)
    }
})

test('ordinary terminal observations reject another ticket and the wrong terminal phase', () => {
    verifyTerminalWitness({ events: terminal() }, ticket, 'completed')
    assert.throws(() => verifyTerminalWitness({ events: terminal() }, ticket, 'cancelled'), /this ticket terminal status/)
    for (const other of otherTickets) {
        assert.throws(() => verifyTerminalWitness({ events: terminal() }, other, 'completed'), /this ticket terminal status/)
    }
    assert.throws(() => verifyTerminalWitness({ events: [event('native_generation_proof', { ...proof, ticket: otherTickets[0] })] },
        ticket, 'completed', { requireStatus: false }), /this ticket terminal adoption proof/)
})

const manifest = { identity: 'synthetic-profile', ledgerSha256: 'synthetic-ledger',
    sources: { 'synthetic-product.ts': 'source-hash' }, instrumentation: { 'synthetic-observer.mjs': 'observer-hash' },
    entries: [{ id: 'send.entry', class: 'allowed-webview' }, { id: 'send.preparation', class: 'must-be-native' }] }
const sendSnapshot = () => {
    const profile = installExecutionProfile(manifest, {})
    profile.begin(manifest.identity)
    profile.enter(manifest.identity, 'send.entry')
    return profile.end(manifest.identity)
}
function restartBoundary({ execute = false, processing = false, identity = manifest.identity } = {}) {
    const profile = installExecutionProfile(manifest, {})
    const witness = installCommandWitness({ invoke: async () => [] })
    let rowsChecked = false
    return {
        driver: {
            async begin() { profile.begin(manifest.identity); witness.begin() },
            async startApp() {
                await witness.invoke('native_generation_active', {})
                if (execute) await witness.invoke('native_generation_execute', { ticket })
                if (processing) profile.enter(manifest.identity, 'send.preparation')
            },
            async profile(end) { assert.equal(rowsChecked, true); assert.equal(end, true); return { ...profile.end(manifest.identity), identity } },
            async witness() { return witness.snapshot() },
        },
        async checkRows() { rowsChecked = true },
    }
}

test('ordinary restart observes startup, retains raw counts and validates only actual counts across epochs', async () => {
    const boundary = restartBoundary()
    const original = sendSnapshot()
    const evidence = await verifyRestart(boundary.driver, manifest, original, boundary.checkRows)
    assert.equal(evidence.snapshot.counts['send.entry']['allowed-webview'], 0)
    assert.equal(original.counts['send.entry']['allowed-webview'], 1)
    assert.deepEqual(evidence.witness.events.map(value => value.command), ['native_generation_active'])
})

test('ordinary restart rejects execute, JS processing and mismatched observer identity', async () => {
    for (const [options, expected] of [[{ execute: true }, /Restart never executes/],
        [{ processing: true }, /js-processing:send.preparation/], [{ identity: 'other-profile' }, /observation-identity/]]) {
        const boundary = restartBoundary(options)
        await assert.rejects(verifyRestart(boundary.driver, manifest, sendSnapshot(), boundary.checkRows), expected)
    }
})


test('body cancellation waits for real partial-body progress and terminal cancellation before release', { timeout: 5_000 }, async () => {
    const fixture = await startOllamaFixture()
    const controller = new AbortController()
    let stopped = false
    let settled = false
    let clientOutcome
    const release = fixture.release
    fixture.release = () => { assert.equal(settled, true, 'Cancellation must settle before release'); release() }
    const cancelled = { ticket, phase: 'cancelled', cancelDecision: 'user', dispatch: 'issued', checkpoint: '0', output: 'none' }
    try {
        fixture.holdBody(8)
        const cancellation = cancelHeldBody({
            async stop() { stopped = true; controller.abort() },
            async settled(actual) { assert.deepEqual(actual, ticket); await clientOutcome; settled = true; return cancelled },
        }, fixture, ticket)
        assert.equal(stopped, false, 'Stop waits for body progress rather than request arrival')
        clientOutcome = assert.rejects(fetch(`${fixture.origin}/api/chat`, {
            method: 'POST', body: '{}', signal: controller.signal,
        }).then(response => response.text()))
        const evidence = await cancellation
        assert.deepEqual(evidence.bodyStart, { bodyStartedCount: 1, heldCount: 1 })
        assert.equal(evidence.status, cancelled)
        assert.equal(evidence.abortedCount, 1)
        assert.equal(fixture.requestCount, 1)
        assert.equal(fixture.heldCount, 0)
    } finally { await fixture.close() }
})

test('wire error cases require native failure descriptors and use the native visible message', () => {
    for (const [name, category, field, stage] of [['PERR-JSON', 'decode', 'syntax', 'buffered-json'],
        ['PERR-NUMBER', 'type', 'indexOf', 'transform']]) {
        const failure = { category, field, stage, sdkMessage: 'Synthetic native diagnostic, independent of browser wording.' }
        assert.equal(verifyWireFailure(name, { failure }), failure.sdkMessage)
        for (const key of ['category', 'field', 'stage', 'sdkMessage']) {
            assert.throws(() => verifyWireFailure(name, { failure: { ...failure, [key]: '' } }))
        }
    }
})

test('wire outcomes preserve effects and forbid checkpoints and output for body cancellation and response failures', () => {
    const effects = [{ requestId: 'synthetic-effect', intentDigest: 'synthetic-digest', receiptRevision: 4 }]
    for (const name of ['CANA-BODY', 'PERR-JSON', 'PERR-NUMBER']) {
        const status = { ticket, phase: name === 'CANA-BODY' ? 'cancelled' : 'failed',
            dispatch: 'issued', cancelDecision: name === 'CANA-BODY' ? 'user' : null, checkpoint: '0', output: 'none' }
        const proof = { ticket, at: status.phase, receipts: { effects, output: null }, outputMessageId: null, failedWithOutput: false }
        const issued = { receipts: { effects } }
        verifyWireProof(name, status, proof, issued)
        for (const changed of [{ ...status, checkpoint: '1' }, { ...status, output: 'committed' }, { ...status, phase: 'completed' }]) {
            assert.throws(() => verifyWireProof(name, changed, proof, issued))
        }
        for (const changed of [{ ...proof, receipts: { ...proof.receipts, effects: [] } },
            { ...proof, receipts: { ...proof.receipts, output: { receiptRevision: 5 } } },
            { ...proof, outputMessageId: 'unexpected-output' }, { ...proof, ticket: otherTickets[0] }]) {
            assert.throws(() => verifyWireProof(name, status, changed, issued))
        }
        if (name === 'CANA-BODY') assert.throws(() => verifyWireProof(name, { ...status, cancelDecision: 'edit' }, proof, issued))
    }
})


const jsManifest = { ...manifest, entries: JSON.parse(readFileSync(
    new URL('../execution-profile/js-processing-ledger.json', import.meta.url), 'utf8')).entries }
function jsSnapshot(historyLimit = true) {
    const profile = installExecutionProfile(jsManifest, {})
    profile.begin(jsManifest.identity)
    for (const id of ['send.entry', 'send.preparation', 'request.wrapper', 'request.main', 'request.ollama',
        'response.application', 'tokenizer.explicit', ...(historyLimit ? ['send.history-window'] : [])]) {
        profile.enter(jsManifest.identity, id)
    }
    return profile.end(jsManifest.identity)
}

test('scenario startup seeds the history option, native opt-in and refusal trigger before the real product entry can start', async () => {
    for (const name of ['JS-HISTORY', 'NATIVE-HISTORY', 'JS', 'SUCC', 'SUCC-UNHELD', 'UNSUPPORTED']) {
        const driver = new UiDriver(null, jsManifest)
        const stored = { settings: {}, root: null, presets: null, committed: false, started: false }
        driver.invoke = async (command, args = {}) => {
            if (command === 'pds_open') return { revision: 0 }
            if (command === 'pds_replace_begin') return { stagingId: 'synthetic-stage' }
            if (command === 'pds_replace_put_root') stored.root = structuredClone(args.root)
            if (command === 'pds_replace_put_presets') stored.presets = structuredClone(args.presets)
            if (command === 'pds_replace_commit') stored.committed = true
            if (command === 'pds_set_device_setting') stored.settings[args.key] = JSON.parse(args.value)
            return {}
        }
        driver.startApp = async () => {
            assert.equal(stored.committed, true, 'Synthetic data is committed before product startup')
            assert.equal(stored.settings.risuNestNativeOllamaGeneration, !['JS', 'JS-HISTORY'].includes(name))
            assert.equal(stored.root.language, 'en')
            assert.equal(stored.root.maxContext, 4000)
            assert.equal(stored.root.requestRetrys, 0)
            const fallback = name === 'UNSUPPORTED' ? ['ollama-hosted'] : undefined
            assert.deepEqual(stored.root.fallbackModels?.model, fallback)
            assert.deepEqual(stored.presets.map(preset => preset.fallbackModels?.model), [fallback])
            if (name.endsWith('-HISTORY')) {
                assert.deepEqual(stored.settings.risuNestDeviceSettings, {
                    schema: 'risunest.device-settings/v1', performanceProfile: 'normal',
                    androidKeepAliveDuringGeneration: true, nativeFileLogEnabled: true,
                    generationHistoryLimitEnabled: true, generationHistoryLimitMultiplier: 2,
                    messageSendKey: 'enter', pluginOutputCurrentChatOnly: false,
                })
            } else assert.equal(stored.settings.risuNestDeviceSettings, undefined, 'Existing controls retain their default history setting')
            stored.started = true
        }
        await startScenario(driver, name, 'http://127.0.0.1:1')
        assert.equal(stored.started, true)
    }
})

test('UNSUPPORTED requires its seeded refusal and names a lifted refusal as a stale trigger', () => {
    const reservation = (result, state = 'resolved') => ({ events: [event('native_generation_host_init', null),
        event('native_generation_reserve_ollama', result, state)] })
    verifyUnsupportedReservation(reservation({ unsupported: 'fallbackModels' }))
    for (const [witness, observed] of [[reservation({ ticket }), 'a reserved ticket'],
        [reservation({ unsupported: 'requestRetrys' }), 'refusal requestRetrys'],
        [reservation(null, 'rejected'), 'a rejected reservation'], [{ events: [] }, 'a missing reservation']]) {
        assert.throws(() => verifyUnsupportedReservation(witness),
            new RegExp(`Stale UNSUPPORTED trigger: expected refusal fallbackModels, observed ${observed}`))
    }
})

test('an unheld native completion fails on a renderer cancellation or a non-completed phase', () => {
    verifyUnheldCompletion(before(), status)
    const cancelled = before()
    cancelled.events.push(event('native_generation_cancel', { ticket, phase: 'cancelled', cancelDecision: 'authority' }))
    assert.throws(() => verifyUnheldCompletion(cancelled, { ...status, phase: 'cancelled' }),
        /cancelled an unheld native completion \(resolved cancelled\/authority\)/)
    assert.throws(() => verifyUnheldCompletion(before(), { ...status, phase: 'failed' }), /unheld native reply completes/)
})

test('the history positive requires a measured history entry while existing JS-zero and native-zero controls stay strict', () => {
    const enabled = jsSnapshot()
    const disabled = jsSnapshot(false)
    verifyJsControl(enabled, jsManifest, { historyLimit: true })
    verifyJsControl(disabled, jsManifest)
    assert.throws(() => verifyJsControl(disabled, jsManifest, { historyLimit: true }), /history-window positive control/)
    assert.throws(() => verifyJsControl(enabled, jsManifest), /history limit disabled/)
    const missing = structuredClone(enabled)
    delete missing.counts['send.history-window']
    assert.throws(() => verifyJsControl(missing, jsManifest), /entry-counts/)
    assert.throws(() => assertNoJsProcessing(enabled, jsManifest), /js-processing/)
    const native = installExecutionProfile(jsManifest, {})
    native.begin(jsManifest.identity)
    native.enter(jsManifest.identity, 'send.entry')
    assertNoJsProcessing(native.end(jsManifest.identity), jsManifest)
})

function jsRestartBoundary({ changedRows = false, native = false, processing = false, scan = true, scanPending = false, active = [], identity = jsManifest.identity } = {}) {
    const rows = [{ chatId: 'earlier', role: 'user', data: 'Earlier question', metadata: { time: 1 } },
        { chatId: 'reply', role: 'char', data: 'Synthetic JS history reply.' }]
    const profile = installExecutionProfile(jsManifest, {})
    const witness = installCommandWitness({ invoke: async command => command === 'native_generation_active'
        ? scanPending ? new Promise(() => {}) : active : status })
    const state = { visible: null }
    return { rows, output: rows[1], state, driver: {
        async begin() { profile.begin(jsManifest.identity); witness.begin() },
        async startApp() {
            if (scan) {
                const activeScan = witness.invoke('native_generation_active', {})
                if (!scanPending) await activeScan
            }
            if (native) await witness.invoke('native_generation_execute', { ticket })
            if (processing) profile.enter(jsManifest.identity, 'send.preparation')
        },
        async messages() { const value = structuredClone(rows); if (changedRows) value[0].metadata.time = 2; return value },
        async shown(chatId, data) { state.visible = { chatId, data } },
        async profile(end) { assert.equal(end, true); return { ...profile.end(jsManifest.identity), identity } },
        async witness() { return witness.snapshot() },
    } }
}

test('JS history restart retains full ordered rows, visible reply, raw zero-processing profile and empty native active scan', async () => {
    const boundary = jsRestartBoundary()
    const evidence = await verifyJsRestart(boundary.driver, jsManifest, jsSnapshot(), boundary.rows, boundary.output)
    assert.deepEqual(boundary.state.visible, { chatId: boundary.output.chatId, data: boundary.output.data })
    assert.equal(evidence.rowsVerified, true)
    assert.equal(evidence.snapshot.counts['send.entry']['allowed-webview'], 0)
    assert.equal(evidence.witness.events.length, 1)
    assert.equal(evidence.witness.events[0].command, 'native_generation_active')
    assert.deepEqual(evidence.witness.events[0].result, [])
})

test('JS history restart rejects earlier-row changes, native execution, generation processing and foreign observers', async () => {
    for (const [options, expected] of [[{ changedRows: true }, /rows survive actual app restart/],
        [{ native: true }, /permits only empty native active scans/],
        [{ scan: false }, /observes an empty native active scan/], [{ active: [status] }, /permits only empty native active scans/],
        [{ scanPending: true }, /permits only empty native active scans/], [{ active: null }, /permits only empty native active scans/],
        [{ processing: true }, /performs no generation processing/],
        [{ identity: 'other-profile' }, /observation-identity/]]) {
        const boundary = jsRestartBoundary(options)
        await assert.rejects(verifyJsRestart(boundary.driver, jsManifest, jsSnapshot(), boundary.rows, boundary.output), expected)
    }
})


const loreFixtureSource = readFileSync(new URL('../../fixtures/native-generation/ollama-text.json', import.meta.url), 'utf8')
function loreOracleBoundary(caseId = 'lore-global-local-state') {
    const fixture = JSON.parse(loreFixtureSource)
    const source = fixture.cases.find(entry => entry.id === caseId)
    assert.ok(source, 'Expected actual oracle input case')
    const assignments = (caseId === 'module-selected-order'
        ? ['__internal_ka_selected-b', '__internal_ka_selected-a', '__internal_ka_selected-c']
        : ['__internal_ka_global', '__internal_da_local']).map(name => ({ name, value: 'true' }))
    const state = { preserve: 'value', ...Object.fromEntries(assignments.map(({ name, value }) => ['$' + name, value])) }
    const expected = { id: source.id, body: JSON.stringify({ model: 'synthetic-model',
        messages: [{ role: 'system', content: 'Local reads true.' }], stream: false }),
        prepared: { messages: [{ role: 'system', content: 'Local reads true.' }] }, output: { text: source.expectedOutput },
        database: { ...fixture.root, ...source.root, botPresets: [{ ...fixture.preset, ...source.preset }],
            characters: [{ ...fixture.character, ...source.character, chats: [{ ...fixture.conversation,
                ...source.conversation, message: fixture.messages }] }] },
        loreObservation: { assignments, requestScriptstate: state, storedScriptstate: { ...state } } }
    const oracle = { schema: 1, fixtureSha256: createHash('sha256').update(loreFixtureSource).digest('hex'),
        cases: fixture.cases.map(entry => entry.id === source.id ? expected : { id: entry.id }) }
    return { oracle, expected, lore: qualifyLoreOracle(oracle, loreFixtureSource, caseId) }
}

test('lore oracle qualification rejects stale input, incomplete identities and missing observed keep/dont outcomes', () => {
    const { oracle } = loreOracleBoundary()
    assert.equal(qualifyLoreOracle(oracle).caseId, 'lore-global-local-state')
    for (const change of [value => { value.fixtureSha256 = 'stale' },
        value => { value.cases.pop() }, value => { value.cases.reverse() },
        value => { value.cases.find(entry => entry.id === 'lore-global-local-state').output.text = 'false|true' },
        value => { delete value.cases.find(entry => entry.id === 'lore-global-local-state').loreObservation.requestScriptstate.$__internal_ka_global },
        value => { value.cases.find(entry => entry.id === 'lore-global-local-state').loreObservation.storedScriptstate.preserve = 'changed' },
        value => { value.cases.find(entry => entry.id === 'lore-global-local-state').loreObservation.assignments.reverse() }]) {
        const changed = structuredClone(oracle)
        change(changed)
        assert.throws(() => qualifyLoreOracle(changed), /Lore oracle/)
    }
})

test('lore request verifier requires the actual TS request bytes and one request, while module fallback requires module content', () => {
    const { lore } = loreOracleBoundary()
    const request = { method: 'POST', url: '/api/chat', text: lore.expected.body, json: JSON.parse(lore.expected.body) }
    verifyLoreRequest([request], lore)
    assert.throws(() => verifyLoreRequest([request, request], lore), /exactly one fixture request/)
    assert.throws(() => verifyLoreRequest([{ ...request, text: request.text.replace('true', 'false') }], lore), /request bytes match real TS Send/)
    assert.throws(() => verifyLoreRequest([request], lore, 'Selected module lore.'), /Selected module reaches the JS request/)
    const moduleRequest = { ...request, json: { ...request.json,
        messages: [...request.json.messages, { role: 'system', content: 'Selected module lore.' }] } }
    verifyLoreRequest([moduleRequest], lore, 'Selected module lore.')
    assert.throws(() => verifyLoreRequest([{ ...moduleRequest, json: { ...moduleRequest.json, stream: true } }], lore, 'Selected module lore.'))
})

test('lore stored-map evidence rejects lost, wrongly typed or unrelated changed state at Issued, completion and restart', () => {
    const { lore } = loreOracleBoundary()
    for (const phase of ['issued', 'terminal', 'restart']) {
        const conversation = { scriptstate: structuredClone(lore.expected.loreObservation.storedScriptstate) }
        verifyLoreState(conversation, lore, phase)
        for (const change of [state => { delete state.$__internal_ka_global },
            state => { state.$__internal_da_local = true }, state => { state.preserve = 'changed' }]) {
            const changed = structuredClone(conversation)
            change(changed.scriptstate)
            assert.throws(() => verifyLoreState(changed, lore, phase), /full stored variable map/)
        }
    }
})

test('lore proof requires the scriptstate unit and preserves real generic receipt identities', () => {
    const effects = [{ requestId: 'preparation', intentDigest: 'digest', receiptRevision: 3 }]
    const keys = [['root', 'statics'], ['character', 'character', 'lastInteraction'],
        ['conversation', 'character', 'conversation', 'lastMemory'], ['conversation', 'character', 'conversation', 'scriptstate']].map(key => JSON.stringify(key))
    const issued = { at: 'issued', outputMessageId: null, affectedUnitKeys: keys, receipts: { effects, output: null } }
    const completed = { receipts: { effects: structuredClone(effects) } }
    verifyLoreReceipts(completed, issued)
    assert.throws(() => verifyLoreReceipts(completed, { ...issued, affectedUnitKeys: keys.slice(0, -1) }), /includes stored variable effects/)
    assert.throws(() => verifyLoreReceipts({ receipts: { effects: [] } }, issued), /receipt identities survive completion/)
    assert.throws(() => verifyLoreReceipts({ receipts: { effects: [{ ...effects[0], receiptRevision: 4 }] } }, issued), /receipt identities survive completion/)
})

function loreSeedBoundary() {
    const stored = {}
    const driver = new UiDriver(null, jsManifest)
    driver.invoke = async (command, args = {}) => {
        if (command === 'pds_open') return { revision: 0 }
        if (command === 'pds_replace_begin') return { stagingId: 'synthetic' }
        if (command === 'pds_replace_put_root') stored.root = structuredClone(args.root)
        if (command === 'pds_replace_put_presets') stored.presets = structuredClone(args.presets)
        if (command === 'pds_replace_add_characters') stored.characters = structuredClone(args.characters)
        if (command === 'pds_replace_commit') stored.committed = true
        return {}
    }
    return { stored, driver }
}

test('lore seed preserves captured root-owned budget and lore before a real composer submission', async () => {
    for (const selected of [false, true]) {
        const { lore } = loreOracleBoundary()
        if (selected) lore.module = { id: 'unselected-lore-module', lorebook: [{ content: 'Selected module lore.' }] }
        const { stored, driver } = loreSeedBoundary()
        await driver.seed('http://127.0.0.1:1', true, false, false, lore)
        assert.equal(stored.root.loreBookToken, 200)
        assert.equal(stored.presets[0].loreBookToken, 0)
        assert.deepEqual(stored.characters[0].globalLore, lore.expected.database.characters[0].globalLore)
        assert.deepEqual(stored.characters[0].chats[0].localLore, lore.expected.database.characters[0].chats[0].localLore)
        assert.deepEqual(stored.characters[0].chats[0].message, lore.expected.database.characters[0].chats[0].message.slice(0, -1))
        assert.equal(stored.characters[0].chats[0].message.some(message => message.data === lore.input), false)
        assert.deepEqual(stored.root.enabledModules, selected ? ['unselected-lore-module'] : [])
    }
})

test('module fallback preserves ordered history and rejects extra output, changed prior rows or extra composer submissions', () => {
    const { lore } = loreOracleBoundary()
    const beforeRows = structuredClone(lore.expected.database.characters[0].chats[0].message.slice(0, -1))
    const composer = { role: 'user', data: lore.input, chatId: 'composer-generated', time: 123 }
    const output = { role: 'char', data: lore.expected.output.text, chatId: 'output-generated', time: 456 }
    const rows = [...beforeRows, composer, output]
    assert.deepEqual(verifyLoreModuleRows(rows, beforeRows, lore), output)
    assert.throws(() => verifyLoreModuleRows([...rows, { ...output, chatId: 'duplicate-output' }], beforeRows, lore),
        /one composer row and one output/)
    const changedPrefix = structuredClone(rows)
    changedPrefix[0].data = 'Changed earlier question.'
    assert.throws(() => verifyLoreModuleRows(changedPrefix, beforeRows, lore), /preserves exact ordered pre-Send rows/)
    assert.throws(() => verifyLoreModuleRows([...beforeRows, composer, { ...composer, chatId: 'duplicate-composer' }, output], beforeRows, lore),
        /one composer row and one output/)
})

test('selected-module refusal rejects a ticket or another refusal instead of accepting a stale control', () => {
    const witness = unsupported => ({ events: [event('native_generation_reserve_ollama', { unsupported })] })
    verifyUnsupportedReservation(witness('modules'), 'modules', 'LORE-MODULE')
    assert.throws(() => verifyUnsupportedReservation({ events: [event('native_generation_reserve_ollama', { unsupported: 'modules', ticket })] },
        'modules', 'LORE-MODULE'), /a reserved ticket/)
    assert.throws(() => verifyUnsupportedReservation(witness('fallbackModels'), 'modules', 'LORE-MODULE'), /Stale LORE-MODULE trigger/)
    assert.throws(() => verifyUnsupportedReservation({ events: [event('native_generation_reserve_ollama', { ticket })] },
        'modules', 'LORE-MODULE'), /a reserved ticket/)
})


async function withLoreOracleBoundary(oracle, run) {
    const source = JSON.stringify(oracle)
    const directory = mkdtempSync(join(tmpdir(), 'risunest-module-route-'))
    const file = join(directory, 'oracle.json')
    const previous = Object.fromEntries(['RISUNEST_OLLAMA_PREPARATION_ORACLE', 'RISUNEST_OLLAMA_PREPARATION_ORACLE_SHA256']
        .map(key => [key, process.env[key]]))
    writeFileSync(file, source)
    try {
        process.env.RISUNEST_OLLAMA_PREPARATION_ORACLE = file
        process.env.RISUNEST_OLLAMA_PREPARATION_ORACLE_SHA256 = createHash('sha256').update(source).digest('hex')
        return await run()
    } finally {
        for (const [key, value] of Object.entries(previous)) {
            if (value === undefined) delete process.env[key]
            else process.env[key] = value
        }
        unlinkSync(file)
        rmdirSync(directory)
    }
}

test('module fallback startup selects a genuine unsupported caption before the real product starts', async () => {
    const { oracle } = loreOracleBoundary()
    await withLoreOracleBoundary(oracle, async () => {
        const { stored, driver } = loreSeedBoundary()
        driver.startApp = async () => {
            assert.equal(stored.committed, true)
            assert.deepEqual(stored.root.enabledModules, ['unselected-lore-module'])
            assert.equal(stored.root.modules[0].customModuleToggle, 'synthetic=Synthetic module caption=caption')
            assert.equal(stored.root.modules[0].lorebook[0].content, 'Unselected module lore.')
            stored.started = true
        }
        await startScenario(driver, 'LORE-MODULE', 'http://127.0.0.1:1')
        assert.equal(stored.started, true)
    })
})

test('MODULE oracle keeps its own exact write order and map while the LORE default rejects that row', () => {
    const { oracle, expected } = loreOracleBoundary('module-selected-order')
    assert.equal(qualifyLoreOracle(oracle, loreFixtureSource, 'module-selected-order').caseId, 'module-selected-order')
    assert.throws(() => qualifyLoreOracle(oracle, loreFixtureSource, 'literal-groups'), /Qualified lore\/module oracle case/)
    const missing = structuredClone(oracle)
    missing.cases = missing.cases.filter(entry => entry.id !== 'module-selected-order')
    assert.throws(() => qualifyLoreOracle(missing, loreFixtureSource, 'module-selected-order'), /complete case identities/)
    for (const change of [value => { value.loreObservation.assignments.reverse() },
        value => { value.loreObservation.requestScriptstate['$__internal_ka_poison'] = 'true' },
        value => { delete value.loreObservation.storedScriptstate['$__internal_ka_selected-b'] }]) {
        const changed = structuredClone(oracle)
        change(changed.cases.find(entry => entry.id === 'module-selected-order'))
        assert.throws(() => qualifyLoreOracle(changed, loreFixtureSource, 'module-selected-order'), /Lore oracle|Module oracle/)
    }
    const misplaced = structuredClone(oracle)
    misplaced.cases[misplaced.cases.findIndex(entry => entry.id === 'lore-global-local-state')] = { ...structuredClone(expected), id: 'lore-global-local-state' }
    assert.throws(() => qualifyLoreOracle(misplaced), /Lore oracle/)
})

test('MODULE startup preserves captured library order and all selection sources instead of the one-module fallback overlay', async () => {
    const { oracle, lore } = loreOracleBoundary('module-selected-order')
    const captured = lore.expected.database
    await withLoreOracleBoundary(oracle, async () => {
        const { stored, driver } = loreSeedBoundary()
        driver.startApp = async () => {
            assert.equal(stored.committed, true)
            assert.deepEqual(stored.root.modules, captured.modules)
            assert.deepEqual(stored.root.enabledModules, captured.enabledModules)
            assert.equal(stored.root.moduleIntergration, captured.moduleIntergration)
            assert.equal(stored.presets[0].moduleIntergration, captured.botPresets[0].moduleIntergration)
            assert.equal(stored.root.selectedPersona, captured.selectedPersona)
            assert.deepEqual(stored.root.personas, captured.personas)
            assert.deepEqual(stored.characters[0].modules, captured.characters[0].modules)
            assert.deepEqual(stored.characters[0].chats[0].modules, captured.characters[0].chats[0].modules)
            assert.equal(stored.characters[0].chats[0].bindedPersona, captured.characters[0].chats[0].bindedPersona)
            assert.deepEqual(stored.characters[0].chats[0].message, captured.characters[0].chats[0].message.slice(0, -1))
            stored.started = true
        }
        await startScenario(driver, 'MODULE', 'http://127.0.0.1:1')
        assert.equal(stored.started, true)
    })
})


function triggerOracleBoundary() {
    const fixture = JSON.parse(loreFixtureSource)
    const database = (entry, messages = fixture.messages) => ({ ...fixture.root, ...entry.root, botPresets: [{ ...fixture.preset, ...entry.preset }],
        characters: [{ ...fixture.character, ...entry.character, chats: [{ ...fixture.conversation, ...entry.conversation, message: structuredClone(messages) }] }] })
    const prompt = [{ role: 'system', content: 'Synthetic trigger prompt.' }]
    const sendRow = (id, state, note, storedMessages = fixture.messages) => {
        const source = fixture.cases.find(entry => entry.id === id)
        return { id, body: JSON.stringify({ model: 'synthetic-model', messages: prompt, stream: false }), prepared: { messages: prompt },
            output: { text: source.expectedOutput }, database: database(source), triggerObservation: { requestScriptstate: { ...state },
                storedScriptstate: { ...state }, storedNote: note, storedMessages: structuredClone(storedMessages) } }
    }
    const base = { preserve: 'value' }
    const edited = [{ ...fixture.messages[0], data: 'Edited first question.' },
        { role: 'char', data: 'Edited answer Synthetic character.', chatId: 'message-b' }, fixture.messages[2]]
    const rows = {
        'trigger-setvar-string': sendRow('trigger-setvar-string',
            { ...base, $count: '10', $greeting: 'hello Synthetic character', $copied: 'hello Synthetic character' }, fixture.conversation.note),
        'trigger-author-note': sendRow('trigger-author-note', { ...base, $before: 'Old author note.', $after: 'Trigger author note.' }, 'Trigger author note.'),
        'trigger-modifychat': sendRow('trigger-modifychat', { ...base, $readBack: 'Edited answer Synthetic character.' }, fixture.conversation.note, edited),
    }
    const stop = { id: 'trigger-stop', outcome: 'stop', requests: 0, body: null, error: null,
        database: database(fixture.effects.find(entry => entry.id === 'trigger-stop'), fixture.effectMessages),
        triggerObservation: { requestScriptstate: null, storedScriptstate: { ...base, $stopped: 'true', $afterStop: 'still-runs' },
            storedNote: fixture.conversation.note, storedMessages: [] } }
    const oracle = { schema: 1, fixtureSha256: createHash('sha256').update(loreFixtureSource).digest('hex'),
        cases: fixture.cases.map(entry => rows[entry.id] ?? { id: entry.id }),
        effects: fixture.effects.map(entry => entry.id === 'trigger-stop' ? stop : { id: entry.id }) }
    return { oracle, fixture, trigger: name => qualifyTriggerOracle(oracle, loreFixtureSource, name) }
}
const triggerNames = ['TRIGGER-VAR', 'TRIGGER-NOTE', 'TRIGGER-DECLINE', 'TRIGGER-STOP']

test('trigger oracle qualification takes each refusal from its fixture row and rejects stale or incomplete oracle rows', () => {
    const { oracle, fixture, trigger } = triggerOracleBoundary()
    const qualified = Object.fromEntries(triggerNames.map(name => [name, trigger(name)]))
    assert.deepEqual(Object.values(qualified).map(value => [value.caseId, value.decline]), [['trigger-setvar-string', null],
        ['trigger-author-note', null], ['trigger-modifychat', 'effect'], ['trigger-stop', 'trigger-stop-sending']])
    assert.deepEqual(qualified['TRIGGER-VAR'].units, ['scriptstate'])
    assert.deepEqual(qualified['TRIGGER-NOTE'].units, ['scriptstate', 'note'])
    assert.equal(qualified['TRIGGER-DECLINE'].input, fixture.messages.at(-1).data)
    assert.deepEqual(qualified['TRIGGER-STOP'].triggerscript, fixture.effects.find(entry => entry.id === 'trigger-stop').character.triggerscript)
    const row = (value, id) => value.cases.find(entry => entry.id === id) ?? value.effects.find(entry => entry.id === id)
    for (const [name, change] of [['TRIGGER-VAR', value => { value.fixtureSha256 = 'stale' }],
        ['TRIGGER-STOP', value => { value.effects.shift() }],
        ['TRIGGER-VAR', value => { delete row(value, 'trigger-setvar-string').triggerObservation.storedScriptstate }],
        ['TRIGGER-NOTE', value => { row(value, 'trigger-author-note').triggerObservation.storedNote = 'Old author note.' }],
        ['TRIGGER-DECLINE', value => { row(value, 'trigger-modifychat').output.text = 'Unedited answer.' }],
        ['TRIGGER-STOP', value => { row(value, 'trigger-stop').requests = 1 }],
        ['TRIGGER-STOP', value => { row(value, 'trigger-stop').triggerObservation.jsonUnsafe = [{ at: 'storedScriptstate.$x', value: 'undefined' }] }]]) {
        const changed = structuredClone(oracle)
        change(changed)
        assert.throws(() => qualifyTriggerOracle(changed, loreFixtureSource, name), /Trigger oracle/)
    }
    // A refused row whose fixture stops naming a refusal no longer qualifies as the refusal scenario.
    const lifted = JSON.parse(loreFixtureSource)
    delete lifted.cases.find(entry => entry.id === 'trigger-modifychat').decline
    const liftedSource = JSON.stringify(lifted)
    const relabeled = { ...structuredClone(oracle), fixtureSha256: createHash('sha256').update(liftedSource).digest('hex') }
    assert.throws(() => qualifyTriggerOracle(relabeled, liftedSource, 'TRIGGER-DECLINE'), /runs natively or names its refusal/)
    assert.throws(() => qualifyTriggerOracle(oracle, loreFixtureSource, 'LORE'), /Qualified trigger oracle scenario/)
})

test('trigger stored maps are compared whole per phase, and the stored author note at every phase', () => {
    const { trigger } = triggerOracleBoundary()
    for (const name of ['TRIGGER-VAR', 'TRIGGER-NOTE']) {
        const qualified = trigger(name)
        const observation = qualified.expected.triggerObservation
        // Issued compares the map the request was built with, the later phases the stored map.
        observation.requestScriptstate = { ...observation.requestScriptstate, $requestOnly: 'synthetic' }
        for (const phase of ['issued', 'terminal', 'restart']) {
            const conversation = { scriptstate: structuredClone(observation[phase === 'issued' ? 'requestScriptstate' : 'storedScriptstate']),
                note: observation.storedNote }
            verifyTriggerState(conversation, qualified, phase)
            for (const change of [state => { state.preserve = 'changed' }, state => { delete state.preserve },
                state => { state.$extra = 'synthetic' }]) {
                const changed = structuredClone(conversation)
                change(changed.scriptstate)
                assert.throws(() => verifyTriggerState(changed, qualified, phase), new RegExp(`Trigger ${phase} full stored variable map`))
            }
            const other = { ...conversation, scriptstate: structuredClone(observation[phase === 'issued' ? 'storedScriptstate' : 'requestScriptstate']) }
            assert.throws(() => verifyTriggerState(other, qualified, phase), /full stored variable map/)
            assert.throws(() => verifyTriggerState({ ...conversation, note: 'Old author note.' }, qualified, phase),
                new RegExp(`Trigger ${phase} stored author note`))
        }
    }
})

test('trigger receipts require the scriptstate unit, the note unit for an author note write, and surviving effect receipts', () => {
    const effects = [{ requestId: 'preparation', intentDigest: 'digest', receiptRevision: 3 },
        { requestId: 'variables', intentDigest: 'digest-2', receiptRevision: 4 }]
    const unit = name => JSON.stringify(['conversation', 'character', 'conversation', name])
    const keys = [JSON.stringify(['root', 'statics']), JSON.stringify(['character', 'character', 'lastInteraction']),
        unit('scriptstate'), unit('note'), unit('lastMemory')]
    const issued = { at: 'issued', outputMessageId: null, affectedUnitKeys: keys, receipts: { effects, output: null } }
    const completed = { receipts: { effects: structuredClone(effects) } }
    for (const units of [['scriptstate'], ['scriptstate', 'note']]) verifyTriggerReceipts(completed, issued, units)
    const without = name => ({ ...issued, affectedUnitKeys: keys.filter(key => key !== unit(name)) })
    assert.throws(() => verifyTriggerReceipts(completed, without('scriptstate'), ['scriptstate']), /conversation scriptstate unit/)
    assert.throws(() => verifyTriggerReceipts(completed, without('note'), ['scriptstate', 'note']), /conversation note unit/)
    assert.throws(() => verifyTriggerReceipts({ receipts: { effects: effects.slice(0, 1) } }, issued, ['scriptstate']), /survive completion/)
    assert.throws(() => verifyTriggerReceipts(completed, issued, []), /name their conversation units/)
})

test('trigger requests carry the real TS bytes once under the trigger labels', () => {
    const qualified = triggerOracleBoundary().trigger('TRIGGER-VAR')
    const request = { method: 'POST', url: '/api/chat', text: qualified.expected.body, json: JSON.parse(qualified.expected.body) }
    verifyLoreRequest([request], qualified, null, 'Trigger')
    assert.throws(() => verifyLoreRequest([request, request], qualified, null, 'Trigger'), /Trigger sends exactly one fixture request/)
    assert.throws(() => verifyLoreRequest([{ ...request, text: request.text.replace('trigger', 'other') }], qualified, null, 'Trigger fallback'),
        /Trigger fallback request bytes match real TS Send/)
})

test('trigger rows keep the earlier rows TS stored, one composer row and the TS output', () => {
    const { trigger } = triggerOracleBoundary()
    for (const name of ['TRIGGER-VAR', 'TRIGGER-DECLINE']) {
        const qualified = trigger(name)
        const earlier = structuredClone(qualified.expected.triggerObservation.storedMessages.slice(0, -1))
        const composer = { role: 'user', data: qualified.input, chatId: 'composer-generated', time: 1 }
        const output = { role: 'char', data: qualified.expected.output.text, chatId: 'output-generated' }
        const rows = [...earlier, composer, output]
        assert.deepEqual(verifyTriggerRows(rows, qualified), output)
        for (const [changed, expected] of [[[...rows, { ...output, chatId: 'second-output' }], /one composer row and one output/],
            [[{ ...earlier[0], data: 'Changed earlier row.' }, ...rows.slice(1)], /earlier rows TS stored/],
            [[...earlier, composer, { ...output, data: 'Other output.' }], /stores the TS output/],
            [[...earlier, { ...composer, data: 'Other composer text.' }, output], /stores the composer row/]]) {
            assert.throws(() => verifyTriggerRows(changed, qualified), expected)
        }
    }
    // The refused row's trigger edits the earlier rows, so the seeded rows themselves do not satisfy it.
    const decline = trigger('TRIGGER-DECLINE')
    const seeded = decline.expected.database.characters[0].chats[0].message.slice(0, -1)
    assert.throws(() => verifyTriggerRows([...seeded, { role: 'user', data: decline.input }, { role: 'char', data: decline.expected.output.text }],
        decline), /earlier rows TS stored/)
})

function triggerStopSnapshot(entries) {
    const profile = installExecutionProfile(jsManifest, {})
    profile.begin(jsManifest.identity)
    for (const [id, mode] of entries) profile.enter(jsManifest.identity, id, mode)
    return profile.end(jsManifest.identity)
}

test('a stopping start trigger needs JS preparation and the JS start trigger, with no request or response entry', () => {
    const stopped = [['send.entry'], ['send.preparation'], ['triggers.mode', 'start'], ['triggers.mode', 'input']]
    verifyTriggerStopControl(triggerStopSnapshot(stopped), jsManifest)
    for (const id of ['request.wrapper', 'request.main', 'request.ollama', 'response.application']) {
        assert.throws(() => verifyTriggerStopControl(triggerStopSnapshot([...stopped, [id]]), jsManifest),
            new RegExp(`makes no request: ${id.replace('.', '\\.')}`))
    }
    assert.throws(() => verifyTriggerStopControl(triggerStopSnapshot([['send.entry'], ['send.preparation'], ['triggers.mode', 'input']]), jsManifest),
        /ran the start trigger/)
    assert.throws(() => verifyTriggerStopControl(triggerStopSnapshot([['send.entry'], ['triggers.mode', 'start']]), jsManifest), /entered JS preparation/)
    assert.throws(() => verifyTriggerStopControl({ ...triggerStopSnapshot(stopped), identity: 'other-profile' }, jsManifest), /observation-identity/)
    // The refused-effect fallback reuses the same JS start-trigger evidence beside the ordinary JS control.
    verifyJsStartTrigger(triggerStopSnapshot([['send.entry'], ['triggers.mode', 'start']]))
    assert.throws(() => verifyJsStartTrigger(jsSnapshot(false)), /ran the start trigger/)
})

test('a stopped Send keeps the exact earlier rows and only the composer row', () => {
    const before = [{ role: 'user', data: 'Synthetic earlier question.', chatId: 'message-a' },
        { role: 'char', data: 'Synthetic earlier answer.', chatId: 'message-b' }]
    const composer = { role: 'user', data: 'Synthetic composer question.', chatId: 'composer' }
    verifyTriggerStopRows([...before, composer], before)
    assert.throws(() => verifyTriggerStopRows([...before, composer, { role: 'char', data: 'Synthetic native UI reply.', chatId: 'reply' }], before),
        /keeps only the composer row/)
    assert.throws(() => verifyTriggerStopRows([{ ...before[0], data: 'Changed.' }, before[1], composer], before), /preserves exact ordered earlier rows/)
    assert.throws(() => verifyTriggerStopRows([...before, { ...composer, role: 'char' }], before), /stores the composer row/)
})

test('trigger startup seeds the oracle row, or the stop trigger over the ordinary history, before the real product starts', async () => {
    const { oracle, fixture } = triggerOracleBoundary()
    await withLoreOracleBoundary(oracle, async () => {
        for (const name of triggerNames) {
            const stored = { settings: {} }
            const driver = new UiDriver(null, jsManifest)
            driver.invoke = async (command, args = {}) => {
                if (command === 'pds_open') return { revision: 0 }
                if (command === 'pds_replace_begin') return { stagingId: 'synthetic' }
                if (command === 'pds_replace_add_characters') stored.characters = structuredClone(args.characters)
                if (command === 'pds_replace_commit') stored.committed = true
                if (command === 'pds_set_device_setting') stored.settings[args.key] = JSON.parse(args.value)
                return {}
            }
            driver.startApp = async () => {
                assert.equal(stored.committed, true, 'Synthetic data is committed before product startup')
                assert.equal(stored.settings.risuNestNativeOllamaGeneration, true)
                assert.equal(driver.trigger.scenario, name)
                const [character] = stored.characters
                if (name === 'TRIGGER-STOP') {
                    assert.deepEqual(character.triggerscript, fixture.effects.find(entry => entry.id === 'trigger-stop').character.triggerscript)
                    assert.deepEqual(character.chats[0].message.map(message => message.chatId), ['message-a', 'message-b'])
                    assert.deepEqual(character.chats[0].scriptstate, fixture.conversation.scriptstate)
                } else {
                    const captured = driver.trigger.expected.database.characters[0]
                    assert.deepEqual(character.triggerscript, captured.triggerscript)
                    assert.deepEqual(character.chats[0].message, captured.chats[0].message.slice(0, -1))
                    assert.deepEqual([character.chats[0].scriptstate, character.chats[0].note], [captured.chats[0].scriptstate, captured.chats[0].note])
                }
                stored.started = true
            }
            await startScenario(driver, name, 'http://127.0.0.1:1')
            assert.equal(stored.started, true)
        }
    })
})
