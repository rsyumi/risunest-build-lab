import { describe, expect, it } from 'vitest'
import { Mutex } from '../mutex'
import type { WorkingSetCommit } from './persistentDataStore'
import type { PreparedUnitIntent } from './saveCoordinator'
import {
    NativeGenerationGuard,
    touchesNativeGenerationTarget,
    type NativeGenerationFenceEvent,
    type NativeGenerationGuardCancelReason,
    type NativeGenerationGuardHooks,
    type NativeGenerationGuardPhase,
    type NativeGenerationGuardStatus,
    type NativeGenerationGuardTicket,
    type NativeGenerationTargetWrite,
} from './nativeGenerationGuard'

const key = (...parts: string[]) => JSON.stringify(parts)
const ticket = (sequence: string): NativeGenerationGuardTicket => ({ epoch: 'epoch-1', sequence })
const target = { characterId: 'char-a', conversationId: 'chat-a' }
const status = (
    sequence: string,
    phase: NativeGenerationGuardPhase,
    output: NativeGenerationGuardStatus['output'] = 'none',
    pendingCommit = false,
): NativeGenerationGuardStatus => ({ ticket: ticket(sequence), phase, output, pendingCommit })
const targetEdit: NativeGenerationTargetWrite = { unitMutations: [{ key: key('messages', 'char-a', 'chat-a') }] }
const otherConversationEdit: NativeGenerationTargetWrite = { unitMutations: [{ key: key('messages', 'char-a', 'chat-b') }] }

function manualClock() {
    let now = 0
    let next = 1
    const timers = new Map<number, { at: number; callback: () => void }>()
    return {
        clock: {
            setTimeout: (callback: () => void, delay: number) => {
                const id = next++
                timers.set(id, { at: now + delay, callback })
                return id
            },
            clearTimeout: (handle: unknown) => { timers.delete(handle as number) },
        },
        advance(ms: number) {
            now += ms
            for (const [id, timer] of [...timers].sort((a, b) => a[1].at - b[1].at)) {
                if (timer.at > now) continue
                timers.delete(id)
                timer.callback()
            }
        },
        pending: () => timers.size,
    }
}

async function drain(): Promise<void> {
    for (let index = 0; index < 100; index++) await Promise.resolve()
}

function track<T>(promise: Promise<T>) {
    const state: { settled: boolean; value?: T } = { settled: false }
    void promise.then((value) => {
        state.settled = true
        state.value = value
    })
    return state
}

function createGuard(hooks: Partial<NativeGenerationGuardHooks<NativeGenerationGuardStatus>> = {}) {
    const cancels: [NativeGenerationGuardTicket, NativeGenerationGuardCancelReason][] = []
    const fences: NativeGenerationFenceEvent<NativeGenerationGuardStatus>[] = []
    const adoptions: NativeGenerationGuardStatus[] = []
    const errors: unknown[] = []
    const clock = manualClock()
    const guard = new NativeGenerationGuard({
        cancel: (value, reason) => { cancels.push([value, reason]) },
        adopt: async (value) => { adoptions.push(value) },
        fence: (event) => { fences.push(event) },
        onError: (error) => { errors.push(error) },
        clock: clock.clock,
        ...hooks,
    })
    return { guard, cancels, fences, adoptions, errors, clock }
}

describe('native generation guard', () => {
    it('classifies writes that reach the target conversation, its character record or effect keys', () => {
        const effects = [key('root', 'statics'), key('character', 'char-a', 'lastInteraction'), key('conversation', 'char-a', 'chat-a', 'lastMemory')]
        const touches = (write: NativeGenerationTargetWrite) => touchesNativeGenerationTarget(write, target, effects)
        const commit: WorkingSetCommit = {
            expectedRevision: 4,
            unitMutations: [{ key: key('conversation', 'char-a', 'chat-a', 'name'), type: 'set', value: 'renamed' }],
        }
        const pluginIntent: PreparedUnitIntent = {
            unitMutations: [],
            conversations: [{ type: 'replace-range', characterId: 'char-a', conversationId: 'chat-a', start: 0, deleteCount: 1, messages: [] }],
        }
        const targetWrites: NativeGenerationTargetWrite[] = [
            commit,
            pluginIntent,
            targetEdit,
            { unitMutations: [{ key: key('exists', 'conversation', 'char-a', 'chat-a') }] },
            { unitMutations: [{ key: key('exists', 'character', 'char-a') }] },
            { unitMutations: [{ key: key('character', 'char-a', 'lastInteraction') }] },
            { unitMutations: [{ key: '["messages","char-a"' }] },
            { rootMutations: [{ key: 'statics' }] },
            { root: {} },
            { character: { chaId: 'char-a' } },
            { characterDetails: [{ chaId: 'char-a' }] },
            { conversations: [{ type: 'delete', characterId: 'char-a', conversationId: 'chat-a' }] },
            { replaceCharacter: { chaId: 'char-a' } },
            { addCharacter: { chaId: 'char-a' } },
            { deleteCharacterIds: ['char-a'] },
        ]
        for (const write of targetWrites) expect(touches(write), JSON.stringify(write)).toBe(true)
        const unrelatedWrites: NativeGenerationTargetWrite[] = [
            otherConversationEdit,
            { unitMutations: [{ key: key('messages', 'char-b', 'chat-a') }] },
            { unitMutations: [{ key: key('conversation', 'char-a', 'chat-b', 'lastMemory') }] },
            { unitMutations: [{ key: key('exists', 'conversation', 'char-a', 'chat-b') }] },
            { unitMutations: [{ key: key('character', 'char-a', 'name') }] },
            { unitMutations: [{ key: key('character', 'char-b', 'lastInteraction') }] },
            { unitMutations: [{ key: key('order', 'conversations', 'char-a') }] },
            { unitMutations: [{ key: key('variable', 'mood') }, { key: key('record', 'plugins', 'helper') }] },
            { rootMutations: [{ key: 'username' }] },
            { conversations: [{ type: 'reorder', characterId: 'char-a' }] },
            { conversations: [{ type: 'replace-range', characterId: 'char-a', conversationId: 'chat-b' }] },
            { replaceCharacter: { chaId: 'char-b' } },
            { characterDetails: [{ chaId: 'char-b' }] },
            { deleteCharacterIds: ['char-b'] },
        ]
        for (const write of unrelatedWrites) expect(touches(write), JSON.stringify(write)).toBe(false)
        // Whole-record writes reach only the effect keys they would replace.
        expect(touchesNativeGenerationTarget({ root: {} }, target, [key('character', 'char-a', 'lastInteraction')])).toBe(false)
        expect(touchesNativeGenerationTarget({ character: { chaId: 'char-a' } }, target, [key('root', 'statics')])).toBe(false)
        expect(() => touchesNativeGenerationTarget(targetEdit, target, ['statics'])).toThrow(TypeError)
    })

    it('holds every target write and cancels the ticket on the first one only', () => {
        const { guard, cancels } = createGuard()
        guard.install(ticket('1'), target)
        const first = guard.holdWrite(targetEdit)
        const second = guard.holdWrite({ conversations: [{ type: 'replace-range', characterId: 'char-a', conversationId: 'chat-a' }] }, 'plugin')
        expect(first?.tickets).toEqual([ticket('1')])
        expect(second?.tickets).toEqual([ticket('1')])
        expect(cancels).toEqual([[ticket('1'), 'edit']])
        guard.install(ticket('2'), { characterId: 'char-b', conversationId: 'chat-b' })
        guard.holdWrite({ unitMutations: [{ key: key('messages', 'char-b', 'chat-b') }] }, 'plugin')
        guard.holdWrite({ unitMutations: [{ key: key('messages', 'char-b', 'chat-b') }] })
        expect(cancels).toEqual([[ticket('1'), 'edit'], [ticket('2'), 'plugin']])
        expect(guard.snapshot().map((entry) => [entry.ticket.sequence, entry.cancelled])).toEqual([['1', true], ['2', true]])
    })

    it('reports a held write synchronously and resolves its parked promise when the ticket settles', async () => {
        const { guard } = createGuard()
        guard.install(ticket('1'), target)
        const hold = guard.holdWrite(targetEdit)!
        expect(hold).not.toBeNull()
        expect((hold as unknown as PromiseLike<unknown>).then).toBeUndefined()
        const parked = track(hold.settled)
        await drain()
        expect(parked.settled).toBe(false)
        expect(await guard.settle(status('1', 'running'))).toBe('pending')
        await drain()
        expect(parked.settled).toBe(false)
        expect(await guard.settle(status('1', 'cancelled'))).toBe('released')
        await drain()
        expect(parked).toEqual({ settled: true, value: 'released' })
        expect(guard.holdWrite(targetEdit)).toBeNull()
        expect(await guard.settle(status('1', 'cancelled'))).toBe('unknown')
    })

    it('never holds or cancels a write that misses every guarded target', () => {
        const { guard, cancels } = createGuard()
        guard.install(ticket('1'), target, [key('character', 'char-a', 'lastInteraction')])
        expect(guard.holdWrite(otherConversationEdit)).toBeNull()
        expect(guard.holdWrite({ unitMutations: [{ key: key('character', 'char-a', 'name') }] })).toBeNull()
        expect(guard.holdWrite({ rootMutations: [{ key: 'username' }] })).toBeNull()
        expect(guard.holdWrite({ conversations: [{ type: 'reorder', characterId: 'char-a' }] }, 'plugin')).toBeNull()
        expect(cancels).toEqual([])
        expect(guard.holdWrite(targetEdit)).not.toBeNull()
        expect(guard.holdWrite(otherConversationEdit)).toBeNull()
        expect(cancels).toEqual([[ticket('1'), 'edit']])
    })

    it('adopts terminal preparation without output and keeps holding while its commit is unresolved', async () => {
        const { guard, adoptions } = createGuard()
        const phases = ['cancelled', 'failed', 'interrupted', 'stale'] as const
        for (const [index, phase] of phases.entries()) {
            const sequence = String(index + 1)
            guard.install(ticket(sequence), target)
            const parked = track(guard.holdWrite(targetEdit)!.settled)
            expect(await guard.settle(status(sequence, phase, 'none', true))).toBe('pending')
            expect(guard.holdWrite(targetEdit)?.tickets).toEqual([ticket(sequence)])
            expect(await guard.settle(status(sequence, phase))).toBe('released')
            await drain()
            expect(parked.value).toBe('released')
        }
        expect(adoptions.map(value => value.phase)).toEqual(phases)
        expect(guard.holdWrite(targetEdit)).toBeNull()
    })

    it('keeps no-output preparation fenced until required adoption succeeds', async () => {
        let readable = false
        let projected = false
        const { guard } = createGuard({ adopt: async () => {
            if (!readable) throw new Error('Proof unavailable')
            projected = true
        } })
        guard.install(ticket('1'), target)
        expect(await guard.settle(status('1', 'cancelled'))).toBe('fenced')
        expect(projected).toBe(false)
        expect(await guard.retryAdoption(ticket('1'))).toBe('fenced')
        expect(guard.snapshot()).toMatchObject([{ state: 'fenced' }])
        readable = true
        expect(await guard.retryAdoption(ticket('1'))).toBe('released')
        expect(projected).toBe(true)
        expect(guard.snapshot()).toEqual([])
    })

    it('releases a terminal settlement with output only after adoption succeeds', async () => {
        let finishAdoption!: () => void
        const adopted: NativeGenerationGuardStatus[] = []
        const { guard } = createGuard({
            adopt: (value) => {
                adopted.push(value)
                return new Promise<void>((resolve) => { finishAdoption = resolve })
            },
        })
        guard.install(ticket('1'), target)
        const parked = track(guard.holdWrite(targetEdit)!.settled)
        const settling = track(guard.settle(status('1', 'completed', 'committed')))
        await drain()
        expect(adopted).toEqual([status('1', 'completed', 'committed')])
        expect(parked.settled).toBe(false)
        expect(settling.settled).toBe(false)
        const duringAdoption = track(guard.holdWrite(targetEdit)!.settled)
        const repeated = guard.settle(status('1', 'completed', 'committed'))
        finishAdoption()
        await drain()
        expect(parked.value).toBe('released')
        expect(duringAdoption.value).toBe('released')
        expect(settling.value).toBe('released')
        expect(await repeated).toBe('released')
        expect(adopted).toHaveLength(1)
        expect(guard.holdWrite(targetEdit)).toBeNull()

        // A failure with a committed fallback row and a cancel after the output commit adopt too.
        const { guard: other, adoptions } = createGuard()
        other.install(ticket('2'), target)
        other.install(ticket('3'), { characterId: 'char-b', conversationId: 'chat-b' })
        expect(await other.settle(status('2', 'failed', 'committed'))).toBe('released')
        expect(await other.settle(status('3', 'cancelled', 'committed'))).toBe('released')
        expect(adoptions.map((value) => value.ticket.sequence)).toEqual(['2', '3'])
    })

    it('turns a failed adoption into the committed-refresh fence and keeps the ticket retryable', async () => {
        const failure = new Error('projection failed')
        let failing = true
        const { guard, fences, cancels } = createGuard({
            adopt: async () => { if (failing) throw failure },
        })
        guard.install(ticket('1'), target)
        const parked = track(guard.holdWrite(targetEdit)!.settled)
        expect(await guard.settle(status('1', 'completed', 'committed'))).toBe('fenced')
        await drain()
        expect(parked.value).toBe('fenced')
        expect(fences).toEqual([{
            ticket: ticket('1'), target, status: status('1', 'completed', 'committed'), reason: 'adoption-failed', error: failure,
        }])
        // Target edits stay held behind the fence, and the settled ticket is not cancelled again.
        expect(await guard.holdWrite(targetEdit)!.settled).toBe('fenced')
        expect(cancels).toEqual([[ticket('1'), 'edit']])
        expect(guard.holdWrite(otherConversationEdit)).toBeNull()
        expect(await guard.retryAdoption(ticket('1'))).toBe('fenced')
        failing = false
        expect(await guard.retryAdoption(ticket('1'))).toBe('released')
        expect(guard.holdWrite(targetEdit)).toBeNull()

        // Output that native cannot verify is fenced without an adoption attempt.
        guard.install(ticket('2'), target)
        expect(guard.release(ticket('2'))).toBe(false)
        expect(await guard.settle(status('2', 'completed', 'unavailable'))).toBe('fenced')
        expect(fences.at(-1)).toMatchObject({ ticket: ticket('2'), reason: 'output-unavailable' })
        expect(guard.holdWrite(targetEdit)?.tickets).toEqual([ticket('2')])
        expect(guard.release(ticket('2'))).toBe(true)
        expect(guard.holdWrite(targetEdit)).toBeNull()
    })

    it('cancels every guarded ticket on close, waits up to the bound, then fences the rest', async () => {
        const { guard, cancels, fences, clock } = createGuard()
        guard.install(ticket('1'), target)
        guard.install(ticket('2'), { characterId: 'char-b', conversationId: 'chat-b' })
        const parked = track(guard.holdWrite({ unitMutations: [{ key: key('messages', 'char-b', 'chat-b') }] })!.settled)
        const closing = track(guard.close(5_000))
        expect(cancels).toEqual([[ticket('2'), 'edit'], [ticket('1'), 'lifecycle']])
        expect(await guard.settle(status('1', 'cancelled'))).toBe('released')
        clock.advance(4_999)
        await drain()
        expect(closing.settled).toBe(false)
        expect(fences).toEqual([])
        clock.advance(1)
        await drain()
        expect(fences.map((event) => [event.ticket, event.reason])).toEqual([[ticket('2'), 'close-timeout']])
        expect(parked.value).toBe('fenced')
        expect(closing.value).toEqual({ released: [ticket('1')], fenced: [ticket('2')] })
        expect(clock.pending()).toBe(0)

        const idle = createGuard()
        expect(await idle.guard.close(5_000)).toEqual({ released: [], fenced: [] })
        expect(idle.clock.pending()).toBe(0)
        idle.guard.install(ticket('3'), target)
        const settledInTime = track(idle.guard.close(5_000))
        expect(await idle.guard.settle(status('3', 'completed', 'committed'))).toBe('released')
        await drain()
        expect(settledInTime.value).toEqual({ released: [ticket('3')], fenced: [] })
        expect(idle.clock.pending()).toBe(0)
        expect(idle.fences).toEqual([])
    })

    it('restores the hold from the active ticket list after a reload', async () => {
        const { guard, cancels } = createGuard()
        const effect = key('character', 'char-a', 'lastInteraction')
        expect(guard.holdWrite(targetEdit)).toBeNull()
        guard.reinstall([{ status: status('1', 'running'), ...target, effectKeys: [effect] }])
        const hold = guard.holdWrite({ unitMutations: [{ key: effect }] })
        expect(hold?.tickets).toEqual([ticket('1')])
        expect(cancels).toEqual([[ticket('1'), 'edit']])
        expect(guard.holdWrite(targetEdit)?.tickets).toEqual([ticket('1')])
        expect(guard.holdWrite(otherConversationEdit)).toBeNull()
        expect(await guard.settle(status('1', 'interrupted'))).toBe('released')
        expect(await hold!.settled).toBe('released')
    })

    it('parks a flush that lost the revision race to a guarded conversation without cancelling', async () => {
        const { guard, cancels } = createGuard()
        guard.install(ticket('1'), target)
        expect(guard.holdConflict([
            { kind: 'conversation', key1: 'char-a', key2: 'chat-b' },
            { kind: 'character', key1: 'char-a', key2: '' },
        ])).toBeNull()
        const hold = guard.holdConflict([{ kind: 'conversation', key1: 'char-a', key2: 'chat-a' }])!
        expect(hold.tickets).toEqual([ticket('1')])
        expect(cancels).toEqual([])
        expect(await guard.settle(status('1', 'completed', 'committed'))).toBe('released')
        expect(await hold.settled).toBe('released')
    })

    it('leaves the save queue free while a write is held, so adoption queued behind it runs', async () => {
        const queue = new Mutex()
        const log: string[] = []
        const { guard } = createGuard({
            adopt: () => queue.runExclusive(async () => { log.push('adopt') }),
        })
        guard.install(ticket('1'), target)
        const flushOnce = () => queue.runExclusive(async () => {
            const hold = guard.holdWrite(targetEdit)
            log.push(hold ? 'held' : 'commit')
            return hold
        })
        const flush = async () => {
            for (let hold = await flushOnce(); hold; hold = await flushOnce()) {
                if (await hold.settled === 'fenced') throw new Error('fenced')
            }
        }
        const flushing = track(flush())
        await queue.runExclusive(async () => { log.push('unrelated') })
        expect(log).toEqual(['held', 'unrelated'])
        const settling = track(guard.settle(status('1', 'completed', 'committed')))
        await drain()
        expect(settling.value).toBe('released')
        expect(flushing.settled).toBe(true)
        expect(log).toEqual(['held', 'unrelated', 'adopt', 'commit'])
    })
})
