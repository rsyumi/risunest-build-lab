import { describe, expect, test, vi } from 'vitest'
import {
    IncrementalTextUnits,
    TextUnitPlanner,
    isFallbackGraphemeBoundary,
    maxTextUnitLength,
    type TextUnit,
    type TextUnitScheduler,
} from './boundedTextUnits'
import {
    asciiControlText,
    graphemeSamples,
    mixedGraphemeText,
    repeatedKoreanEmojiThought,
} from './largeThoughtFixtures.testUtils'

const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

function graphemeBoundaries(text: string) {
    const boundaries = new Set<number>([text.length])
    for (const segment of segmenter.segment(text)) boundaries.add(segment.index)
    return boundaries
}

function plan(text: string, options?: ConstructorParameters<typeof TextUnitPlanner>[1]) {
    const planner = new TextUnitPlanner(text, options)
    const ends: number[] = []
    for (let start = 0; start < text.length; ) {
        const end = planner.next(start)
        expect(end).toBeGreaterThan(start)
        ends.push(end)
        start = end
    }
    return ends
}

function expectBoundedUnits(text: string, ends: number[], maxLength = maxTextUnitLength) {
    const boundaries = graphemeBoundaries(text)
    let start = 0
    const parts: string[] = []
    for (const end of ends) {
        expect(boundaries.has(end)).toBe(true)
        const unit = text.slice(start, end)
        // Only one oversized grapheme may exceed the bound.
        if (unit.length > maxLength) expect([...segmenter.segment(unit)]).toHaveLength(1)
        parts.push(unit)
        start = end
    }
    expect(parts.join('')).toBe(text)
}

class ManualFrames implements TextUnitScheduler {
    queue = new Map<number, () => void>()
    next = 0
    cancelled = 0
    request(callback: () => void) {
        const id = ++this.next
        this.queue.set(id, callback)
        return id
    }
    cancel(handle: unknown) {
        if (this.queue.delete(handle as number)) this.cancelled++
    }
    runOne() {
        const [id, callback] = this.queue.entries().next().value ?? []
        if (id === undefined) return false
        this.queue.delete(id)
        callback!()
        return true
    }
    runAll(limit = 10_000) {
        let frames = 0
        while (this.runOne()) {
            if (++frames > limit) throw new Error('frames did not settle')
        }
        return frames
    }
}

const joined = (units: readonly TextUnit[]) => units.map((unit) => unit.text).join('')

describe('bounded text units', () => {
    test('split the reproduced Korean/emoji shape and an ASCII control into bounded units', () => {
        for (const text of [
            repeatedKoreanEmojiThought(),
            asciiControlText(500_049),
            mixedGraphemeText(200_000),
        ]) {
            const ends = plan(text)
            expectBoundedUnits(text, ends)
            expect(ends.length).toBeGreaterThanOrEqual(Math.ceil(text.length / maxTextUnitLength))
        }
    })

    test('split long unbroken text only at grapheme boundaries', () => {
        const text = mixedGraphemeText(100_000, { spaces: false }).replaceAll('\r\n', '')
        expect(/[ \n]/.test(text)).toBe(false)
        expectBoundedUnits(text, plan(text))
        for (const maxLength of [1, 2, 3, 5, 7, 11]) {
            const sample = graphemeSamples.join('').repeat(4)
            expectBoundedUnits(sample, plan(sample, { maxLength }), maxLength)
            expectBoundedUnits(
                sample,
                plan(sample, { maxLength, segmenter: null }),
                maxLength,
            )
        }
    })

    test('the fallback agrees with Intl.Segmenter on the fixture samples', () => {
        const text = mixedGraphemeText(20_000, { spaces: false, seed: 3 })
        const boundaries = graphemeBoundaries(text)
        for (let index = 0; index <= text.length; index++)
            expect(isFallbackGraphemeBoundary(text, index), `${index}`).toBe(boundaries.has(index))
        expectBoundedUnits(text, plan(text, { segmenter: null }))
    })

    test('keep an oversized grapheme whole', () => {
        const text = 'x' + 'a' + '\u{301}'.repeat(5000) + 'y'
        for (const segmenterOption of [undefined, null]) {
            const ends = plan(text, { segmenter: segmenterOption })
            expect(ends).toEqual([1, 5002, 5003])
        }
    })

    test('prefer the last line break, then a space near the bound', () => {
        const lines = 'line one\nline two\n' + 'z'.repeat(3000)
        expect(plan(lines).slice(0, 1)).toEqual([18])
        const words = ('w'.repeat(99) + ' ').repeat(50)
        const ends = plan(words)
        expect(words[ends[0] - 1]).toBe(' ')
        expect(ends[0]).toBeGreaterThan(maxTextUnitLength - 256)
    })
})

describe('incremental text units', () => {
    test('mount one batch synchronously and the rest over frames', () => {
        const text = repeatedKoreanEmojiThought()
        const frames = new ManualFrames()
        let published: readonly TextUnit[] = []
        const units = new IncrementalTextUnits((next) => { published = next }, { scheduler: frames })
        units.setText(text)
        expect(joined(published).length).toBeLessThanOrEqual(2 * maxTextUnitLength)
        expect(units.complete).toBe(false)
        const count = frames.runAll()
        expect(count).toBeGreaterThan(1)
        expect(units.complete).toBe(true)
        expect(joined(published)).toBe(text)
        expect(published.at(-1)!.text.endsWith(text.slice(-20))).toBe(true)
        expectBoundedUnits(text, published.map((unit) => unit.end))
    })

    test('slow frames shrink the batch and fast frames grow it', () => {
        const frames = new ManualFrames()
        let clock = 0
        let published: readonly TextUnit[] = []
        const units = new IncrementalTextUnits((next) => { published = next }, {
            scheduler: frames,
            now: () => clock,
        })
        units.setText(repeatedKoreanEmojiThought())
        const added = () => {
            const before = published.length
            clock += frameInterval
            frames.runOne()
            return published.length - before
        }
        let frameInterval = 200
        for (let frame = 0; frame < 5; frame++) added()
        expect(units.budget).toBeLessThanOrEqual(maxTextUnitLength)
        expect(added()).toBe(1)
        frameInterval = 16
        for (let frame = 0; frame < 10; frame++) added()
        expect(units.budget).toBeGreaterThan(8192)
        expect(added()).toBeGreaterThan(4)
        frames.runAll()
        expect(joined(published)).toBe(repeatedKoreanEmojiThought())
    })

    test('disposal during incremental work stops further work', () => {
        const frames = new ManualFrames()
        const publish = vi.fn()
        const units = new IncrementalTextUnits(publish, { scheduler: frames })
        units.setText(repeatedKoreanEmojiThought())
        frames.runOne()
        const calls = publish.mock.calls.length
        units.dispose()
        expect(frames.cancelled).toBe(1)
        expect(frames.runAll()).toBe(0)
        units.setText('ignored')
        expect(publish).toHaveBeenCalledTimes(calls)
    })

    test('an appended stream keeps earlier units and replans the last one', () => {
        const frames = new ManualFrames()
        let published: readonly TextUnit[] = []
        const units = new IncrementalTextUnits((next) => { published = next }, {
            scheduler: frames,
            maxLength: 8,
            budget: 16,
        })
        // The appended ZWJ continues the final grapheme of the first snapshot.
        const first = 'abcdefgh ijklmnop \u{1f468}'
        const second = first + '\u{200d}\u{1f469}\u{200d}\u{1f467} tail text'
        units.setText(first)
        frames.runAll()
        const before = published
        units.setText(second)
        frames.runAll()
        expect(joined(published)).toBe(second)
        expect(published.slice(0, before.length - 1)).toEqual(before.slice(0, -1))
        for (let index = 0; index < before.length - 1; index++)
            expect(published[index]).toBe(before[index])
        expectBoundedUnits(second, published.map((unit) => unit.end), 8)
    })

    test('a replaced source restarts at the first changed unit', () => {
        const frames = new ManualFrames()
        let published: readonly TextUnit[] = []
        const units = new IncrementalTextUnits((next) => { published = next }, { scheduler: frames })
        const first = repeatedKoreanEmojiThought(20_000)
        units.setText(first)
        frames.runOne()
        const replacement = 'REPLACED ' + asciiControlText(100_000)
        units.setText(replacement)
        // The obsolete frame is cancelled; only the replacement is scheduled.
        expect(frames.queue.size).toBe(1)
        expect(published[0].text.startsWith('REPLACED')).toBe(true)
        frames.runAll()
        expect(joined(published)).toBe(replacement)
    })

    test('an edit that turns a kept unit end into a grapheme interior drops that unit', () => {
        const frames = new ManualFrames()
        let published: readonly TextUnit[] = []
        const units = new IncrementalTextUnits((next) => { published = next }, {
            scheduler: frames,
            maxLength: 4,
        })
        units.setText('abcdefghijkl')
        frames.runAll()
        const edited = 'abcd\u{301}fghijklm'
        units.setText(edited)
        frames.runAll()
        expect(joined(published)).toBe(edited)
        expectBoundedUnits(edited, published.map((unit) => unit.end), 4)
    })
})
