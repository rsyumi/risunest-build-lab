import assert from 'node:assert/strict'
import { setTimeout as delay } from 'node:timers/promises'

export const nativeGateCases = Object.freeze({
    'CAN-CREDIT': 'credit', 'CAN-OFFER': 'offer',
    'CAN-PREADMIT': 'preAdmission', 'CAN-ADMITTED': 'admitted',
})
export const nativeEntryScenarios = ['SUCC', ...Object.keys(nativeGateCases)]
const stages = ['requestFormatted', 'bufferDecoded', 'jsonParsed', 'responseTransformed', 'storagePolicyApplied']
const results = {
    creditForwarded: ['ok', 'cancelled', 'failed'],
    offerForwarded: ['accepted', 'rejectedCancelled', 'rejectedFailed', 'unknown'],
    outputResolved: ['committed', 'uncommitted', 'unresolved', 'error'],
    ownerCompleted: ['completed', 'cancelled', 'failed', 'stale', 'interrupted', 'running', 'error'],
}
const boundaries = ['creditEntered', 'creditDelegated', 'creditForwarded', 'creditDropped',
    'offerEntered', 'offerDelegated', 'offerForwarded', 'offerDropped', 'outputSegmentEntered',
    'outputAdmitted', 'cancelWaiting', 'outputResolved', 'ownerCompleted', 'gateEntered',
    'gateReleased', 'gateDropped', 'retired']
const kinds = new Set([...stages, ...boundaries])
const clone = value => structuredClone(value)

export function verifyNativeToken(token, ticket, gate) {
    assert.deepEqual(Object.keys(token).sort(), ['boot', 'gate', 'generation', 'ticket'])
    assert.equal(typeof token.boot, 'string')
    assert.ok(token.boot.length > 0 && token.boot.length <= 128)
    assert.ok(Number.isSafeInteger(token.generation) && token.generation > 0)
    assert.deepEqual(token.ticket, ticket)
    assert.equal(token.gate, gate)
}

export function verifyNativeSnapshot(snapshot, token, { cleanup = false } = {}) {
    assert.deepEqual(snapshot.token, token, 'Native observation belongs to this exact boot/session/ticket/gate')
    for (const key of ['retired', 'drained']) assert.equal(typeof snapshot[key], 'boolean')
    for (const key of ['actors', 'holds', 'hooks']) assert.ok(Number.isSafeInteger(snapshot[key]) && snapshot[key] >= 0)
    assert.ok(snapshot.invalid === null || typeof snapshot.invalid === 'string')
    if (!cleanup) assert.equal(snapshot.invalid, null, 'Native session remains valid')
    assert.ok(Array.isArray(snapshot.events) && snapshot.events.length <= 128)
    const seen = new Set()
    for (const [index, event] of snapshot.events.entries()) {
        assert.deepEqual(Object.keys(event).sort(), ['kind', 'result', 'sequence'])
        assert.equal(event.sequence, index + 1, 'Contiguous native event sequence')
        assert.ok(kinds.has(event.kind), 'Fixed native observation kind')
        assert.ok(!seen.has(event.kind), 'Single buffered output has no duplicate native observation')
        seen.add(event.kind)
        if (results[event.kind]) assert.ok(results[event.kind].includes(event.result), 'Typed native boundary result')
        else assert.equal(event.result, null, 'Native observation has no caller-defined result')
    }
    return snapshot
}

export function verifyNativeTrace(snapshot, token, name) {
    verifyNativeSnapshot(snapshot, token)
    assert.equal(snapshot.retired, false, 'Validate original trace before retirement')
    assert.deepEqual([snapshot.actors, snapshot.holds, snapshot.hooks], [0, 0, 0], 'Original native actors and hooks have drained')
    const gate = nativeGateCases[name] ?? 'none'
    assert.equal(token.gate, gate)
    const events = new Map(snapshot.events.map(event => [event.kind, event]))
    const has = kind => events.has(kind)
    const need = kind => { assert.ok(has(kind), 'Missing native observation: ' + kind); return events.get(kind) }
    const absent = kind => assert.equal(has(kind), false, 'Unexpected native observation: ' + kind)
    const before = (a, b) => assert.ok(need(a).sequence < need(b).sequence, a + ' precedes ' + b)
    for (const kind of stages.slice(0, 4)) need(kind)
    for (let i = 1; i < 4; i++) before(stages[i - 1], stages[i])
    before('responseTransformed', 'creditEntered')
    need('ownerCompleted')
    assert.equal(events.get('ownerCompleted').result, name === 'SUCC' ? 'completed' : 'cancelled')
    absent('retired')
    assert.equal(snapshot.events.at(-1)?.kind, 'ownerCompleted', 'Owner completion is the final original event')
    if (gate === 'none') {
        for (const kind of ['gateEntered', 'gateReleased', 'gateDropped', 'cancelWaiting', 'creditDropped', 'offerDropped']) absent(kind)
    } else {
        need('gateEntered')
        if (gate === 'credit' || gate === 'offer') {
            need('gateDropped'); absent('gateReleased')
            before('gateEntered', 'gateDropped')
            before('gateDropped', gate === 'credit' ? 'creditDropped' : 'offerDropped')
        } else {
            need('gateReleased'); absent('gateDropped')
            before('gateEntered', 'gateReleased')
        }
    }
    if (gate === 'credit') {
        before('creditEntered', 'gateEntered'); need('creditDropped')
        for (const kind of ['creditDelegated', 'creditForwarded', 'offerEntered', 'offerDelegated',
            'offerForwarded', 'offerDropped', 'storagePolicyApplied', 'outputSegmentEntered',
            'outputAdmitted', 'outputResolved', 'cancelWaiting']) absent(kind)
    } else {
        before('creditEntered', 'creditDelegated'); before('creditDelegated', 'creditForwarded')
        assert.equal(events.get('creditForwarded').result, 'ok')
        absent('creditDropped'); before('creditForwarded', 'offerEntered')
        if (gate === 'offer') {
            before('offerEntered', 'gateEntered'); need('offerDropped')
            for (const kind of ['offerDelegated', 'offerForwarded', 'storagePolicyApplied', 'outputSegmentEntered',
                'outputAdmitted', 'outputResolved', 'cancelWaiting']) absent(kind)
        } else {
            before('offerEntered', 'offerDelegated'); before('offerDelegated', 'storagePolicyApplied')
            before('storagePolicyApplied', 'outputSegmentEntered')
            need('outputResolved')
            assert.equal(events.get('outputResolved').result, gate === 'preAdmission' ? 'uncommitted' : 'committed')
            before('outputSegmentEntered', 'outputResolved'); before('outputResolved', 'ownerCompleted')
            assert.notEqual(has('offerForwarded'), has('offerDropped'), 'Delegated offer has one actual completion or drop')
            if (has('offerForwarded')) {
                before('offerDelegated', 'offerForwarded')
                before('outputResolved', 'offerForwarded')
                assert.equal(events.get('offerForwarded').result, gate === 'preAdmission' ? 'rejectedCancelled' : 'accepted',
                    'Actual forwarded offer follows its committed or cancelled resolution')
            }
            if (has('offerDropped')) before('offerDelegated', 'offerDropped')
            if (gate === 'preAdmission') {
                before('outputSegmentEntered', 'gateEntered'); before('gateReleased', 'outputResolved')
                absent('outputAdmitted'); absent('cancelWaiting')
            } else {
                before('outputSegmentEntered', 'outputAdmitted'); before('outputAdmitted', 'outputResolved')
                if (gate === 'admitted') {
                    before('outputAdmitted', 'gateEntered'); before('gateEntered', 'cancelWaiting')
                    before('cancelWaiting', 'gateReleased'); before('gateReleased', 'outputResolved')
                } else {
                    need('offerForwarded'); assert.equal(events.get('offerForwarded').result, 'accepted')
                }
            }
        }
    }
    return snapshot
}

export function verifyNativeGateOutcome(name, ticket, status, proof, issuedProof) {
    assert.ok(nativeEntryScenarios.includes(name))
    const output = name === 'SUCC' || name === 'CAN-ADMITTED'
    assert.deepEqual(status.ticket, ticket)
    assert.deepEqual(proof.ticket, ticket)
    assert.equal(status.phase, name === 'SUCC' ? 'completed' : 'cancelled')
    if (name !== 'SUCC') assert.equal(status.cancelDecision, 'user')
    assert.equal(status.dispatch, 'issued')
    assert.equal(status.pendingCommit, false)
    assert.equal(status.output, output ? 'committed' : 'none')
    assert.equal(status.checkpoint, output ? '1' : '0')
    assert.equal(proof.at, status.phase)
    assert.equal(proof.failedWithOutput, false, 'User cancellation is never Failed-with-output')
    assert.deepEqual(proof.receipts.effects, issuedProof.receipts.effects, 'Issued effect receipts survive exactly once')
    assert.ok(proof.receipts.effects.length > 0)
    if (output) {
        assert.equal(proof.outputMessageId, status.messageId)
        assert.ok(proof.receipts.output, 'Real committed output receipt')
        assert.equal(proof.receipts.output.sourceSequence, '1')
        assert.ok(proof.receipts.output.receiptRevision > Math.max(...proof.receipts.effects.map(value => value.receiptRevision)))
    } else {
        assert.equal(proof.outputMessageId, null)
        assert.equal(proof.receipts.output, null)
    }
}

async function poll(read, accepts, label) {
    const deadline = Date.now() + 45_000
    for (;;) {
        const value = await read()
        if (accepts(value)) return value
        if (Date.now() >= deadline) throw new Error('ollama-ui:timeout:' + label)
        await delay(50)
    }
}

function acceptSnapshot(state, value, cleanup = false) {
    const previous = state.last
    state.last = clone(value)
    verifyNativeSnapshot(value, state.token, { cleanup })
    if (previous && !cleanup) {
        assert.deepEqual(value.events.slice(0, previous.events.length), previous.events, 'Native snapshots preserve the observed trace prefix')
        if (previous.retired) assert.equal(value.retired, true, 'Native retirement cannot reverse')
    }
    return value
}

export function createNativeControlState() {
    return {
        token: null, driver: null, last: null, finalized: null, retirement: null,
        async arm(driver, ticket, gate) {
            assert.equal(this.token, null)
            assert.equal(this.finalized, null)
            this.driver = driver
            const token = await driver.invoke('ollama_ui_arm', { ticket, gate })
            verifyNativeToken(token, ticket, gate)
            this.token = clone(token)
            return clone(token)
        },
        async snapshot(cleanup = false) {
            assert.ok(this.token, 'Native token remains owned in the original process')
            const value = await this.driver.invoke('ollama_ui_snapshot', { token: this.token })
            return acceptSnapshot(this, value, cleanup)
        },
        waitFor(kind) {
            return poll(() => this.snapshot(), value => value.events.some(event => event.kind === kind), kind)
        },
        async release() {
            assert.ok(this.token)
            const before = await this.snapshot()
            assert.ok(before.events.some(event => event.kind === 'gateEntered'))
            assert.ok(before.holds > 0)
            assert.ok(!before.events.some(event => ['gateReleased', 'gateDropped'].includes(event.kind)))
            const value = await this.driver.invoke('ollama_ui_release', { token: this.token })
            return acceptSnapshot(this, value)
        },
        initiateRetirement() {
            if (!this.token) return Promise.resolve({ ok: true, value: null })
            this.retirement ??= this.driver.invoke('ollama_ui_retire', { token: this.token })
                .then(value => ({ ok: true, value }), error => ({ ok: false, error: String(error) }))
            return this.retirement
        },
        async drain(retirement, cleanup = false) {
            if (!this.token) return null
            const result = await retirement
            assert.equal(result.ok, true, result.error ?? 'Native retirement acknowledged')
            acceptSnapshot(this, result.value, cleanup)
            assert.equal(result.value.retired, true)
            return poll(() => this.snapshot(cleanup), value => value.retired && value.drained
                && value.actors === 0 && value.holds === 0 && value.hooks === 0, 'native actor and hook drain')
        },
        async finalize(name) {
            const token = clone(this.token)
            const before = await poll(() => this.snapshot(), value => value.drained
                && value.events.some(event => event.kind === 'ownerCompleted'), 'native owner completion')
            verifyNativeTrace(before, token, name)
            const retirement = this.initiateRetirement()
            const final = await this.drain(retirement)
            assert.deepEqual(final.events, [...before.events, { sequence: before.events.length + 1, kind: 'retired', result: null }],
                'No unexpected late native events after successful retirement')
            this.finalized = { token, beforeRetirement: clone(before), retirement: clone((await retirement).value), final: clone(final) }
            this.token = null
            return clone(this.finalized)
        },
        requireFinalizedBeforeRestart() {
            assert.equal(this.token, null, 'Original native token finalized before ordinary restart')
            assert.ok(this.finalized, 'Final original-process trace retained before restart')
        },
        evidence() { return clone({ token: this.token, last: this.last, finalized: this.finalized }) },
    }
}

export async function verifyStaleNativeToken(driver, token) {
    const results = []
    for (const command of ['ollama_ui_snapshot', 'ollama_ui_release', 'ollama_ui_retire']) {
        const result = await driver.evaluate(`(async () => {
            try {
                await globalThis.__risunestOllamaUi.witness.inspect(${JSON.stringify(command)}, {token:${JSON.stringify(token)}})
                return { resolved: true }
            } catch (error) { return { resolved: false, error: String(error) } }
        })()`)
        assert.deepEqual(result, { resolved: false, error: 'native-ui:stale-token' }, 'Fresh native process refuses the original token')
        results.push({ command, ...result })
    }
    return results
}

export async function cancelAtNativeGate(state, driver, ticket, gate) {
    const gateObservation = await state.waitFor('gateEntered')
    assert.ok(gateObservation.holds > 0, 'Actual native gate is held before UI Stop')
    await driver.stop()
    let preAdmissionCancellation = null
    if (gate === 'preAdmission') {
        const witness = await poll(() => driver.witness(), value => value.events.some(event =>
            event.command === 'native_generation_cancel' && event.state === 'resolved'
            && event.result?.ticket?.epoch === ticket.epoch && event.result.ticket.sequence === ticket.sequence),
            'resolved real pre-admission cancellation')
        preAdmissionCancellation = witness.events.find(event => event.command === 'native_generation_cancel'
            && event.state === 'resolved' && event.result?.ticket?.epoch === ticket.epoch
            && event.result.ticket.sequence === ticket.sequence).result
        assert.equal(preAdmissionCancellation.cancelDecision, 'user')
        assert.ok(['running', 'cancelled'].includes(preAdmissionCancellation.phase))
        await state.release()
    } else if (gate === 'admitted') {
        await state.waitFor('cancelWaiting')
        await state.release()
    }
    return { gateObservation, preAdmissionCancellation }
}

export async function cleanupNativeControls(state, releaseRenderer, releaseFixture, close) {
    if (!state) {
        await releaseRenderer()
        releaseFixture()
        await close()
        return
    }
    const failures = []
    const actualErrors = []
    const failed = (stage, error) => {
        actualErrors.push(error)
        failures.push({ stage, error: String(error).slice(0, 1024) })
    }
    let retirement
    try { retirement = state.initiateRetirement() } catch (error) { failed('native-retire-initiation', error) }
    try { await releaseRenderer() } catch (error) { failed('renderer-release', error) }
    try { releaseFixture() } catch (error) { failed('fixture-release', error) }
    try {
        if (state.token && retirement) await state.drain(retirement, true)
    } catch (error) { failed('native-drain', error) }
    finally {
        try { await close() } catch (error) { failed('ordinary-close', error) }
    }
    if (failures.length) {
        const error = new AggregateError(actualErrors, 'Native verification cleanup failed: '
            + failures.map(value => value.stage + ': ' + value.error).join('; '))
        error.nativeCleanup = failures
        throw error
    }
}
