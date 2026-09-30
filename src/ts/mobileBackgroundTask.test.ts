import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
vi.mock('./platform', async original => ({
    ...await original<typeof import('./platform')>(), isTauriAndroid: true, isTauriIOS: false,
}))
vi.mock('./androidNativeControl', () => ({}))
import { beginMobileBackgroundTask, hasMobileBackgroundTasks, measuredTaskPercent, runWithMobileBackgroundTask } from './mobileBackgroundTask'
import { runSharedNativeFileOperation, cancelActiveNativeFileOperation } from './storage/nativeFileJobManager'
import { createExternalStorageController } from './storage/sync/external/controller'
import type { ExternalJobSummary, ExternalStorageState } from './storage/sync/external/types'

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>(done => { resolve = done })
    return { promise, resolve }
}
const bridge = { begin: vi.fn(), progress: vi.fn(), end: vi.fn() }
beforeEach(() => {
    let id = 0
    bridge.begin.mockReset().mockImplementation(async () => `task-${++id}`)
    bridge.progress.mockReset().mockResolvedValue(undefined)
    bridge.end.mockReset().mockResolvedValue(undefined)
    window.RisuBackgroundTasks = bridge
})
afterEach(() => {
    delete window.RisuBackgroundTasks
    expect(hasMobileBackgroundTasks()).toBe(false)
})

describe('mobile background task lifetime', () => {
    it('retains pending admission and overlapping owners until each finishes', async () => {
        const admission = deferred<string>()
        bridge.begin.mockReturnValueOnce(admission.promise)
        const first = beginMobileBackgroundTask('backup')
        expect(hasMobileBackgroundTasks()).toBe(true)
        const second = await beginMobileBackgroundTask('sync')
        admission.resolve('pending')
        const task = await first
        await second.dispose(true)
        expect(hasMobileBackgroundTasks()).toBe(true)
        await task.dispose(true)
        await task.dispose(true)
        expect(bridge.end.mock.calls).toEqual([['task-1'], ['pending']])
    })

    it.each([null, new Error('unavailable')])('keeps foreground work available when admission fails: %s', async failure => {
        if (failure) bridge.begin.mockRejectedValueOnce(failure)
        else bridge.begin.mockResolvedValueOnce(null)
        await expect(runWithMobileBackgroundTask('restore', async () => 'done')).resolves.toBe('done')
        expect(bridge.end).not.toHaveBeenCalled()
    })

    it('releases on errors and cancellation during acquisition', async () => {
        await expect(runWithMobileBackgroundTask('export', async () => { throw new Error('failed') })).rejects.toThrow('failed')
        const pending = deferred<string>()
        bridge.begin.mockReturnValueOnce(pending.promise)
        const abort = new AbortController()
        const operation = vi.fn(async () => {})
        const result = runWithMobileBackgroundTask('backup', operation, abort.signal)
        abort.abort()
        pending.resolve('cancelled')
        await expect(result).rejects.toMatchObject({ name: 'AbortError' })
        expect(operation).not.toHaveBeenCalled()
        expect(bridge.end).toHaveBeenCalledWith('cancelled')
    })

    it('serializes progress, clears unknown totals, and ignores reports after disposal', async () => {
        const task = await beginMobileBackgroundTask('backup')
        task.progress(40)
        task.progress(40)
        task.progress(null)
        task.progress(20)
        await task.dispose(true)
        task.progress(100)
        expect(bridge.progress.mock.calls).toEqual([['task-1', 40], ['task-1', -1], ['task-1', 20]])
        expect(bridge.progress.mock.invocationCallOrder.at(-1)!).toBeLessThan(bridge.end.mock.invocationCallOrder[0])
        expect(measuredTaskPercent(2, 8)).toBe(25)
        expect(measuredTaskPercent(2, 0)).toBeNull()
        expect(measuredTaskPercent(2)).toBeNull()
        expect(measuredTaskPercent(Infinity, 8)).toBeNull()
    })

    it('does not abort on Home and handles only its native expiration event', async () => {
        const task = await beginMobileBackgroundTask('sync')
        document.dispatchEvent(new Event('visibilitychange'))
        window.dispatchEvent(new CustomEvent('risunest-background-expired', { detail: 'other' }))
        expect(task.signal?.aborted).toBe(false)
        window.dispatchEvent(new CustomEvent('risunest-background-expired', { detail: 'task-1' }))
        expect(task.signal?.aborted).toBe(true)
        expect(hasMobileBackgroundTasks()).toBe(false)
        await task.dispose()
    })

    it('protects the entire file operation and reports the same measured bytes', async () => {
        const finish = deferred<void>()
        let signal: AbortSignal | undefined
        const operation = runSharedNativeFileOperation('export', 'synthetic-backup', async context => {
            signal = context.signal
            context.onStatus({
                jobId: 'synthetic', kind: 'export-portable-backup', state: 'running', phase: 'writing-export',
                progress: { completedBytes: 25, totalBytes: 100, completedItems: 0 },
            })
            await finish.promise
        }, { format: 'library-backup' })
        await vi.waitFor(() => expect(bridge.progress).toHaveBeenCalledWith('task-1', 25))
        expect(bridge.begin).toHaveBeenCalledWith('backup')
        expect(bridge.end).not.toHaveBeenCalled()
        cancelActiveNativeFileOperation()
        expect(signal?.aborted).toBe(true)
        finish.resolve()
        await operation
        expect(bridge.end).toHaveBeenCalledExactlyOnceWith('task-1')
    })

    it('protects external native polling and releases a blocked job', async () => {
        const terminal = deferred<ExternalJobSummary>()
        const job: ExternalJobSummary = { id: 'job', connectionId: 'connection', kind: 'backup', state: 'running', phase: 'upload', completedBytes: '5', totalBytes: '10', completedItems: '0', startedAtMs: '1', updatedAtMs: '1' }
        const native = {
            startJob: vi.fn(async () => job), getJob: vi.fn(() => terminal.promise), cancelJob: vi.fn(async () => ({ ...job, state: 'cancelled' as const })),
        }
        const controller = createExternalStorageController(native, { supported: true, selection: { kind: 'none', selectionEpoch: '0', paused: false, decisionRequired: false }, connections: [], jobs: [] } as ExternalStorageState, { wait: async () => {} })
        const result = runWithMobileBackgroundTask('backup', backgroundTask => controller.request({ connectionId: 'connection', kind: 'backup', targetRevision: '1', reason: 'manual', session: { kind: 'foreground', id: 'session' }, backgroundTask }))
        await vi.waitFor(() => expect(bridge.progress).toHaveBeenCalledWith('task-1', 50))
        expect(bridge.begin).toHaveBeenCalledOnce()
        document.dispatchEvent(new Event('visibilitychange'))
        expect(native.cancelJob).not.toHaveBeenCalled()
        terminal.resolve({ ...job, state: 'failed' })
        await expect(result).resolves.toMatchObject({ kind: 'blocked' })
        await vi.waitFor(() => expect(bridge.end).toHaveBeenCalledWith('task-1'))
    })
})

it.each([false, true])('distinguishes OS expiry after inner cancellation, user cancel wins: %s', async userCancel => {
    const caller = new AbortController()
    const operation = runWithMobileBackgroundTask('import', async task => {
        await new Promise<void>(resolve => task.signal!.addEventListener('abort', () => resolve(), { once: true }))
        if (userCancel) caller.abort()
        throw new DOMException('inner cancellation', 'AbortError')
    }, caller.signal)
    await vi.waitFor(() => expect(hasMobileBackgroundTasks()).toBe(true))
    await Promise.resolve()
    window.dispatchEvent(new CustomEvent('risunest-background-expired', { detail: 'task-1' }))
    const { isBackgroundExpiryReason } = await import('./iosNative')
    await expect(operation.catch(error => isBackgroundExpiryReason(error))).resolves.toBe(!userCancel)
})
