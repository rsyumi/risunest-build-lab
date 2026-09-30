import './androidNativeControl'
import { isTauriAndroid, isTauriIOS } from './platform'
import { beginIOSBackgroundTask, BackgroundExecutionExpiredError, isBackgroundExpiryReason } from './iosNative'

export type MobileTaskKind = 'backup' | 'restore' | 'sync' | 'import' | 'export' | 'maintenance'
export interface MobileBackgroundTask {
    signal?: AbortSignal
    expired?(): boolean
    progress(percent: number | null): void
    dispose(success?: boolean): Promise<void>
}
export interface AndroidBackgroundTaskBridge {
    begin(kind: MobileTaskKind): Promise<string | null>
    progress(id: string, percent: number): Promise<unknown>
    end(id: string): Promise<unknown>
}
declare global {
    interface Window { RisuBackgroundTasks?: AndroidBackgroundTaskBridge }
}
const active = new Set<object>()
const listeners = new Set<() => void>()
export const hasMobileBackgroundTasks = () => active.size > 0
export function subscribeMobileBackgroundTasks(listener: () => void): () => void {
    listeners.add(listener)
    return () => listeners.delete(listener)
}
function publish() { for (const listener of listeners) listener() }
export function measuredTaskPercent(completed: number, total?: number): number | null {
    return Number.isFinite(completed) && Number.isFinite(total) && total! > 0
        ? Math.max(0, Math.min(100, Math.round(completed * 100 / total!))) : null
}

export function beginMobileBackgroundTask(kind: MobileTaskKind, signal?: AbortSignal, userInitiated = false): MobileBackgroundTask | Promise<MobileBackgroundTask> {
    const noop: MobileBackgroundTask = { signal, progress() {}, async dispose() {} }
    if ((!isTauriAndroid && !isTauriIOS) || signal?.aborted) return noop
    // Register before awaiting native admission so Home cannot cancel work during acquisition.
    const owner = {}
    active.add(owner)
    publish()
    const release = () => { if (active.delete(owner)) publish() }
    return (async () => {
        let task: MobileBackgroundTask
        try {
            if (isTauriIOS) {
                task = await beginIOSBackgroundTask(kind, signal, release, userInitiated)
            } else {
                const bridge = window.RisuBackgroundTasks
                const id = await bridge?.begin(kind)
                if (!id || !bridge) { release(); return noop }
                const controller = new AbortController()
                const abort = () => controller.abort(signal?.reason)
                signal?.addEventListener('abort', abort, { once: true })
                if (signal?.aborted) abort()
                const expired = (event: Event) => {
                    if ((event as CustomEvent<string>).detail !== id) return
                    controller.abort(new BackgroundExecutionExpiredError())
                    release()
                }
                window.addEventListener('risunest-background-expired', expired)
                let pending = Promise.resolve()
                let reported: number | null | undefined
                let disposed = false
                task = {
                    signal: controller.signal,
                    progress(percent) {
                        if (disposed || controller.signal.aborted || percent === reported) return
                        reported = percent
                        pending = pending.then(async () => { await bridge.progress(id, percent ?? -1) }).catch(() => {})
                    },
                    async dispose() {
                        if (disposed) return
                        disposed = true
                        window.removeEventListener('risunest-background-expired', expired)
                        signal?.removeEventListener('abort', abort)
                        await pending
                        await bridge.end(id)
                    },
                }
            }
        } catch { release(); return noop }
        let disposed = false
        return {
            signal: task.signal,
            expired: () => task.expired?.() ?? isBackgroundExpiryReason(task.signal?.reason),
            progress: percent => task.progress(percent === null ? null : measuredTaskPercent(percent, 100)),
            async dispose(success = false) {
                if (disposed) return
                disposed = true
                try { await task.dispose(success) } catch {} finally { release() }
            },
        }
    })()
}

export function runWithMobileBackgroundTask<T>(
    kind: MobileTaskKind,
    operation: (task: MobileBackgroundTask) => Promise<T>,
    signal?: AbortSignal,
    userInitiated = false,
): Promise<T> {
    if (!isTauriAndroid && !isTauriIOS) return operation({ signal, progress() {}, async dispose() {} })
    return (async () => {
        const task = await beginMobileBackgroundTask(kind, signal, userInitiated)
        let success = false
        try {
            task.signal?.throwIfAborted()
            const result = await operation(task)
            success = !task.signal?.aborted && result !== null
            return result
        } catch (error) {
            if (signal?.aborted && isBackgroundExpiryReason(error)) {
                throw new DOMException('Operation cancelled', 'AbortError')
            }
            if (error instanceof Error && error.name === 'AbortError' && task.expired?.() && !signal?.aborted) {
                throw new BackgroundExecutionExpiredError()
            }
            throw error
        } finally { await task.dispose(success) }
    })()
}
