import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { rebaseConversationJournal, ConversationJournalConflictError, type ConversationHistoryMapping } from './conversationJournalRebase'

const proofs = JSON.parse(readFileSync(new URL('../../../tests/fixtures/nativeConversationHistory.json', import.meta.url), 'utf8')) as Array<{
    name: string; original: string[]; current: string[]; mapping: ConversationHistoryMapping
}>
function proof(name: string) { return proofs.find(value => value.name === name)! }
async function apply(original: string[], current: string[], changes: Array<{ start: number; deleteCount: number; messages: string[] }>) {
    const fixture = proofs.find(value => JSON.stringify(value.original) === JSON.stringify(original) && JSON.stringify(value.current) === JSON.stringify(current))!
    const ranges = rebaseConversationJournal(fixture.mapping, changes.map(change => ({ ...change, insertCount: change.messages.length })))
    const result = [...current]
    for (const [index, range] of ranges.entries()) result.splice(range.start, range.deleteCount, ...changes[index].messages)
    return result
}

describe('windowed journal history reconciliation', () => {
    it('retains a native prefix and local edits to rows inserted by an earlier local edit', async () => {
        expect(await apply(['a', 'b'], ['remote', 'a', 'b'], [
            { start: 0, deleteCount: 1, messages: ['local-1'] },
            { start: 0, deleteCount: 1, messages: ['local-2'] },
            { start: 2, deleteCount: 0, messages: ['local-tail'] },
            { start: 2, deleteCount: 1, messages: ['edited-tail'] },
        ])).toEqual(['remote', 'local-2', 'b', 'edited-tail'])
    })

    it('keeps uniquely proven rows outside ambiguous insertion, deletion and mixed windows', async () => {
        for (const [original, current, start] of [
            [['a', 'a', 'b'], ['a', 'a', 'a', 'b'], 2],
            [['a', 'a', 'a', 'b'], ['a', 'a', 'b'], 3],
            [['a', 'a', 'b', 'old'], ['a', 'a', 'a', 'b', 'new'], 2],
        ] as const) {
            expect(await apply([...original], [...current], [{ start, deleteCount: 1, messages: ['local'] }]))
                .toEqual(current.map(value => value === 'b' ? 'local' : value))
        }
    })

    it('keeps duplicate positions when the entire history is unchanged', async () => {
        expect(await apply(['same', 'same'], ['same', 'same'], [
            { start: 1, deleteCount: 1, messages: ['local'] },
        ])).toEqual(['same', 'local'])
    })

    it('retains a native deletion and unrelated mixed replacements', async () => {
        expect(await apply(['a', 'removed', 'b'], ['a', 'b'], [
            { start: 2, deleteCount: 1, messages: ['local'] },
        ])).toEqual(['a', 'local'])
        expect(await apply(['a', 'old', 'c'], ['prefix', 'a', 'new', 'c'], [
            { start: 2, deleteCount: 1, messages: ['local'] },
        ])).toEqual(['prefix', 'a', 'new', 'local'])
    })

    it.each([
        { original: ['same', 'same'], current: ['same', 'same', 'same'], start: 1, deleteCount: 1 },
        { original: ['a', 'b'], current: ['a', 'remote', 'b'], start: 1, deleteCount: 0 },
        { original: ['a', 'b'], current: ['a', 'remote', 'b'], start: 0, deleteCount: 2 },
        { original: ['a', 'b'], current: ['a'], start: 1, deleteCount: 1 },
        { original: ['a', 'b'], current: ['a', 'changed'], start: 1, deleteCount: 1 },
    ])('retains local evidence instead of guessing a conflicting row: %j', async value => {
        const changes = [{ start: value.start, deleteCount: value.deleteCount, messages: ['local'] }]
        const before = structuredClone(changes)
        await expect(apply(value.original, value.current, changes)).rejects.toBeInstanceOf(ConversationJournalConflictError)
        expect(changes).toEqual(before)
    })

    it.each(['unique-fringe', 'repeated-fringe', 'shifted-suffix'])('replays edits at offsets supplied by a native fringe proof: %s', name => {
        const fixture = proof(name)
        const start = name === 'shifted-suffix' ? 1 : 0
        const ranges = rebaseConversationJournal(fixture.mapping, [{ start, deleteCount: 1, insertCount: 1 }])
        expect(ranges).toEqual([{ start: name === 'shifted-suffix' ? 2 : 0, deleteCount: 1, insertCount: 1 }])
    })

    it.each(['false-prefix', 'cell-budget'])('preserves native budget conflicts: %s', name => {
        expect(() => rebaseConversationJournal(proof(name).mapping, [{ start: 0, deleteCount: 1, insertCount: 1 }]))
            .toThrow('history-budget')
    })
})
