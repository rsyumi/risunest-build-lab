import { afterEach, describe, expect, it, vi } from 'vitest'
import {
    beginGenerationEndRun,
    noteGenerationMessage,
    noteGenerationStarted,
    subscribeGenerationEnd,
    type GenerationEndRecord,
} from './generationEnd'

const target = { characterId: 'char', conversationId: 'chat' }
const unsubscribes: Array<() => void> = []

function listen() {
    const records: GenerationEndRecord[] = []
    unsubscribes.push(subscribeGenerationEnd((record) => records.push(record)))
    return records
}

afterEach(() => {
    while (unsubscribes.length) unsubscribes.pop()!()
})

describe('generation end runs', () => {
    it('reports once what the captured steps noted', async () => {
        const records = listen()
        const run = beginGenerationEndRun(target, { reroll: true })

        await run.capture(async () => {
            noteGenerationStarted(target)
            noteGenerationMessage(target, 'gen-1')
            noteGenerationMessage(target, 'gen-1')
            noteGenerationMessage(target, undefined)
        })
        await run.capture(async () => {
            noteGenerationStarted(target)
            noteGenerationMessage(target, 'gen-2')
        })
        run.finish('completed')
        run.finish('failed')

        expect(records).toEqual([{ ...target, status: 'completed', reroll: true, messageIds: ['gen-1', 'gen-2'] }])
    })

    it('reports nothing when no captured step entered generation', async () => {
        const records = listen()
        const run = beginGenerationEndRun(target)

        await run.capture(async () => noteGenerationMessage(target, 'gen-1'))
        run.finish('failed')

        expect(records).toEqual([])
    })

    it('ignores notes outside a capture or for another conversation', async () => {
        const records = listen()
        const run = beginGenerationEndRun(target)

        noteGenerationStarted(target)
        noteGenerationMessage(target, 'before')
        await run.capture(async () => {
            noteGenerationStarted({ characterId: 'char', conversationId: 'other' })
            noteGenerationMessage({ characterId: 'other', conversationId: 'chat' }, 'other')
            noteGenerationMessage(undefined, 'unknown')
        })
        run.finish('completed')
        expect(records).toEqual([])

        const next = beginGenerationEndRun(target)
        await next.capture(async () => noteGenerationStarted(target))
        noteGenerationMessage(target, 'after')
        next.finish('aborted')
        expect(records).toEqual([{ ...target, status: 'aborted', reroll: false, messageIds: [] }])
    })

    it('keeps reporting to other listeners when one throws', async () => {
        const error = vi.spyOn(console, 'error').mockImplementation(() => {})
        unsubscribes.push(subscribeGenerationEnd(() => { throw new Error('listener failure') }))
        const records = listen()
        const run = beginGenerationEndRun(target)
        await run.capture(async () => noteGenerationStarted(target))

        run.finish('failed')

        expect(records).toHaveLength(1)
        expect(error).toHaveBeenCalledOnce()
        error.mockRestore()
    })
})
