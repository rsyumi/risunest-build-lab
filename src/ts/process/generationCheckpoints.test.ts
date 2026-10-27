import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createGenerationCheckpoints } from './generationCheckpoints'
import type { GenerationDiagnosticTrace, GenerationSavePath } from '../generationDiagnostics'

function setup() {
    let owned = true
    let text = ''
    let saved = ''
    let savePath: GenerationSavePath = 'windowed'
    let captures = true
    const flushes: string[] = []
    const phases: unknown[][] = []
    let write = async () => {
        saved = text
        return true
    }
    const checkpoints = createGenerationCheckpoints({
        isOwned: () => owned,
        savePath: () => savePath,
        capturesImmutableCharacters: () => captures,
        flush: () => {
            flushes.push(text)
            return write()
        },
        diagnostics: { phase: (...args: unknown[]) => { phases.push(args) } } as unknown as GenerationDiagnosticTrace,
    })
    return {
        checkpoints, flushes, phases,
        update(value: string) {
            text = value
            checkpoints.applied()
        },
        loseOwner() { owned = false },
        route(path: GenerationSavePath, immutableCapture = true) {
            savePath = path
            captures = immutableCapture
        },
        setWriter(next: typeof write) { write = next },
        saved: () => saved,
    }
}

describe('generation checkpoints', () => {
    beforeEach(() => vi.useFakeTimers())
    afterEach(() => vi.useRealTimers())

    it('saves continuous updates at the interval without waiting for a quiet gap', async () => {
        const h = setup()
        for (let index = 0; index < 45; index++) {
            h.update(`snapshot-${index}`)
            await vi.advanceTimersByTimeAsync(100)
        }
        expect(h.flushes).toEqual(['snapshot-19', 'snapshot-39'])
        h.update('last')
        await h.checkpoints.checkpoint()
        expect(h.saved()).toBe('last')
        await vi.advanceTimersByTimeAsync(10_000)
        expect(h.flushes).toEqual(['snapshot-19', 'snapshot-39', 'last'])
        expect(h.phases).toEqual([
            ['checkpoint', 'enter', { savePath: 'windowed' }],
            ['checkpoint', 'exit', { outcome: 'ok', savePath: 'windowed' }],
        ])
    })

    it('keeps one request while a save is blocked and saves newer output before completion', async () => {
        const h = setup()
        let release!: (saved: boolean) => void
        h.setWriter(() => new Promise((resolve) => { release = resolve }))
        h.update('first')
        await vi.advanceTimersByTimeAsync(2_000)
        h.update('second')
        await vi.advanceTimersByTimeAsync(6_000)
        h.update('third')
        expect(h.flushes).toEqual(['first'])
        let settled = false
        const completion = h.checkpoints.checkpoint().then(() => { settled = true })
        await vi.advanceTimersByTimeAsync(0)
        expect(settled).toBe(false)
        h.setWriter(async () => true)
        release(true)
        await completion
        expect(h.flushes).toEqual(['first', 'third'])
    })

    it('stops further checkpoints after a storage failure and still lets completion continue', async () => {
        const h = setup()
        h.setWriter(async () => false)
        h.update('first')
        await vi.advanceTimersByTimeAsync(2_000)
        h.update('after failure')
        await vi.advanceTimersByTimeAsync(10_000)
        await expect(h.checkpoints.checkpoint()).resolves.toBeUndefined()
        expect(h.flushes).toEqual(['first'])
        expect(h.phases).toEqual([
            ['checkpoint', 'exit', { outcome: 'failed', savePath: 'windowed' }],
            ['checkpoint', 'exit', { outcome: 'skipped', skipReason: 'storage-failed', savePath: 'windowed' }],
        ])
    })

    it('stops at a refused periodic save and rejects an awaited one', async () => {
        const h = setup()
        const refusal = new Error('synthetic fence')
        h.setWriter(async () => { throw refusal })
        h.update('first')
        await vi.advanceTimersByTimeAsync(2_000)
        h.update('second')
        await vi.advanceTimersByTimeAsync(10_000)
        expect(h.flushes).toEqual(['first'])
        await expect(h.checkpoints.checkpoint()).rejects.toBe(refusal)
        expect(h.phases.at(-1)).toEqual(['checkpoint', 'exit', { outcome: 'stale', savePath: 'windowed' }])
    })

    it.each([
        ['fallback', true, 'compatibility'],
        ['complete', false, 'missing-capture'],
        ['unknown', true, 'ineligible'],
    ] as const)('skips a %s save path (immutable capture: %s) as %s and checks again when it changes', async (path, captures, skipReason) => {
        const h = setup()
        h.route(path, captures)
        h.update('first')
        await vi.advanceTimersByTimeAsync(10_000)
        await vi.advanceTimersByTimeAsync(0)
        expect(h.flushes).toEqual([])
        expect(h.phases).toEqual([['checkpoint', 'exit', { outcome: 'skipped', skipReason, savePath: path }]])
        h.route('complete')
        h.update('second')
        await vi.advanceTimersByTimeAsync(2_000)
        expect(h.flushes).toEqual(['second'])
        h.checkpoints.stop()
    })

    it('saves nothing for an old target after ownership loss and never restarts a stopped timer', async () => {
        const h = setup()
        h.update('old target')
        h.loseOwner()
        await vi.advanceTimersByTimeAsync(2_000)
        await h.checkpoints.checkpoint()
        expect(h.flushes).toEqual([])
        const stopped = setup()
        stopped.update('first')
        stopped.checkpoints.stop()
        stopped.update('late applied callback')
        await vi.advanceTimersByTimeAsync(10_000)
        expect(stopped.flushes).toEqual([])
        expect(vi.getTimerCount()).toBe(0)
    })
})
