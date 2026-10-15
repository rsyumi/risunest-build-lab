export interface NativeRestoreClock {
    schedule(milliseconds: number, callback: () => void): () => void
}

const systemClock: NativeRestoreClock = {
    schedule(milliseconds, callback) {
        const timer = setTimeout(callback, milliseconds)
        return () => clearTimeout(timer)
    },
}

export class NativeRestoreControlTimeout extends Error {
    readonly code = 'restore-control-timeout'
    constructor(operation: string) {
        super(`Native restore ${operation} did not respond`)
        this.name = 'NativeRestoreControlTimeout'
    }
}

// A deadline applies to one control/UI response, never the lifetime of the job.
export function nativeRestoreDeadline<T>(
    work: Promise<T>,
    operation: string,
    clock: NativeRestoreClock = systemClock,
    lateResult?: (value: T) => void | Promise<void>,
    milliseconds = 30_000,
    signal?: AbortSignal,
): Promise<T> {
    return new Promise((resolve, reject) => {
        let settled = false
        let cancel = () => {}
        const abort = () => {
            if (settled) return
            settled = true
            cancel()
            signal?.removeEventListener('abort', abort)
            reject(new DOMException('Backup cancelled', 'AbortError'))
        }
        cancel = clock.schedule(milliseconds, () => {
            if (settled) return
            settled = true
            signal?.removeEventListener('abort', abort)
            reject(new NativeRestoreControlTimeout(operation))
        })
        if (settled) cancel()
        else signal?.addEventListener('abort', abort, { once: true })
        if (signal?.aborted) abort()
        work.then(value => {
            if (settled) {
                void Promise.resolve().then(() => lateResult?.(value)).catch(() => {})
                return
            }
            settled = true
            cancel()
            signal?.removeEventListener('abort', abort)
            resolve(value)
        }, error => {
            if (settled) return
            settled = true
            cancel()
            signal?.removeEventListener('abort', abort)
            reject(error)
        })
    })
}
