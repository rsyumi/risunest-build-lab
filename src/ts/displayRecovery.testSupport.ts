import type { DisplayRecoveryStore, DisplayRecoveryTimers } from './displayRecovery.svelte'

export interface DisplayRecoveryRecord {
    working: unknown[]
    marked: unknown[]
}

/**
 * A device store holding the display recovery record. Its writes stay pending until
 * `commitAll`, unless `commitAtOnce` is set.
 */
export function recoveryStore(
    initial: DisplayRecoveryRecord | string | null = null,
    options: { commitAtOnce?: boolean } = {},
) {
    let value: string | null = typeof initial === 'object' && initial !== null ? JSON.stringify(initial) : null
    if (typeof initial === 'string') value = initial
    const commits: Array<() => void> = []
    const records: Array<DisplayRecoveryRecord | null> = []
    const store: DisplayRecoveryStore = {
        getItem: () => value,
        setItem(_key, next) {
            value = next
            records.push(JSON.parse(next))
        },
        removeItem() {
            value = null
            records.push(null)
        },
        flush: () => options.commitAtOnce
            ? Promise.resolve()
            : new Promise<void>((resolve) => commits.push(resolve)),
    }
    return {
        store,
        /** Every write in order, `null` for a removal. */
        records,
        get record(): DisplayRecoveryRecord | null {
            return value === null ? null : JSON.parse(value)
        },
        get pendingWrites() {
            return commits.length
        },
        commitNext() {
            commits.shift()?.()
        },
        commitAll() {
            for (const commit of commits.splice(0)) commit()
        },
    }
}

/** Timers that fire only when the test runs them. */
export function manualTimers() {
    const pending = new Map<number, () => void>()
    let next = 0
    const timers: DisplayRecoveryTimers = {
        set(callback) {
            pending.set(++next, callback)
            return next
        },
        clear(handle) {
            pending.delete(handle as number)
        },
    }
    return {
        timers,
        get pending() {
            return pending.size
        },
        runAll() {
            for (const [handle, callback] of [...pending]) {
                pending.delete(handle)
                callback()
            }
        },
    }
}
