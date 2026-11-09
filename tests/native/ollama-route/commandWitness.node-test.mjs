import { test } from 'node:test'
import assert from 'node:assert/strict'
import { installCommandWitness, hasStatusAfterIssuedProof } from './commandWitness.mjs'

const boundary = handler => {
    const internals = {}
    Object.defineProperty(internals, 'invoke', { value: handler })
    return { internals, witness: installCommandWitness(internals) }
}

test('the witness preserves a readonly native invoke and keeps setup/readback out of product evidence', async () => {
    const result = { phase: 'running', ticket: { epoch: '1', sequence: '2' } }
    const invoke = async () => result
    const { internals, witness } = boundary(invoke)
    witness.begin()
    assert.equal(await witness.inspect('native_generation_status', { ticket: result.ticket }), result)
    assert.deepEqual(witness.snapshot().events, [])
    assert.throws(() => witness.inspect('native_generation_execute', {}), /inspection-command-refused/)
    assert.equal(await witness.invoke('native_generation_execute', { ticket: result.ticket }), result)
    assert.equal(internals.invoke, invoke)
    assert.equal(witness.snapshot().events[0].command, 'native_generation_execute')
    assert.deepEqual(witness.snapshot().events[0].result, result)
    const copy = witness.snapshot()
    copy.events[0].result.phase = 'tampered'
    assert.equal(witness.end().events[0].result.phase, 'running')
})

test('before-execute holds the real witness reply until the driver releases it', async () => {
    const nativeReply = { rows: [{ rowHash: 'synthetic-private-witness' }] }
    const { witness } = boundary(async () => nativeReply)
    witness.begin()
    witness.arm('before-execute')
    let returned = false
    const pending = witness.invoke('native_generation_capture_witness', {}).then(value => { returned = true; return value })
    await Promise.resolve()
    assert.equal(witness.snapshot().blocked, 'before-execute')
    assert.equal(returned, false)
    assert.equal(JSON.stringify(witness.snapshot()).includes('synthetic-private-witness'), false)
    assert.throws(() => witness.end(), /unresolved-barrier/)
    witness.resume()
    assert.equal(await pending, nativeReply)
    assert.equal(witness.snapshot().blocked, null)
    witness.end()
})

test('terminal-proof passes Issued evidence and holds only committed terminal output before caller adoption', async () => {
    const issued = { at: 'issued', receipts: { output: null } }
    const terminal = { at: 'completed', receipts: { output: { revision: 9 } } }
    let next = issued
    const { witness } = boundary(async () => next)
    witness.begin()
    witness.arm('terminal-proof')
    assert.equal(await witness.invoke('native_generation_proof', {}), issued)
    assert.equal(witness.snapshot().blocked, null)
    next = terminal
    let returned = false
    const pending = witness.invoke('native_generation_proof', {}).then(value => { returned = true; return value })
    await Promise.resolve()
    assert.equal(witness.snapshot().blocked, 'terminal-proof')
    assert.equal(returned, false)
    assert.deepEqual(witness.snapshot().events.at(-1).result, terminal)
    witness.resume()
    assert.equal(await pending, terminal)
    witness.end()
})

test('a native rejection reaches the product caller and cannot become a resolved witness', async () => {
    const failure = new Error('synthetic native failure')
    const { witness } = boundary(async () => { throw failure })
    witness.begin()
    await assert.rejects(witness.invoke('native_generation_execute', {}), error => error === failure)
    const event = witness.end().events[0]
    assert.equal(event.command, 'native_generation_execute')
    assert.equal(event.state, 'rejected')
    assert.equal(event.result, null)
    assert.ok(event.completionOrder > event.invocationOrder)
})


test('status readiness requires invocation after the Issued proof completes', async () => {
    const proof = Promise.withResolvers()
    const earlyStatus = Promise.withResolvers()
    const laterStatus = Promise.withResolvers()
    const replies = [earlyStatus, laterStatus]
    const { witness } = boundary(command => command === 'native_generation_proof' ? proof.promise : replies.shift().promise)
    witness.begin()
    const pendingProof = witness.invoke('native_generation_proof', {})
    const pendingEarlyStatus = witness.invoke('native_generation_status', {})
    assert.equal(hasStatusAfterIssuedProof(witness.snapshot()), false)
    proof.resolve({ at: 'issued', receipts: { output: null } })
    await pendingProof
    assert.equal(hasStatusAfterIssuedProof(witness.snapshot()), false,
        'A status invoked during the proof does not establish post-proof readiness')
    const pendingLaterStatus = witness.invoke('native_generation_status', {})
    assert.equal(hasStatusAfterIssuedProof(witness.snapshot()), true,
        'A later pending status establishes invocation order without waiting for its reply')
    earlyStatus.resolve({ phase: 'running', dispatch: 'issued' })
    laterStatus.resolve({ phase: 'running', dispatch: 'issued' })
    await Promise.all([pendingEarlyStatus, pendingLaterStatus])
    assert.equal(hasStatusAfterIssuedProof(witness.end()), true)
})

test('a rejected proof cannot establish status readiness', async () => {
    const proof = Promise.withResolvers()
    const { witness } = boundary(command => command === 'native_generation_proof'
        ? proof.promise : Promise.resolve({ phase: 'running', dispatch: 'issued' }))
    witness.begin()
    const pendingProof = witness.invoke('native_generation_proof', {})
    const rejected = assert.rejects(pendingProof, /synthetic proof failure/)
    proof.reject(new Error('synthetic proof failure'))
    await rejected
    await witness.invoke('native_generation_status', {})
    assert.equal(hasStatusAfterIssuedProof(witness.end()), false)
})
