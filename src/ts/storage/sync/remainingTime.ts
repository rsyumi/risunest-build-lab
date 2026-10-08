import { formatElapsed } from '../../gui/nativeFileJobDialogModel'

/** How long a phase must have transferred before its rate predicts the time left. */
export const REMAINING_TIME_WINDOW_MS = 5000
/** How far the recent rate, and the time items predict, may stray from the rate the time left uses. */
const STEADY_RATIO = 2
/** Steps of progress a phase makes before its rate counts. */
const MIN_STEPS = 3

type Point = { at: number; done: number; items?: number }

/**
 * The time left of a phase measured in bytes, from that phase's own rate. The rate is taken
 * over a span of six windows, and a time is shown only while the rate over the last window
 * agrees with it, the phase keeps moving as often as it did, and the items it counts, when
 * given, predict about the same. A phase whose byte total changes after it began has no
 * estimate until another phase begins.
 */
export function createRemainingTimeEstimator(windowMs = REMAINING_TIME_WINDOW_MS) {
    const spanMs = 6 * windowMs
    let phase: string | undefined
    let total = 0
    let steady = false
    // The start of the phase and each sample that moved it, from the newest one at least a span old.
    const moved: Point[] = []
    let steps = 0
    const reset = () => { phase = undefined; moved.length = 0; steps = 0 }
    const latestBefore = (time: number) => {
        let found = moved[0]
        for (const point of moved) { if (point.at > time) break; found = point }
        return found
    }
    return {
        /**
         * `key` names the running phase, or is undefined while none is measured in bytes. `items`
         * are what the phase counts besides bytes, where it does. Returns milliseconds left.
         */
        update(key: string | undefined, done: number, planned: number, at: number, items?: { done: number; total: number }): number | undefined {
            if (key === undefined || !(planned > 0)) { reset(); return undefined }
            if (key !== phase) { reset(); phase = key; total = planned; steady = true; moved.push({ at, done, items: items?.done }); return undefined }
            if (planned !== total) steady = false
            if (!steady) return undefined
            if (done > moved[moved.length - 1].done) { moved.push({ at, done, items: items?.done }); steps++ }
            const latest = moved[moved.length - 1]
            while (moved.length > 2 && latest.at - moved[1].at >= spanMs) moved.shift()
            const start = moved[0]
            if (steps < MIN_STEPS || latest.at - start.at < windowMs) return undefined
            // A phase that has not moved for longer than it usually takes to move has no rate now.
            const gap = (latest.at - start.at) / (moved.length - 1)
            if (at - latest.at > Math.max(windowMs, 2 * gap)) return undefined
            const rate = (latest.done - start.done) / (latest.at - start.at)
            const from = latestBefore(latest.at - windowMs)
            const recent = (latest.done - from.done) / (latest.at - from.at)
            if (!(rate > 0) || !(recent > 0) || Math.max(rate, recent) > STEADY_RATIO * Math.min(rate, recent)) return undefined
            const left = Math.max(0, total - latest.done) / rate - (at - latest.at)
            if (!(left > 0)) return undefined
            if (items && start.items !== undefined && latest.items !== undefined && latest.items > start.items) {
                const byItems = Math.max(0, items.total - latest.items) * (latest.at - start.at) / (latest.items - start.items) - (at - latest.at)
                if (Math.max(left, byItems) > STEADY_RATIO * Math.max(Math.min(left, byItems), windowMs)) return undefined
            }
            return left
        },
        reset,
    }
}

/** Remaining time in the elapsed-time format, rounded up to whole seconds. */
export const formatRemaining = (milliseconds: number) => formatElapsed(Math.ceil(milliseconds / 1000) * 1000)
