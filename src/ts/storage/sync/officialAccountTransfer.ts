import type { AccountReadOptions, AccountReadResult } from '../accountStorage'
import { AccountReadError, OfficialAssetOwnershipError } from '../accountReadError'
import { createRateMeter } from './transferRate'
import type { OfficialAccountAssetProgress } from './officialAccountAssetOutcome'

interface Pause { epoch: number; until: number; paused: boolean; probe: symbol | null; cap: number; transient: number; episodes: number }
const pauses = new Map<string, Pause>()

export function accountRetryDeadline(value: string | undefined, now: number): number | undefined {
    if (!value?.trim()) return undefined
    const text = value.trim()
    const delay = /^\d+$/.test(text) ? Number(text) * 1000 : NaN
    const date = Number.isNaN(delay) && /[a-z]/i.test(text) ? Date.parse(text) : NaN
    const deadline = Number.isNaN(delay) ? date : now + delay
    return Number.isFinite(deadline) ? Math.max(now, deadline) : undefined
}

export interface OfficialAccountTransferProgress {
    bytes: number
    bytesPerSecond?: number
    active: number
    limit: number
    waitingUntil?: number
}

/** The account GET pool holds each slot through verified installation. */
export async function transferOfficialAccountAssets(options: {
    accountId: string; origin: string; keys: readonly string[]; concurrency?: 1 | 2 | 4; signal?: AbortSignal
    read(key: string, options: AccountReadOptions): Promise<AccountReadResult>
    install(key: string, bytes: Uint8Array, signal: AbortSignal): Promise<void>
    settled(key: string, result: 'installed' | 'missing' | 'failed'): void
    progress?(value: OfficialAccountTransferProgress): void
}) {
    const nominal = options.concurrency ?? 2
    const pauseKey = JSON.stringify([options.accountId, options.origin])
    const pause = pauses.get(pauseKey) ?? { epoch: 0, until: 0, paused: false, probe: null, cap: nominal, transient: 0, episodes: 0 }
    if (!pause.paused) { pause.cap = nominal; pause.transient = 0; pause.episodes = 0 }
    pauses.set(pauseKey, pause)
    let limit = Math.min(nominal, pause.cap)
    const retries: { key: string; attempts: number; ready: number }[] = []
    let nextKey = 0
    const queued = () => nextKey < options.keys.length || retries.length > 0
    const active = new Set<Promise<void>>()
    const controller = new AbortController()
    const token = Symbol()
    const abort = () => { controller.abort(options.signal?.reason); notify() }
    let wake: (() => void) | undefined
    const notify = () => wake?.()
    let reason: OfficialAccountAssetProgress['reason']
    let serviceFailures = 0, bytes = 0
    const rate = createRateMeter()
    rate.add(Date.now(), 0)
    let installTail = Promise.resolve()
    const stop = (why: typeof reason) => { reason ??= why; controller.abort(); notify() }
    const progress = () => options.progress?.({ bytes, bytesPerSecond: rate.rate(Date.now()), active: active.size,
        limit, waitingUntil: pause.paused ? pause.until : retries.find(item => item.ready > Date.now())?.ready })
    const fallback = (attempt: number) => Math.min(300_000, 1000 * 2 ** Math.min(18, attempt) * (1 + Math.random() * 0.2))
    const slow = (ticket: number, error: AccountReadError) => {
        const fresh = ticket === pause.epoch
        if (fresh) {
            pause.epoch++; pause.cap = Math.max(1, Math.floor(limit / 2)); pause.episodes++
            pause.paused = true; pause.probe = null
        }
        limit = Math.min(limit, pause.cap)
        pause.until = Math.max(pause.until, accountRetryDeadline(error.retryAfter, Date.now())
            ?? Date.now() + fallback(Math.max(0, pause.episodes - 1)))
    }
    const start = (item: typeof retries[number], probe: boolean) => {
        const ticket = pause.epoch
        if (probe) pause.probe = token
        item.attempts++
        const task = (async () => {
            let result: AccountReadResult
            try {
                result = await options.read(item.key, { signal: controller.signal, onBytes: count => {
                    bytes += count; rate.add(Date.now(), bytes)
                } })
            } catch (error) {
                if (controller.signal.aborted) { options.settled(item.key, 'failed'); return }
                if (error instanceof OfficialAssetOwnershipError) { options.settled(item.key, 'failed'); stop(error.reason === 'receipt' ? 'local' : error.reason); return }
                if (!(error instanceof AccountReadError)) { options.settled(item.key, 'failed'); stop('authentication'); return }
                if (error.kind === 'local') { options.settled(item.key, 'failed'); stop('local'); return }
                if (error.kind === 'account-changed' || error.status === 401 || error.status === 403) {
                    options.settled(item.key, 'failed'); stop(error.kind === 'account-changed' ? 'account-changed' : 'authentication'); return
                }
                const retryable = error.kind === 'transport' || error.kind === 'timeout' || error.status === 429 || (error.status !== undefined && error.status >= 500)
                if (retryable) {
                    pause.transient++
                    if (error.status === 429 || (error.status === 503 && error.retryAfter) || pause.transient >= 2 || probe) slow(ticket, error)
                    if (item.attempts < 3) { item.ready = Date.now() + (pause.paused ? 0 : fallback(item.attempts - 1)); retries.push(item); return }
                    serviceFailures++
                } else if (probe && ticket === pause.epoch) {
                    // A permanent object response is a completed exchange, but not a successful recovery probe.
                    pause.probe = null
                }
                options.settled(item.key, 'failed')
                if (serviceFailures >= 5) stop('service')
                return
            }
            if (ticket === pause.epoch) {
                serviceFailures = 0; pause.transient = 0
                if (probe && pause.probe === token) { pause.paused = false; pause.probe = null; pause.until = 0 }
            }
            if (result.kind === 'missing') { options.settled(item.key, 'missing'); return }
            try {
                const install = installTail.then(async () => {
                    if (controller.signal.aborted) throw controller.signal.reason
                    await options.install(item.key, result.bytes, controller.signal)
                })
                installTail = install.catch(() => {})
                await install
                options.settled(item.key, 'installed')
            } catch (error) {
                options.settled(item.key, 'failed')
                if (!controller.signal.aborted) stop(error instanceof OfficialAssetOwnershipError && error.reason !== 'receipt' ? error.reason : 'local')
            }
        })().finally(() => { active.delete(task); notify(); progress() })
        active.add(task)
    }
    options.signal?.addEventListener('abort', abort, { once: true })
    if (options.signal?.aborted) abort()
    const update = setInterval(progress, 150)
    try {
        while (queued() || active.size) {
            while (!controller.signal.aborted && active.size < limit && queued()) {
                const now = Date.now()
                if (pause.paused && (pause.until > now || pause.probe)) break
                const index = retries.findIndex(item => item.ready <= now)
                const item = index >= 0 ? retries.splice(index, 1)[0]
                    : nextKey < options.keys.length && active.size + retries.length < limit
                        ? { key: options.keys[nextKey++], attempts: 0, ready: 0 } : undefined
                if (!item) break
                start(item, pause.paused)
            }
            progress()
            if (!active.size && (controller.signal.aborted || !queued())) break
            const next = Math.max(pause.paused ? pause.until : 0,
                retries.reduce((earliest, item) => Math.min(earliest, item.ready),
                    nextKey < options.keys.length && active.size + retries.length < limit ? 0 : Infinity))
            await new Promise<void>(resolve => {
                let timer: ReturnType<typeof setTimeout> | undefined
                wake = () => { clearTimeout(timer); resolve() }
                if (!controller.signal.aborted && active.size < limit && !pause.probe && Number.isFinite(next)) {
                    timer = setTimeout(wake, Math.min(2_147_483_647, Math.max(1, next - Date.now())))
                }
            })
            wake = undefined
        }
    } finally {
        clearInterval(update); options.signal?.removeEventListener('abort', abort)
        if (pause.probe === token) pause.probe = null
    }
    return { reason, cancelled: options.signal?.aborted === true }
}
