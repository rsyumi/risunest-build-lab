import { invoke } from '@tauri-apps/api/core'
import ollama from '../fixtures/native-generation/ollama-text.json'
import {
    nativeGenerationActive, nativeGenerationCancel, nativeGenerationCaptureWitness, nativeGenerationExecute,
    nativeGenerationFailureMessage, nativeGenerationHostInit, nativeGenerationProof, nativeGenerationReserveOllama,
    nativeGenerationStatus, type NativeGenerationStatus, type NativeGenerationTicket,
} from '../../src/ts/storage/nativeGeneration'

interface Phase { runId: string; phase: string; accountOrigin?: string }
interface FixtureState { requestCount: number; heldCount: number; abortedCount: number }

// 'generation-lost' crashes the renderer from the runner; 'generation-reload' starts a new document itself.
const generationPhases = new Set(['generation', 'generation-reload', 'generation-lost'])
export const isGenerationPhase = (phase: string): boolean => generationPhases.has(phase)

const characterId = 'character'
const conversationId = 'conversation'
const messagesKey = JSON.stringify(['messages', characterId, conversationId])
const effectKeys = [['root', 'statics'], ['character', characterId, 'lastInteraction'], ['conversation', characterId, conversationId, 'lastMemory']].map(key => JSON.stringify(key))
const terminal = (status: NativeGenerationStatus) => !['reserved', 'running'].includes(status.phase)

function check(condition: unknown, name: string): asserts condition {
    if (!condition) throw new Error(name)
}

async function until<T>(read: () => Promise<T>, done: (value: T) => boolean, name: string, limitMs = 60_000): Promise<T> {
    const deadline = Date.now() + limitMs
    for (;;) {
        const value = await read()
        if (done(value)) return value
        if (Date.now() > deadline) throw new Error(`${name} was not reached`)
        await new Promise(resolve => setTimeout(resolve, 20))
    }
}

function fixtureControl(origin: string) {
    const call = async (action: string, body?: unknown): Promise<FixtureState> => {
        const response = await fetch(`${origin}/__fixture/${action}`, action === 'state' ? {}
            : { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body ?? {}) })
        if (!response.ok) throw new Error(`fixture ${action} returned ${response.status}`)
        return await response.json() as FixtureState
    }
    return {
        state: () => call('state'),
        hold: () => call('hold'),
        release: () => call('release'),
        reset: () => call('reset'),
        reply: (next: unknown) => call('reply', next),
        until: (done: (state: FixtureState) => boolean, name: string) => until(() => call('state'), done, name),
    }
}

async function seed(origin: string): Promise<void> {
    const messages = structuredClone(ollama.messages) as Record<string, unknown>[]
    // A row without an ID gives the proof a normalization to report.
    delete messages[2].chatId
    const character = { ...ollama.character, chats: [{ ...ollama.conversation, message: messages }] }
    const { stagingId } = await invoke<{ stagingId: string }>('pds_replace_begin')
    await invoke('pds_replace_put_root', { stagingId, root: { ...ollama.root, ollamaURL: origin } })
    await invoke('pds_replace_put_presets', { stagingId, presets: [ollama.preset] })
    await invoke('pds_replace_add_characters', { stagingId, characters: [character] })
    await invoke('pds_replace_commit', { stagingId, expectedRevision: 0 })
}

const revision = async () => (await invoke<{ revision: number }>('pds_read_root', { lease: null })).revision
const lease = async (at: number) => ({ token: (await invoke<{ lease: string }>('pds_acquire_revision', { revision: at })).lease, revision: at })
const messages = async () =>
    (await invoke<{ value: { message: { data: string; chatId?: string }[] } }>('pds_read_conversation', { characterId, conversationId, lease: null })).value.message

async function reserve(): Promise<NativeGenerationTicket> {
    const { targetAuthority } = await invoke<{ targetAuthority: string }>('pds_lww_binding_state')
    const reservation = await nativeGenerationReserveOllama({
        characterId, conversationId, expectedRevision: await revision(),
        libraryIdentity: await invoke<string>('pds_read_library_identity'),
        bindingAuthority: targetAuthority, effectivePresetId: ollama.preset.id,
    })
    if (!('ticket' in reservation)) throw new Error(`reservation refused for ${reservation.unsupported}`)
    return reservation.ticket
}

// The first read answers at once; later reads wait for a change, briefly, since one between reads waits for the next.
function settled(ticket: NativeGenerationTicket, done: (status: NativeGenerationStatus) => boolean = terminal) {
    let waitMs: number | undefined
    return until(async () => {
        const status = await nativeGenerationStatus(ticket, waitMs)
        waitMs = 1_000
        return status
    }, done, 'native generation status')
}
const issued = (ticket: NativeGenerationTicket) => settled(ticket, status => status.dispatch === 'issued' || terminal(status))

async function proofAtIssued(fixture: ReturnType<typeof fixtureControl>): Promise<void> {
    await fixture.hold()
    const original = await lease(await revision())
    const ticket = await reserve()
    const witness = await nativeGenerationCaptureWitness(ticket, original, [2])
    check(witness.rows.length === 1 && witness.rows[0].idState === 'missing', 'witness of the row without an ID')
    await nativeGenerationExecute(ticket)
    await fixture.until(state => state.heldCount === 1, 'held provider request')
    check((await issued(ticket)).phase === 'running', 'running at issue')
    const current = await lease(await revision())
    const proof = await nativeGenerationProof(ticket, { witness, original, current })
    const assigned = (await messages())[2].chatId
    check(proof.at === 'issued' && proof.outputMessageId === null && proof.receipts.output === null, 'issued proof has no output')
    check(JSON.stringify(proof.affectedUnitKeys) === JSON.stringify([...effectKeys, messagesKey]), 'issued proof names the exact pre-dispatch units')
    check(proof.receipts.effects.length > 0, 'issued proof carries verified effects')
    check(proof.receipts.normalization !== null && !proof.mappingUnavailable, 'issued proof carries the normalization')
    check(typeof assigned === 'string' && JSON.stringify(proof.idDeltas?.map(delta => (delta as { assignedId: string }).assignedId)) === JSON.stringify([assigned]), 'assigned ID delta')
    await fixture.release()
    const status = await settled(ticket)
    const final = await nativeGenerationProof(ticket)
    check(status.phase === 'completed' && final.at === 'completed' && final.outputMessageId === status.messageId, 'terminal proof names the output')
    check(JSON.stringify(final.affectedUnitKeys) === JSON.stringify([...effectKeys, messagesKey]) && final.receipts.effects.length > 0, 'terminal proof carries the exact effect units and receipts')
    check((await fixture.state()).requestCount === 1, 'one provider request')
}

async function successOnce(fixture: ReturnType<typeof fixtureControl>): Promise<void> {
    await fixture.reset()
    await fixture.reply({ content: ' Boundary reply. ' })
    const ticket = await reserve()
    const [first, second] = await Promise.all([nativeGenerationExecute(ticket), nativeGenerationExecute(ticket)])
    check(first.characterId === characterId && second.conversationId === conversationId, 'status names the target')
    const status = await settled(ticket)
    check(status.phase === 'completed' && status.output === 'committed' && status.failure === null, 'completed with output')
    const row = (await messages()).at(-1)
    check(row?.data === 'Boundary reply.' && row.chatId === status.messageId, 'output row stored')
    check((await fixture.state()).requestCount === 1, 'two executes send one request')
    check((await nativeGenerationActive()).length === 0, 'nothing active after completion')
}

async function providerError(fixture: ReturnType<typeof fixtureControl>): Promise<void> {
    await fixture.reset()
    await fixture.reply({ status: 500, body: { error: 'Synthetic failure' } })
    const ticket = await reserve()
    await nativeGenerationExecute(ticket)
    const status = await settled(ticket)
    check(status.phase === 'failed' && status.output === 'none', 'provider failure without output')
    check(status.failure?.category === 'http' && status.failure.httpStatus === 500, 'http failure')
    check(nativeGenerationFailureMessage(status.failure) === 'Synthetic failure', 'TS route text')
    check((await fixture.state()).requestCount === 1, 'one provider request')
}

// No reset follows, so the next case's single request also proves this one sent none.
async function cancelBeforeExecute(fixture: ReturnType<typeof fixtureControl>): Promise<void> {
    await fixture.reset()
    const ticket = await reserve()
    check((await nativeGenerationCancel(ticket, 'user')).phase === 'cancelled', 'cancelled before execute')
    const status = await nativeGenerationExecute(ticket)
    check(status.phase === 'cancelled' && status.dispatch === 'none', 'execute keeps the cancel')
}

async function cancelAfterDispatch(fixture: ReturnType<typeof fixtureControl>): Promise<void> {
    await fixture.hold()
    const ticket = await reserve()
    await nativeGenerationExecute(ticket)
    await fixture.until(state => state.heldCount === 1, 'held provider request')
    await issued(ticket)
    check((await nativeGenerationCancel(ticket, 'user')).cancelDecision === 'user', 'cancel recorded')
    const status = await settled(ticket)
    check(status.phase === 'cancelled' && status.cancelDecision === 'user' && status.output === 'none', 'cancelled during the provider wait')
    const state = await fixture.until(value => value.abortedCount === 1, 'provider request dropped')
    check(state.requestCount === 1, 'one provider request across both cancel cases')
    await fixture.release()
}

async function unknownTicket(): Promise<void> {
    const ticket = await reserve()
    let error: unknown
    try { await nativeGenerationStatus({ epoch: ticket.epoch, sequence: '999999' }) } catch (failure) { error = failure }
    check(JSON.stringify(error) === JSON.stringify({ code: 'validation', message: 'native-generation:unknown-ticket' }), 'unknown ticket error')
    await nativeGenerationCancel(ticket, 'user')
}

/** A new document finds the operation the lost one started, which then finishes with its one request. */
async function afterLoss(fixture: ReturnType<typeof fixtureControl>, cases: string[]): Promise<void> {
    const active = await nativeGenerationActive()
    check(active.length === 1 && active[0].phase === 'running' && active[0].characterId === characterId
        && active[0].conversationId === conversationId, 'the new document lists the operation')
    const state = await fixture.state()
    check(state.requestCount === 1 && state.heldCount === 1 && state.abortedCount === 0, 'the provider request outlived the document')
    cases.push('listed-after-loss')
    await fixture.release()
    const status = await settled(active[0].ticket)
    check(status.phase === 'completed' && status.cancelDecision === null, 'completed after the loss')
    check((await messages()).at(-1)?.chatId === status.messageId, 'output row stored')
    check((await fixture.state()).requestCount === 1, 'no second provider request')
    cases.push('resolved-once')
}

/** Returns true when the document hands the phase to a later document instead of finishing it. */
export async function runGenerationPhase(phase: Phase, openedRevision: number | undefined, cases: string[]): Promise<boolean> {
    check(phase.accountOrigin, 'fixture origin')
    const fixture = fixtureControl(phase.accountOrigin)
    await nativeGenerationHostInit(navigator.userAgent)
    if (phase.phase !== 'generation' && openedRevision !== 0) {
        await afterLoss(fixture, cases)
        return false
    }
    await fixture.reset()
    await seed(phase.accountOrigin)
    if (phase.phase === 'generation') {
        await proofAtIssued(fixture)
        cases.push('proof-at-issued')
        await successOnce(fixture)
        cases.push('success-once')
        await providerError(fixture)
        cases.push('provider-error')
        await cancelBeforeExecute(fixture)
        cases.push('cancel-before-execute')
        await cancelAfterDispatch(fixture)
        cases.push('cancel-after-dispatch')
        await unknownTicket()
        cases.push('unknown-ticket')
        return false
    }
    await fixture.hold()
    const ticket = await reserve()
    await nativeGenerationExecute(ticket)
    await fixture.until(state => state.heldCount === 1, 'held provider request')
    await issued(ticket)
    if (phase.phase === 'generation-lost') await invoke('boundary_ready')
    else location.reload()
    return true
}
