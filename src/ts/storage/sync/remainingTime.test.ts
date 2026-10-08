import { describe, expect, it } from 'vitest'
import { createRemainingTimeEstimator, formatRemaining, REMAINING_TIME_WINDOW_MS } from './remainingTime'

const MiB = 1024 * 1024
type Sample = { at: number; done: number; items?: { done: number; total: number } }
const run = (samples: Sample[], total: number, key = 'phase') => {
    const estimator = createRemainingTimeEstimator()
    return samples.map(sample => estimator.update(key, sample.done, total, sample.at, sample.items))
}
const shown = (values: (number | undefined)[]) => values.filter((value): value is number => value !== undefined)

describe('remaining time', () => {
    it('estimates nothing until the phase has moved for the whole window', () => {
        const estimator = createRemainingTimeEstimator()
        const seen = [0, 1000, 2000, 3000, 4000].map(at => estimator.update('phase', at / 20, 1000, at))
        expect(seen).toEqual([undefined, undefined, undefined, undefined, undefined])
        // 50 bytes per second with 750 left.
        expect(estimator.update('phase', 250, 1000, REMAINING_TIME_WINDOW_MS)).toBe(15_000)
        expect(estimator.update('phase', 300, 1000, 6000)).toBe(14_000)
    })

    it('counts down a steady transfer by its rate', () => {
        const samples = Array.from({ length: 60 }, (_, second) => ({ at: second * 1000, done: second * MiB }))
        const values = run(samples, 60 * MiB)
        expect(values.slice(0, 5)).toEqual([undefined, undefined, undefined, undefined, undefined])
        expect(values[10]).toBe(50_000)
        expect(values[59]).toBe(1000)
        for (let index = 6; index < 60; index += 1) expect(values[index]).toBe(values[index - 1]! - 1000)
    })

    it('keeps counting down between the bursts of a transfer that moves a pack at a time', () => {
        // 5 MiB every 5 seconds.
        const samples = Array.from({ length: 60 }, (_, second) => ({ at: second * 1000, done: Math.floor(second / 5) * 5 * MiB }))
        const values = run(samples, 60 * MiB)
        expect(values.slice(0, 15).every(value => value === undefined)).toBe(true)
        expect(values.slice(15, 20)).toEqual([45_000, 44_000, 43_000, 42_000, 41_000])
        expect(values[20]).toBe(40_000)
        expect(values[59]).toBe(1000)
    })

    it('measures the rate over a span of the phase, while the last window keeps up with it', () => {
        const estimator = createRemainingTimeEstimator(5000)
        for (let at = 0; at <= 5000; at += 1000) estimator.update('phase', at / 10, 10_000, at)
        // The rate rose from 100 to 1000 bytes per second, within the ratio the span allows.
        let left: number | undefined
        for (let at = 6000; at <= 8000; at += 1000) left = estimator.update('phase', 500 + (at - 5000), 10_000, at)
        // 3500 bytes over 8 seconds, 6500 left.
        expect(left).toBeCloseTo(6500 * 8000 / 3500, 6)
        // Ten times faster than the span is not steady.
        for (let at = 9000; at <= 12_000; at += 1000) left = estimator.update('phase', 3500 + (at - 8000) * 10, 50_000, at)
        expect(left).toBeUndefined()
    })

    it('hides a backup preparation whose bytes stop moving while its items go on', () => {
        // run2 of the S3 backup: bytes went 6.5 -> 36.5 MiB in 6 s, then barely moved while items kept moving.
        const total = 225_053_040
        const samples: Sample[] = [
            [0, 6_782_443, 141], [3000, 21_638_631, 432], [6000, 38_264_453, 748], [9000, 39_047_199, 1038],
            [11_000, 39_158_128, 1386], [14_000, 40_746_001, 1661], [17_000, 41_320_681, 1979],
        ].map(([at, done, items]) => ({ at, done, items: { done: items, total: 7720 } }))
        const values = run(samples, total)
        expect(values.slice(4)).toEqual([undefined, undefined, undefined])
        for (const value of shown(values)) expect(value).toBeLessThan(2 * 60_000)
    })

    it('hides a restore download whose bytes and items disagree, and keeps its bytes alone within a steady range', () => {
        // The S3 restore: 84 of 86 packs held 7.7 of 25.5 MiB, and the last two arrived together.
        const megabytes = [5.1, 5.1, 5.2, 5.4, 5.4, 5.6, 5.6, 5.8, 5.8, 6.1, 6.1, 6.2, 6.3, 6.3, 6.5, 6.5, 7.0, 7.0, 7.1, 7.1, 7.2, 7.2, 7.3, 7.6, 7.6, 7.7, 7.7, 7.7, 7.7, 7.7]
        const packs = [50, 50, 54, 54, 54, 58, 58, 62, 62, 62, 65, 65, 65, 69, 69, 72, 72, 72, 77, 77, 77, 80, 80, 84, 84, 84, 84, 84, 84, 84]
        const samples = megabytes.map((value, second) => ({ at: second * 1000, done: Math.round(value * MiB), items: { done: packs[second], total: 86 } }))
        expect(shown(run(samples, 26_694_418))).toEqual([])
        // Without the pack counts the bytes alone stay steady through the pause before the last packs.
        const bytesOnly = run(samples.map(({ at, done }) => ({ at, done })), 26_694_418)
        const values = shown(bytesOnly)
        expect(values.length).toBeGreaterThan(0)
        expect(Math.max(...values)).toBeLessThan(2 * Math.min(...values))
        expect(Math.max(...values)).toBeLessThan(5 * 60_000)
        expect(bytesOnly.at(-1)).toBeDefined()
    })

    it('hides a transfer that stalls instead of extrapolating it', () => {
        const samples = Array.from({ length: 40 }, (_, second) => ({ at: second * 1000, done: Math.min(second, 20) * MiB }))
        const values = run(samples, 60 * MiB)
        expect(values[20]).toBe(40_000)
        // It keeps counting down for one window, then shows nothing.
        expect(values.slice(21, 26)).toEqual([39_000, 38_000, 37_000, 36_000, 35_000])
        expect(values.slice(26).every(value => value === undefined)).toBe(true)
    })

    it('shows nothing near the end when the rate has just dropped', () => {
        // 1 MiB per second up to 57 of 60 MiB, then 50 KiB per second.
        const samples = Array.from({ length: 70 }, (_, second) => ({ at: second * 1000, done: second <= 57 ? second * MiB : 57 * MiB + (second - 57) * 50 * 1024 }))
        const values = run(samples, 60 * MiB)
        expect(values[57]).toBe(3000)
        expect(values.slice(60).every(value => value === undefined)).toBe(true)
        for (const value of shown(values)) expect(value).toBeLessThanOrEqual(55_000)
    })

    it('stops estimating a phase whose total changed and starts again with the next phase', () => {
        const estimator = createRemainingTimeEstimator(5000)
        for (let at = 0; at <= 5000; at += 1000) estimator.update('pack', at / 10, 1000, at)
        expect(estimator.update('pack', 600, 1000, 6000)).toBeDefined()
        expect(estimator.update('pack', 700, 2000, 7000)).toBeUndefined()
        expect(estimator.update('pack', 800, 2000, 13_000)).toBeUndefined()
        for (let at = 14_000; at <= 19_000; at += 1000) estimator.update('next', at - 14_000, 10_000, at)
        expect(estimator.update('next', 6000, 10_000, 20_000)).toBe(4000)
    })

    it('has no estimate while nothing moves, for a phase not measured in bytes, or without a total', () => {
        const estimator = createRemainingTimeEstimator(5000)
        for (let at = 0; at <= 6000; at += 1000) expect(estimator.update('stalled', 100, 1000, at)).toBeUndefined()
        expect(estimator.update(undefined, 200, 1000, 7000)).toBeUndefined()
        for (let at = 0; at <= 6000; at += 1000) expect(estimator.update('open', at, 0, at)).toBeUndefined()
    })

    it('rounds the time left up to whole seconds', () => {
        expect(formatRemaining(14_200)).toBe('00:15')
        expect(formatRemaining(400)).toBe('00:01')
        expect(formatRemaining(3_725_000)).toBe('1:02:05')
    })
})
