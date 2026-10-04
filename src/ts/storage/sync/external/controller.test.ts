import { describe, expect, it, vi } from 'vitest'
import { createExternalStorageController, type ExternalStorageJobBridge } from './controller'
import type { ExternalJobSummary, ExternalStorageState } from './types'

const state: ExternalStorageState = {
    supported: true,
    selection: {
        kind: 'external',
        connectionId: 'sync-1',
        selectionEpoch: 'opaque-selection',
        paused: false,
    },
    connections: [],
    jobs: [],
}

function job(
    id: string,
    status: ExternalJobSummary['state'],
    publishedRevision?: string,
): ExternalJobSummary {
    return {
        id,
        connectionId: 'sync-1',
        kind: 'backup',
        state: status,
        phase: status,
        completedBytes: '0',
        completedItems: '0',
        startedAtMs: '1',
        updatedAtMs: '1',
        result: publishedRevision ? { publishedRevision: publishedRevision as `${number}` } : undefined,
    }
}

function cleanupJob(): ExternalJobSummary {
    return {
        ...job('cleanup-job', 'succeeded'), kind: 'cleanup',
        result: { deletedObjects: '0', deletedBytes: '0', stopReason: 'complete' },
    }
}

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>(settle => {
        resolve = settle
    })
    return { promise, resolve }
}

describe('external storage controller', () => {
    it('retains structured native rejection causes', async () => {
        const cause = { kind: 'preconditionFailed', httpStatus: null, retryAtMs: null }
        const controller = createExternalStorageController({ startJob: vi.fn().mockRejectedValue(cause), getJob: vi.fn(), cancelJob: vi.fn() }, state)
        await expect(controller.request({ connectionId: 'sync-1', kind: 'backup', targetRevision: '1', reason: 'manual',
            session: { kind: 'foreground', id: 'session' } })).resolves.toMatchObject({ kind: 'blocked', reason: 'preconditionFailed', cause })
    })

    it('routes a retained conflict before its precondition error detail', async () => {
        const conflict = { ...job('conflict', 'conflict'),
            error: { code: 'preconditionFailed', message: '', action: 'none' as const, retryable: false } }
        const controller = createExternalStorageController({ startJob: vi.fn().mockResolvedValue(conflict), getJob: vi.fn(), cancelJob: vi.fn() }, state)
        await expect(controller.request({ connectionId: 'sync-1', kind: 'backup', targetRevision: '1', reason: 'manual',
            session: { kind: 'foreground', id: 'session' } })).resolves.toMatchObject({ kind: 'blocked', reason: 'external-storage-conflict',
            error: conflict.error, job: { id: 'conflict' } })
    })

    it('finishes cleanup without inventing a published revision', async () => {
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async (): Promise<ExternalJobSummary> => ({ ...job('cleanup-1', 'succeeded'), kind: 'cleanup',
                result: { deletedObjects: '2', deletedBytes: '100', stopReason: 'complete' } })),
            getJob: vi.fn(), cancelJob: vi.fn(),
        }
        const controller = createExternalStorageController(bridge, state)
        const result = await controller.request({ connectionId: 'sync-1', kind: 'cleanup', targetRevision: '0',
            reason: 'manual', session: { kind: 'foreground', id: 'session' } })
        expect(result).toMatchObject({ kind: 'complete', job: { result: { deletedObjects: '2' } } })
        expect(bridge.startJob).toHaveBeenCalledOnce()
        expect(vi.mocked(bridge.startJob).mock.calls[0][0]).not.toHaveProperty('targetRevision')
    })

    it('keeps the latest dirty revision while resolving an earlier manual goal', async () => {
        const firstPoll = deferred<ExternalJobSummary>()
        let starts = 0
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async request => job(`job-${++starts}`, 'running', request.targetRevision)),
            getJob: vi.fn(async id => {
                if (id === 'job-1') return firstPoll.promise
                return job(id, 'succeeded', '30')
            }),
            cancelJob: vi.fn(async id => job(id, 'cancelled')),
        }
        const controller = createExternalStorageController(bridge, state, {
            wait: async () => {},
        })
        const manual = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '10',
            reason: 'manual', session: { kind: 'foreground', id: 'session-1' },
        })
        await vi.waitFor(() => expect(bridge.getJob).toHaveBeenCalledWith('job-1'))
        const newer = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '30',
            reason: 'automatic', session: { kind: 'foreground', id: 'session-1' },
        })
        firstPoll.resolve(job('job-1', 'succeeded', '10'))
        await expect(manual).resolves.toMatchObject({ kind: 'complete', revision: '10' })
        await expect(newer).resolves.toMatchObject({ kind: 'complete', revision: '30' })
        expect(bridge.startJob).toHaveBeenCalledTimes(2)
        expect(bridge.startJob).toHaveBeenLastCalledWith(expect.objectContaining({
            targetRevision: '30',
        }))
    })

    it('runs backup and cleanup goals as distinct native jobs', async () => {
        const firstPoll = deferred<ExternalJobSummary>()
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async request => request.kind === 'backup'
                ? job('backup-job', 'running')
                : cleanupJob()),
            getJob: vi.fn(async () => firstPoll.promise),
            cancelJob: vi.fn(),
        }
        const controller = createExternalStorageController(bridge, state, { wait: async () => {} })
        const backup = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '8',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })
        await vi.waitFor(() => expect(bridge.getJob).toHaveBeenCalledWith('backup-job'))
        const cleanup = controller.request({
            connectionId: 'sync-1', kind: 'cleanup', targetRevision: '0',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })
        firstPoll.resolve(job('backup-job', 'succeeded', '8'))

        await expect(backup).resolves.toMatchObject({ kind: 'complete', job: { id: 'backup-job' } })
        await expect(cleanup).resolves.toMatchObject({ kind: 'complete', job: { id: 'cleanup-job' } })
        expect(bridge.startJob).toHaveBeenCalledTimes(2)
        expect(bridge.startJob).toHaveBeenNthCalledWith(1, expect.objectContaining({ kind: 'backup' }))
        expect(bridge.startJob).toHaveBeenNthCalledWith(2, expect.objectContaining({ kind: 'cleanup' }))
    })

    it('runs another kind after a blocked attempt without sharing its result', async () => {
        const blocked = {
            ...job('backup-job', 'waiting'),
            phase: 'paused',
            error: { code: 'offline', message: 'Offline', retryable: true, action: 'retry' as const },
        }
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async request => request.kind === 'backup'
                ? blocked
                : cleanupJob()),
            getJob: vi.fn(),
            cancelJob: vi.fn(),
        }
        const controller = createExternalStorageController(bridge, state)
        const backup = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '8',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })
        const cleanup = controller.request({
            connectionId: 'sync-1', kind: 'cleanup', targetRevision: '0',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })

        await expect(backup).resolves.toMatchObject({ kind: 'blocked', job: { id: 'backup-job' } })
        await expect(cleanup).resolves.toMatchObject({ kind: 'complete', job: { id: 'cleanup-job' } })
        expect(bridge.startJob).toHaveBeenCalledTimes(2)
    })

    it('aborts an active request without discarding a coalesced goal', async () => {
        let starts = 0
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => ++starts === 1
                ? job('first-job', 'running')
                : job('foreground-job', 'succeeded', '20')),
            getJob: vi.fn(),
            cancelJob: vi.fn(async id => job(id, 'cancelled')),
        }
        const controller = createExternalStorageController(bridge, state, {
            wait: async (_delay, signal) => new Promise<void>((resolve, reject) => {
                if (signal?.aborted) reject(signal.reason)
                else signal?.addEventListener('abort', () => reject(signal.reason), { once: true })
            }),
        })
        const abort = new AbortController()
        const removeListener = vi.spyOn(abort.signal, 'removeEventListener')
        const first = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '12', reason: 'manual',
            session: { kind: 'foreground', id: 'foreground-1' }, signal: abort.signal,
        })
        await vi.waitFor(() => expect(bridge.startJob).toHaveBeenCalledTimes(1))
        const foreground = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '20',
            reason: 'automatic', session: { kind: 'foreground', id: 'foreground-1' },
        })

        abort.abort(new Error('request cancelled'))

        await expect(first).resolves.toEqual({ kind: 'cancelled' })
        await vi.waitFor(() => expect(bridge.cancelJob).toHaveBeenCalledWith('first-job'))
        await expect(foreground).resolves.toMatchObject({ kind: 'complete', revision: '20' })
        expect(removeListener).toHaveBeenCalledWith('abort', expect.any(Function))
        expect(bridge.startJob).toHaveBeenCalledTimes(2)
        expect(bridge.startJob).toHaveBeenLastCalledWith(expect.objectContaining({
            reason: 'automatic', session: 'foreground', sessionId: 'foreground-1',
        }))
    })

    it('cancels a native job returned after its caller already aborted', async () => {
        const started = deferred<ExternalJobSummary>()
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => started.promise),
            getJob: vi.fn(),
            cancelJob: vi.fn(async id => job(id, 'cancelled')),
        }
        const controller = createExternalStorageController(bridge, state)
        const abort = new AbortController()
        const pending = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '12', reason: 'manual',
            session: { kind: 'foreground', id: 'foreground-1' }, signal: abort.signal,
        })
        abort.abort(new Error('request cancelled'))
        started.resolve(job('late-job', 'running'))

        await expect(pending).resolves.toEqual({ kind: 'cancelled' })
        await vi.waitFor(() => expect(bridge.cancelJob).toHaveBeenCalledWith('late-job'))
        expect(bridge.getJob).not.toHaveBeenCalled()
    })

    it.each(['backup', 'cleanup'] as const)('cancels a late-started %s job after the connection was cancelled', async kind => {
        const started = deferred<ExternalJobSummary>()
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => started.promise),
            getJob: vi.fn(),
            cancelJob: vi.fn(async id => ({ ...job(id, 'cancelled'), kind })),
        }
        const controller = createExternalStorageController(bridge, state)
        const pending = controller.request({
            connectionId: 'sync-1', kind, targetRevision: '12',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })
        expect(bridge.startJob).toHaveBeenCalledTimes(1)
        const cancelled = controller.cancel('sync-1')
        started.resolve({ ...job('late-job', 'running'), kind })

        await cancelled
        await expect(pending).resolves.toEqual({ kind: 'cancelled' })
        expect(bridge.cancelJob).toHaveBeenCalledExactlyOnceWith('late-job')
        expect(bridge.getJob).not.toHaveBeenCalled()
    })

    it('keeps a replacement request separate until the cancelled native start is settled', async () => {
        const started = deferred<ExternalJobSummary>()
        const cancelled = deferred<ExternalJobSummary>()
        let starts = 0
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => ++starts === 1
                ? started.promise
                : job('replacement-job', 'succeeded', '20')),
            getJob: vi.fn(async id => job(id, 'succeeded', '12')),
            cancelJob: vi.fn(async () => cancelled.promise),
        }
        const controller = createExternalStorageController(bridge, state, { wait: async () => {} })
        const first = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '12',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })
        const stopping = controller.cancel('sync-1')
        const replacement = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '20',
            reason: 'automatic', session: { kind: 'foreground', id: 'foreground-1' },
        })
        started.resolve(job('late-job', 'running'))

        await vi.waitFor(() => expect(bridge.cancelJob).toHaveBeenCalledExactlyOnceWith('late-job'))
        expect(bridge.startJob).toHaveBeenCalledTimes(1)
        expect(bridge.getJob).not.toHaveBeenCalled()
        cancelled.resolve(job('late-job', 'cancelled'))
        await stopping
        await expect(first).resolves.toEqual({ kind: 'cancelled' })
        await expect(replacement).resolves.toMatchObject({ kind: 'complete', revision: '20' })
        expect(bridge.startJob).toHaveBeenCalledTimes(2)
    })

    it.each(['connection', 'caller'] as const)('does not start a queued replacement cancelled by its %s', async cancellation => {
        const started = deferred<ExternalJobSummary>()
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => started.promise),
            getJob: vi.fn(),
            cancelJob: vi.fn(async id => job(id, 'cancelled')),
        }
        const controller = createExternalStorageController(bridge, state)
        const first = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '12',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })
        const stopping = controller.cancel('sync-1')
        const abort = new AbortController()
        const replacement = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '20',
            reason: 'automatic', session: { kind: 'foreground', id: 'foreground-1' },
            signal: abort.signal,
        })
        const stopReplacement = cancellation === 'connection'
            ? controller.cancel('sync-1')
            : Promise.resolve(abort.abort())
        started.resolve(job('late-job', 'running'))

        await Promise.all([stopping, stopReplacement])
        await expect(first).resolves.toEqual({ kind: 'cancelled' })
        await expect(replacement).resolves.toEqual({ kind: 'cancelled' })
        expect(bridge.startJob).toHaveBeenCalledTimes(1)
        expect(bridge.cancelJob).toHaveBeenCalledExactlyOnceWith('late-job')
        expect(bridge.getJob).not.toHaveBeenCalled()
    })

    it('waits for native cancellation of an aborted job before starting the next goal', async () => {
        const polled = deferred<ExternalJobSummary>()
        const cancelled = deferred<ExternalJobSummary>()
        let starts = 0
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => ++starts === 1
                ? job('first-job', 'running')
                : job('foreground-job', 'succeeded', '20')),
            getJob: vi.fn(async () => polled.promise),
            cancelJob: vi.fn(async () => cancelled.promise),
        }
        const controller = createExternalStorageController(bridge, state, {
            wait: async () => {},
        })
        const abort = new AbortController()
        const first = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '12', reason: 'manual',
            session: { kind: 'foreground', id: 'foreground-1' }, signal: abort.signal,
        })
        await vi.waitFor(() => expect(bridge.getJob).toHaveBeenCalledWith('first-job'))
        const foreground = controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '20',
            reason: 'automatic', session: { kind: 'foreground', id: 'foreground-1' },
        })

        abort.abort(new Error('request cancelled'))
        polled.resolve(job('first-job', 'running'))

        await expect(first).resolves.toEqual({ kind: 'cancelled' })
        await vi.waitFor(() => expect(bridge.cancelJob).toHaveBeenCalledWith('first-job'))
        expect(bridge.startJob).toHaveBeenCalledTimes(1)
        cancelled.resolve(job('first-job', 'cancelled'))
        await expect(foreground).resolves.toMatchObject({ kind: 'complete', revision: '20' })
        expect(bridge.startJob).toHaveBeenCalledTimes(2)
    })

    it('does not accept success without native publication evidence', async () => {
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => job('job-1', 'succeeded')),
            getJob: vi.fn(),
            cancelJob: vi.fn(),
        }
        const controller = createExternalStorageController(bridge, state)
        await expect(controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '10',
            reason: 'manual', session: { kind: 'foreground', id: 'foreground-1' },
        })).resolves.toEqual(expect.objectContaining({
            kind: 'blocked', reason: 'external-storage-published-revision-missing',
        }))
    })

    it('backs off native polling while a persisted job waits', async () => {
        const waits: number[] = []
        const capturing = { ...job('job-1', 'waiting'), phase: 'device-capture' }
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => capturing),
            getJob: vi.fn(async () => job('job-1', 'succeeded', '4')),
            cancelJob: vi.fn(),
        }
        const controller = createExternalStorageController(bridge, state, {
            wait: async delay => { waits.push(delay) },
        })
        await controller.request({
            connectionId: 'sync-1', kind: 'backup', targetRevision: '4',
            reason: 'automatic', session: { kind: 'foreground', id: 'foreground-1' },
        })
        expect(waits).toEqual([5_000])
    })

    it('returns a paused native wait and lets an explicit retry resume the retained job', async () => {
        let attempts = 0
        const paused = {
            ...job('job-1', 'waiting'),
            phase: 'paused',
            error: {
                code: 'offline', message: 'Offline', retryable: true, action: 'retry' as const,
            },
        }
        const bridge: ExternalStorageJobBridge = {
            startJob: vi.fn(async () => ++attempts === 1
                ? paused
                : job('job-1', 'succeeded', '4')),
            getJob: vi.fn(),
            cancelJob: vi.fn(),
        }
        const controller = createExternalStorageController(bridge, state)
        const request = {
            connectionId: 'sync-1', kind: 'backup' as const, targetRevision: '4' as const,
            reason: 'manual' as const, session: { kind: 'foreground' as const, id: 'foreground-1' },
        }
        await expect(controller.request(request)).resolves.toMatchObject({
            kind: 'blocked', reason: 'offline', job: paused,
        })
        await expect(controller.request(request)).resolves.toMatchObject({
            kind: 'complete', revision: '4',
        })
        expect(bridge.startJob).toHaveBeenCalledTimes(2)
        expect(bridge.getJob).not.toHaveBeenCalled()
    })
})
