import { describe, expect, it } from 'vitest'
import { replaceArrayRange } from './arrayRange'

describe('replaceArrayRange', () => {
    it('matches splice for inserts, deletes and replacements', () => {
        const cases: [number, number, number[]][] = [
            [0, 0, [9]],
            [2, 0, [7, 8]],
            [1, 2, []],
            [1, 2, [5]],
            [0, 5, [1, 2, 3, 4, 5, 6]],
            [5, 0, [6]],
        ]
        for (const [start, deleteCount, items] of cases) {
            const expected = [0, 1, 2, 3, 4]
            const expectedRemoved = expected.splice(start, deleteCount, ...items)
            const actual = [0, 1, 2, 3, 4]
            const removed = replaceArrayRange(actual, start, deleteCount, items)
            expect(actual).toEqual(expected)
            expect(removed).toEqual(expectedRemoved)
        }
    })

    it('replaces a range longer than a call can take as arguments', () => {
        const target = [1, 2, 3]
        const items = Array.from({ length: 1_000_000 }, (_, index) => index)
        replaceArrayRange(target, 1, 1, items)
        expect(target.length).toBe(1_000_002)
        expect(target[0]).toBe(1)
        expect(target[1]).toBe(0)
        expect(target[1_000_000]).toBe(999_999)
        expect(target[1_000_001]).toBe(3)
    })
})
