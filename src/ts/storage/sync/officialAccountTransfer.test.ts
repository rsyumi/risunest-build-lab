import { afterEach, describe, expect, it, vi } from 'vitest'
import { accountRetryDeadline, transferOfficialAccountAssets } from './officialAccountTransfer'
import { AccountReadError } from '../accountReadError'

afterEach(() => vi.useRealTimers())
let sequence = 0
function setup(count = 12, concurrency: 1 | 2 | 4 = 2) {
    const results: Record<string, string> = {}
    return { accountId: `synthetic-${sequence++}`, origin: 'https://synthetic.invalid', concurrency,
        keys: Array.from({ length: count }, (_, i) => String(i)),
        read: vi.fn(async () => ({ kind: 'value' as const, bytes: Uint8Array.of(1) })),
        install: vi.fn(async () => {}), settled: (key: string, result: string) => { results[key] = result }, results }
}

describe('official account GET scheduling', () => {
    it.each(['read', 'install'] as const)('does not mark interrupted %s work as failed', async boundary => {
        const h = setup(4)
        const controller = new AbortController()
        const pending: (() => void)[] = []
        const interrupted = (signal?: AbortSignal) => new Promise<never>((_resolve, reject) => {
            pending.push(() => reject(signal?.reason))
            signal?.addEventListener('abort', () => reject(signal.reason), { once: true })
        })
        const run = transferOfficialAccountAssets({ ...h, signal: controller.signal,
            read: boundary === 'read' ? (_key, options) => interrupted(options.signal) : h.read,
            install: boundary === 'install' ? (_key, _bytes, signal) => interrupted(signal) : h.install,
        })
        await vi.waitFor(() => expect(pending).toHaveLength(boundary === 'read' ? 2 : 1))
        controller.abort()
        expect(await run).toMatchObject({ cancelled: true })
        expect(h.results).toEqual({})
    })

    it.each([new AccountReadError('http', 401), new Error('unexpected read failure')])('counts only the cause of an internal stop as failed', async failure => {
        const h = setup(4)
        const run = transferOfficialAccountAssets({ ...h,
            read: async (key, options) => {
                if (key === '0') throw failure
                return new Promise((_resolve, reject) => options.signal!.addEventListener('abort', () => reject(options.signal!.reason), { once: true }))
            },
        })
        expect(await run).toMatchObject({ reason: failure instanceof AccountReadError ? 'authentication' : 'local' })
        expect(h.results).toEqual({ '0': 'failed' })
    })

    it('stops immediately on a local cache failure without retrying other files', async () => {
        const h = setup(12, 1)
        h.read.mockRejectedValue(new AccountReadError('local'))
        expect(await transferOfficialAccountAssets(h)).toMatchObject({ reason: 'local' })
        expect(Object.values(h.results)).toEqual(['failed'])
    })
    it('overlaps independent reads while bounding completed bodies and installing in order', async () => {
        const h = setup(6)
        const reads: (() => void)[] = [], installs: (() => void)[] = []
        h.read.mockImplementation(() => new Promise(resolve => reads.push(() => resolve({ kind: 'value', bytes: Uint8Array.of(1) }))))
        h.install.mockImplementation(() => new Promise(resolve => installs.push(resolve)))
        const run = transferOfficialAccountAssets(h)
        expect(reads).toHaveLength(2)
        reads.shift()!(); reads.shift()!()
        await vi.waitFor(() => expect(installs).toHaveLength(1))
        expect(reads).toHaveLength(0)
        installs.shift()!()
        await vi.waitFor(() => { expect(reads).toHaveLength(1); expect(installs).toHaveLength(1) })
        for (let i = 0; i < 5; i++) {
            reads.shift()?.()
            installs.shift()?.()
            await new Promise(resolve => setTimeout(resolve, 0))
        }
        await run
        expect(Object.values(h.results)).toEqual(Array(6).fill('installed'))
    })

    it('coalesces simultaneous overload into one reduction and only one recovery probe', async () => {
        vi.useFakeTimers(); vi.setSystemTime(0)
        const h = setup(8, 4)
        const pending: { resolve(value: { kind: 'value'; bytes: Uint8Array<ArrayBuffer> }): void; reject(error: unknown): void }[] = []
        h.read.mockImplementation(() => new Promise((resolve, reject) => pending.push({ resolve, reject })))
        const progress: { limit: number; waitingUntil?: number }[] = []
        const run = transferOfficialAccountAssets({ ...h, progress: value => progress.push(value) })
        const initial = pending.splice(0)
        initial[0].reject(new AccountReadError('http', 429, '45'))
        initial[1].reject(new AccountReadError('http', 429, '45'))
        initial[2].resolve({ kind: 'value', bytes: Uint8Array.of(1) })
        initial[3].resolve({ kind: 'value', bytes: Uint8Array.of(1) })
        await vi.advanceTimersByTimeAsync(44_999)
        expect(pending).toHaveLength(0)
        expect(progress.at(-1)).toMatchObject({ limit: 2, waitingUntil: 45_000 })
        await vi.advanceTimersByTimeAsync(1)
        expect(pending).toHaveLength(1)
        pending.shift()!.resolve({ kind: 'value', bytes: Uint8Array.of(1) })
        await vi.advanceTimersByTimeAsync(0)
        expect(pending).toHaveLength(2)
        while (Object.keys(h.results).length < 8) {
            pending.splice(0).forEach(p => p.resolve({ kind: 'value', bytes: Uint8Array.of(1) }))
            await vi.advanceTimersByTimeAsync(0)
        }
        await run
        expect(progress.every(p => p.limit >= 2)).toBe(true)
    })

    it('stops after five distinct exhausted objects even at concurrency one', async () => {
        vi.useFakeTimers()
        const h = setup(20, 1)
        h.read.mockRejectedValue(new AccountReadError('http', 429, '0'))
        const run = transferOfficialAccountAssets(h)
        await vi.advanceTimersByTimeAsync(1000)
        expect(await run).toMatchObject({ reason: 'service' })
        expect(h.read).toHaveBeenCalledTimes(15)
        expect(Object.values(h.results)).toEqual(Array(5).fill('failed'))
    })

    it('recovers concurrency after sustained successful exchanges', async () => {
        vi.useFakeTimers(); vi.setSystemTime(0)
        const h = setup(20, 4)
        h.read.mockRejectedValueOnce(new AccountReadError('http', 429, '0'))
        const limits: number[] = []
        const run = transferOfficialAccountAssets({ ...h, progress: value => limits.push(value.limit) })
        await vi.advanceTimersByTimeAsync(100)
        await run
        expect(limits).toContain(2)
        expect(limits.at(-1)).toBe(4)
        expect(Object.values(h.results)).toEqual(Array(20).fill('installed'))
    })

    it('decays fallback pauses after successful recovery rather than accumulating unrelated outages', async () => {
        vi.useFakeTimers(); vi.setSystemTime(0)
        const h = setup(8, 1)
        const attempts = new Map<string, number>()
        const pauses: number[] = []
        const run = transferOfficialAccountAssets({ ...h,
            read: async key => {
                const attempt = attempts.get(key) ?? 0
                attempts.set(key, attempt + 1)
                if (attempt === 0) throw new AccountReadError('http', 429)
                return { kind: 'value', bytes: Uint8Array.of(1) }
            },
            progress: value => { if (value.waitingUntil) pauses.push(value.waitingUntil - Date.now()) },
        })
        await vi.advanceTimersByTimeAsync(500_000)
        await run
        expect(Object.values(h.results)).toEqual(Array(8).fill('installed'))
        expect(Math.max(...pauses)).toBeLessThanOrEqual(1200)
    })

    it('continues past permanent errors and retries transient failures within three attempts', async () => {
        vi.useFakeTimers()
        const h = setup(10)
        let call = 0
        h.read.mockImplementation(async () => {
            call++
            if (call <= 6) throw new AccountReadError('http', 404)
            if (call <= 8) throw new AccountReadError('transport')
            return { kind: 'value', bytes: Uint8Array.of(1) }
        })
        const run = transferOfficialAccountAssets(h)
        await vi.advanceTimersByTimeAsync(5000)
        expect(await run).toMatchObject({ reason: undefined })
        expect(Object.values(h.results).filter(value => value === 'installed')).toHaveLength(4)
        expect(Object.values(h.results).filter(value => value === 'failed')).toHaveLength(6)
    })

    it('retains a server pause across cancellation and a subsequent run', async () => {
        vi.useFakeTimers(); vi.setSystemTime(0)
        const h = setup(1), controller = new AbortController()
        h.read.mockRejectedValueOnce(new AccountReadError('http', 503, '86400'))
        const first = transferOfficialAccountAssets({ ...h, signal: controller.signal })
        await vi.advanceTimersByTimeAsync(1)
        controller.abort()
        expect(await first).toMatchObject({ cancelled: true })
        const secondController = new AbortController()
        const second = transferOfficialAccountAssets({ ...h, signal: secondController.signal })
        await vi.advanceTimersByTimeAsync(60_000)
        expect(h.read).toHaveBeenCalledOnce()
        secondController.abort(); await second
    })

    it('parses delay seconds and HTTP dates without shortening a server deadline', () => {
        const now = Date.UTC(2026, 9, 19)
        expect(accountRetryDeadline('86400', now)).toBe(now + 86_400_000)
        expect(accountRetryDeadline(new Date(now + 600_000).toUTCString(), now)).toBe(now + 600_000)
        expect(accountRetryDeadline('nonsense', now)).toBeUndefined()
        expect(accountRetryDeadline('-1', now)).toBeUndefined()
    })
})
