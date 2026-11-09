import test from 'node:test'
import assert from 'node:assert/strict'
import { installCommandWitness } from './commandWitness.mjs'
import { runScenario } from './driver.mjs'
import { nativeGateCases, nativeEntryScenarios, verifyNativeSnapshot, verifyNativeTrace,
    verifyNativeGateOutcome, createNativeControlState, cancelAtNativeGate,
    cleanupNativeControls, verifyStaleNativeToken } from './nativeControls.mjs'

const ticket = { epoch: 'synthetic-epoch', sequence: '1' }
const token = gate => ({ boot: 'synthetic-process-boot', generation: 1, ticket: structuredClone(ticket), gate })
const stages = ['requestFormatted', 'bufferDecoded', 'jsonParsed', 'responseTransformed']
const credit = ['creditEntered', 'creditDelegated', ['creditForwarded', 'ok']]
const eventList = values => values.map((value, index) => ({ sequence: index + 1,
    kind: Array.isArray(value) ? value[0] : value, result: Array.isArray(value) ? value[1] : null }))
function snapshot(name) {
    const gate = nativeGateCases[name] ?? 'none'
    let values
    if (gate === 'credit') values = [...stages, 'creditEntered', 'gateEntered', 'gateDropped', 'creditDropped', ['ownerCompleted', 'cancelled']]
    else if (gate === 'offer') values = [...stages, ...credit, 'offerEntered', 'gateEntered', 'gateDropped', 'offerDropped', ['ownerCompleted', 'cancelled']]
    else if (gate === 'preAdmission') values = [...stages, ...credit, 'offerEntered', 'offerDelegated',
        'storagePolicyApplied', 'outputSegmentEntered', 'gateEntered', 'offerDropped',
        'gateReleased', ['outputResolved', 'uncommitted'], ['ownerCompleted', 'cancelled']]
    else if (gate === 'admitted') values = [...stages, ...credit, 'offerEntered', 'offerDelegated',
        'storagePolicyApplied', 'outputSegmentEntered', 'outputAdmitted', 'gateEntered', 'offerDropped',
        'cancelWaiting', 'gateReleased', ['outputResolved', 'committed'], ['ownerCompleted', 'cancelled']]
    else values = [...stages, ...credit, 'offerEntered', 'offerDelegated', 'storagePolicyApplied',
        'outputSegmentEntered', 'outputAdmitted', ['outputResolved', 'committed'], ['offerForwarded', 'accepted'], ['ownerCompleted', 'completed']]
    return { token: token(gate), events: eventList(values), retired: false, drained: true,
        actors: 0, holds: 0, hooks: 0, invalid: null }
}
const renumber = value => { value.events.forEach((event, index) => { event.sequence = index + 1 }); return value }
const retired = value => ({ ...structuredClone(value), retired: true,
    events: [...structuredClone(value.events), { sequence: value.events.length + 1, kind: 'retired', result: null }] })
function outcome(name) {
    const output = name === 'SUCC' || name === 'CAN-ADMITTED'
    const effects = [{ requestId: 'effect-a', receiptRevision: 5 }, { requestId: 'effect-b', receiptRevision: 6 }]
    const phase = name === 'SUCC' ? 'completed' : 'cancelled'
    const status = { ticket, phase, cancelDecision: name === 'SUCC' ? null : 'user', dispatch: 'issued',
        pendingCommit: false, output: output ? 'committed' : 'none', checkpoint: output ? '1' : '0', messageId: 'output-id' }
    const issuedProof = { receipts: { effects: structuredClone(effects) } }
    const proof = { ticket, at: phase, failedWithOutput: false, outputMessageId: output ? status.messageId : null,
        receipts: { effects, output: output ? { sourceSequence: '1', receiptRevision: 7 } : null } }
    return { status, proof, issuedProof }
}

for (const name of nativeEntryScenarios) test('native trace and real receipt contract: ' + name, () => {
    const value = snapshot(name), result = outcome(name)
    verifyNativeTrace(value, value.token, name)
    verifyNativeGateOutcome(name, ticket, result.status, result.proof, result.issuedProof)
})

test('admitted cancellation also permits the actual forwarded Accepted outcome', () => {
    const value = snapshot('CAN-ADMITTED')
    value.events = value.events.filter(event => event.kind !== 'offerDropped')
    value.events.splice(value.events.findIndex(event => event.kind === 'outputResolved') + 1, 0,
        { kind: 'offerForwarded', result: 'accepted' })
    renumber(value)
    verifyNativeTrace(value, value.token, 'CAN-ADMITTED')
})

for (const kind of [...stages, 'storagePolicyApplied']) test('missing real positive stage is refused: ' + kind, () => {
    const value = snapshot('SUCC')
    value.events = value.events.filter(event => event.kind !== kind)
    renumber(value)
    assert.throws(() => verifyNativeTrace(value, value.token, 'SUCC'), /Missing native observation/)
})

for (const field of ['boot', 'generation', 'ticket', 'gate']) test('misattributed native ' + field + ' is refused', () => {
    const value = snapshot('SUCC'), expected = structuredClone(value.token)
    value.token[field] = field === 'ticket' ? { epoch: 'foreign', sequence: '1' } : field === 'generation' ? 2 : 'foreign'
    assert.throws(() => verifyNativeSnapshot(value, expected), /exact boot/)
})

test('native protocol rejects duplicated, gapped, invented and out-of-order events', () => {
    for (const corrupt of [
        value => value.events.push({ ...value.events[0], sequence: value.events.length + 1 }),
        value => { value.events[0].sequence = 2 },
        value => { value.events[0].kind = 'callerSaysRustSucceeded' },
        value => { [value.events[0], value.events[1]] = [value.events[1], value.events[0]]; renumber(value) },
        value => { value.events[0].result = 'caller-defined-success' },
        value => { value.invalid = 'lease-expired' },
        value => { value.actors = 1; value.drained = false },
    ]) {
        const value = snapshot('SUCC'); corrupt(value)
        assert.throws(() => verifyNativeTrace(value, token('none'), 'SUCC'))
    }
})

test('admitted commit must observe the real cancellation waiter before release', () => {
    const value = snapshot('CAN-ADMITTED')
    const waiting = value.events.find(event => event.kind === 'cancelWaiting')
    value.events = value.events.filter(event => event !== waiting)
    value.events.splice(value.events.findIndex(event => event.kind === 'gateReleased') + 1, 0, waiting)
    renumber(value)
    assert.throws(() => verifyNativeTrace(value, value.token, 'CAN-ADMITTED'), /precedes/)
})

test('SUCC requires actual Accepted while a cancelled held offer must not delegate', () => {
    const success = snapshot('SUCC')
    success.events.find(event => event.kind === 'offerForwarded').result = 'unknown'
    assert.throws(() => verifyNativeTrace(success, success.token, 'SUCC'))
    const held = snapshot('CAN-OFFER')
    held.events.splice(-1, 0, { kind: 'offerDelegated', result: null })
    renumber(held)
    assert.throws(() => verifyNativeTrace(held, held.token, 'CAN-OFFER'), /Unexpected native observation/)
})

test('retained cancellation output requires the actual receipt and unchanged Issued effects', () => {
    for (const corrupt of [
        value => { value.proof.receipts.output = null },
        value => { value.proof.outputMessageId = 'foreign-output' },
        value => { value.proof.receipts.effects.push({ requestId: 'duplicate', receiptRevision: 6 }) },
        value => { value.status.cancelDecision = null },
        value => { value.proof.failedWithOutput = true },
        value => { value.status.checkpoint = '0' },
    ]) {
        const value = outcome('CAN-ADMITTED'); corrupt(value)
        assert.throws(() => verifyNativeGateOutcome('CAN-ADMITTED', ticket, value.status, value.proof, value.issuedProof))
    }
})

function completedBoundary(name, { late = false } = {}) {
    const before = snapshot(name), final = retired(before)
    if (late) { final.events.push({ sequence: final.events.length + 1, kind: 'gateReleased', result: null }) }
    let isRetired = false
    return { before, final, driver: { async invoke(command) {
        if (command === 'ollama_ui_arm') return before.token
        if (command === 'ollama_ui_retire') { isRetired = true; return final }
        if (command === 'ollama_ui_snapshot') return isRetired ? final : before
        throw new Error('Unexpected process-boundary command')
    } } }
}

test('successful original-process finalization is required before restart', async () => {
    const boundary = completedBoundary('SUCC'), state = createNativeControlState()
    await state.arm(boundary.driver, ticket, 'none')
    assert.throws(() => state.requireFinalizedBeforeRestart(), /before ordinary restart/)
    const evidence = await state.finalize('SUCC')
    state.requireFinalizedBeforeRestart()
    assert.equal(state.token, null)
    assert.deepEqual(evidence.beforeRetirement, boundary.before)
    assert.deepEqual(evidence.final, boundary.final)
})

test('unexpected late finalization events retain the original token for cleanup', async () => {
    const boundary = completedBoundary('SUCC', { late: true }), state = createNativeControlState()
    await state.arm(boundary.driver, ticket, 'none')
    await assert.rejects(() => state.finalize('SUCC'), /unexpected late native events/)
    assert.deepEqual(state.token, boundary.before.token)
    assert.equal(state.finalized, null)
    assert.throws(() => state.requireFinalizedBeforeRestart())
})

function heldBoundary(gate) {
    const base = snapshot(Object.keys(nativeGateCases).find(name => nativeGateCases[name] === gate))
    const index = base.events.findIndex(event => event.kind === 'gateEntered')
    let stopped = false, released = false, readProductCancellation = false
    const driver = {
        async invoke(command) {
            if (command === 'ollama_ui_arm') return base.token
            if (command === 'ollama_ui_release') { released = true; return { ...base, holds: 0 } }
            assert.equal(command, 'ollama_ui_snapshot')
            const events = base.events.slice(0, index + 1)
            if (stopped && gate === 'admitted') events.push(...base.events.slice(index + 1, base.events.findIndex(event => event.kind === 'cancelWaiting') + 1))
            return { ...base, events, drained: false, actors: 1, holds: 1 }
        },
        async stop() { stopped = true },
        async witness() {
            assert.notEqual(gate, 'admitted', 'Never await the product cancellation/status while admitted work holds the gate')
            readProductCancellation = true
            return { events: [{ command: 'native_generation_cancel', state: 'resolved', result: { ticket, phase: 'running', cancelDecision: 'user' } }] }
        },
    }
    return { driver, inspect: () => ({ stopped, released, readProductCancellation }) }
}

test('pre-admission permits Running with resolved user cancellation before release', async () => {
    const boundary = heldBoundary('preAdmission'), state = createNativeControlState()
    await state.arm(boundary.driver, ticket, 'preAdmission')
    const evidence = await cancelAtNativeGate(state, boundary.driver, ticket, 'preAdmission')
    assert.equal(evidence.preAdmissionCancellation.phase, 'running')
    assert.deepEqual(boundary.inspect(), { stopped: true, released: true, readProductCancellation: true })
})

test('admitted Stop waits native cancelWaiting and does not await product cancellation IPC', async () => {
    const boundary = heldBoundary('admitted'), state = createNativeControlState()
    await state.arm(boundary.driver, ticket, 'admitted')
    const evidence = await cancelAtNativeGate(state, boundary.driver, ticket, 'admitted')
    assert.equal(evidence.preAdmissionCancellation, null)
    assert.ok(state.last.events.some(event => event.kind === 'cancelWaiting'))
    assert.deepEqual(boundary.inspect(), { stopped: true, released: true, readProductCancellation: false })
})

for (const gate of ['credit', 'offer']) test('held ' + gate + ' cancellation never forces native release', async () => {
    const boundary = heldBoundary(gate), state = createNativeControlState()
    await state.arm(boundary.driver, ticket, gate)
    await cancelAtNativeGate(state, boundary.driver, ticket, gate)
    assert.deepEqual(boundary.inspect(), { stopped: true, released: false, readProductCancellation: false })
})

test('failure cleanup retires without awaiting, releases renderer and HTTP, then drains and closes', { timeout: 1000 }, async () => {
    const base = snapshot('CAN-CREDIT'), final = retired(base)
    let acknowledge, fixtureHeld = true, rendererHeld = true, retiring = false, drained = false, closed = false
    const state = createNativeControlState()
    await state.arm({ invoke(command) {
        if (command === 'ollama_ui_arm') return Promise.resolve(base.token)
        if (command === 'ollama_ui_retire') { retiring = true; return new Promise(resolve => { acknowledge = resolve }) }
        assert.equal(command, 'ollama_ui_snapshot'); assert.equal(fixtureHeld, false)
        drained = true; return Promise.resolve(final)
    } }, ticket, 'credit')
    await cleanupNativeControls(state, async () => { assert.equal(retiring, true); rendererHeld = false },
        () => { assert.equal(rendererHeld, false); fixtureHeld = false; acknowledge(final) },
        async () => { assert.equal(drained, true); closed = true })
    assert.equal(closed, true)
    assert.deepEqual(state.token, base.token, 'Failure cleanup does not turn a case into successful finalization')
})

test('renderer-release failure still releases HTTP, drains and attempts ordinary close', async () => {
    const boundary = completedBoundary('SUCC'), state = createNativeControlState()
    await state.arm(boundary.driver, ticket, 'none')
    let released = false, closed = false
    await assert.rejects(() => cleanupNativeControls(state, async () => { throw new Error('synthetic renderer release failure') },
        () => { released = true }, async () => { closed = true }), /renderer release failure/)
    assert.equal(released, true); assert.equal(closed, true)
})

test('control bridge remains inspect-only and preserves the product command ledger', async () => {
    const witness = installCommandWitness({ invoke: async () => null })
    witness.begin()
    for (const command of ['ollama_ui_arm', 'ollama_ui_snapshot', 'ollama_ui_release', 'ollama_ui_retire']) await witness.inspect(command, {})
    assert.deepEqual(witness.snapshot().events, [])
    await witness.invoke('native_generation_execute', {})
    assert.equal(witness.snapshot().events.length, 1)
    assert.throws(() => witness.inspect('native_generation_execute', {}), /inspection-command-refused/)
})

test('fresh-process stale-token refusal must be an actual native rejection', async () => {
    const driver = { evaluate: async () => ({ resolved: false, error: 'native-ui:stale-token' }) }
    const result = await verifyStaleNativeToken(driver, token('none'))
    assert.equal(result.length, 3)
    for (const wrong of [{ resolved: true }, { resolved: false, error: 'native-ui:controller-unavailable' }]) {
        await assert.rejects(() => verifyStaleNativeToken({ evaluate: async () => wrong }, token('none')))
    }
    await assert.rejects(() => verifyStaleNativeToken({ evaluate: async () => { throw new Error('synthetic CDP transport failure') } }, token('none')), /CDP transport failure/)
})

test('later native snapshots cannot replace an already observed stage', async () => {
    const expected = token('none'), state = createNativeControlState()
    let observations = 0
    await state.arm({ async invoke(command) {
        if (command === 'ollama_ui_arm') return expected
        return { token: expected, events: [{ sequence: 1, kind: observations++ === 0 ? 'requestFormatted' : 'bufferDecoded', result: null }],
            retired: false, drained: false, actors: 1, holds: 0, hooks: 0, invalid: null }
    } }, ticket, 'none')
    await state.snapshot()
    await assert.rejects(() => state.snapshot(), /observed trace prefix/)
    assert.deepEqual(state.token, expected)
})

test('ordinary binary scenarios cannot enter native gate choreography', async () => {
    await assert.rejects(() => runScenario({ name: 'CAN-CREDIT' }), /explicit test executable/)
})

test('uncommitted pre-admission can forward only rejectedCancelled after real resolution', () => {
    const value = snapshot('CAN-PREADMIT')
    value.events = value.events.filter(event => event.kind !== 'offerDropped')
    value.events.splice(value.events.findIndex(event => event.kind === 'outputResolved') + 1, 0,
        { kind: 'offerForwarded', result: 'rejectedCancelled' })
    renumber(value)
    verifyNativeTrace(value, value.token, 'CAN-PREADMIT')
    value.events.find(event => event.kind === 'offerForwarded').result = 'accepted'
    assert.throws(() => verifyNativeTrace(value, value.token, 'CAN-PREADMIT'), /forwarded offer/)
})

test('an offer cannot forward before its actual output resolution', () => {
    const value = snapshot('SUCC')
    const forwarded = value.events.find(event => event.kind === 'offerForwarded')
    value.events = value.events.filter(event => event !== forwarded)
    value.events.splice(value.events.findIndex(event => event.kind === 'storagePolicyApplied'), 0, forwarded)
    renumber(value)
    assert.throws(() => verifyNativeTrace(value, value.token, 'SUCC'), /precedes/)
})

for (const closeFails of [false, true]) test('failed native drain still attempts ordinary close, close failure=' + closeFails, async () => {
    const base = snapshot('SUCC'), state = createNativeControlState()
    let rendererReleased = false, fixtureReleased = false, closeAttempted = false
    await state.arm({ async invoke(command) {
        if (command === 'ollama_ui_arm') return base.token
        assert.equal(command, 'ollama_ui_retire')
        throw new Error('synthetic retire IPC failure')
    } }, ticket, 'none')
    await assert.rejects(() => cleanupNativeControls(state,
        async () => { rendererReleased = true },
        () => { assert.equal(rendererReleased, true); fixtureReleased = true },
        async () => { assert.equal(fixtureReleased, true); closeAttempted = true
            if (closeFails) throw new Error('synthetic ordinary close failure') }), error => {
        assert.equal(closeAttempted, true)
        assert.ok(error.nativeCleanup.some(value => value.stage === 'native-drain' && value.error.includes('retire IPC failure')))
        assert.equal(error.nativeCleanup.some(value => value.stage === 'ordinary-close' && value.error.includes('ordinary close failure')), closeFails)
        return true
    })
    assert.deepEqual(state.token, base.token)
    assert.equal(state.finalized, null)
})
