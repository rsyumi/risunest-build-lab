import { describe, expect, it, vi } from 'vitest'
import { languageKorean } from 'src/lang/ko'
import { createRateMeter, laneDeltas, routinePeak, serverSyncProgressView, serverSyncRoutineView, type ServerSyncAttempt, type ServerSyncLane } from './serverSyncProgress'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const text = languageKorean.risuNest.serverSync
const lane = (name: ServerSyncLane['lane'], counts: Partial<ServerSyncLane> = {}): ServerSyncLane => ({
    lane: name, active: false, step: 'idle', listed: 0, itemsDone: 0, itemsTotal: 0, filesDone: 0, filesTotal: 0, bytesDone: 0, bytesTotal: 0, sentBytes: 0, receivedBytes: 0, backlogDone: 0, backlogLeft: 0, ...counts,
})
const attempt = (fields: Partial<ServerSyncAttempt>): ServerSyncAttempt => ({ mode: 'full', startedAt: 0, stages: ['publishing'], active: ['publishing'], current: 'publishing', ...fields })

describe('server sync lane deltas', () => {
    it('subtracts the baseline lane by lane and keeps the live step', () => {
        const baseline = [lane('send', { sentBytes: 100, filesDone: 2 }), lane('receive', { receivedBytes: 50 })]
        const current = [lane('send', { active: true, step: 'uploading', sentBytes: 400, filesDone: 5, filesTotal: 9 }), lane('receive', { receivedBytes: 50 })]
        expect(laneDeltas(current, baseline)).toEqual([
            lane('send', { active: true, step: 'uploading', sentBytes: 300, filesDone: 3, filesTotal: 9 }),
            lane('receive'),
        ])
    })
    it('reads what is left of known work as it is now', () => {
        const baseline = [lane('receive', { backlogDone: 500, backlogLeft: 40 })]
        const current = [lane('receive', { active: true, step: 'downloading', backlogDone: 530, backlogLeft: 10 })]
        expect(laneDeltas(current, baseline)).toEqual([lane('receive', { active: true, step: 'downloading', backlogDone: 30, backlogLeft: 10 })])
    })
})

describe('server sync rate meter', () => {
    it('reports no rate from one sample and the recent slope afterwards', () => {
        const meter = createRateMeter(3000)
        meter.add(0, 0)
        expect(meter.rate()).toBeUndefined()
        for (let at = 500; at <= 5000; at += 500) meter.add(at, at * 2)
        expect(meter.rate()).toBe(2000)
        // A stall longer than the window reads as no transfer.
        for (let at = 5500; at <= 8500; at += 500) meter.add(at, 10_000)
        expect(meter.rate()).toBe(0)
        meter.reset()
        expect(meter.rate()).toBeUndefined()
    })
})

describe('server sync progress view', () => {
    it('names the native step and shows uploaded bytes against the planned total', () => {
        const view = serverSyncProgressView(attempt({ lanes: [lane('send', { active: true, step: 'uploading', filesDone: 1, filesTotal: 4, bytesDone: 1024 * 1024, bytesTotal: 4 * 1024 * 1024, sentBytes: 1024 * 1024 })], rate: 512 * 1024 }), text, 65_000)
        expect(view.label).toBe(text.activity.uploading)
        expect(view.detail).toBe('1 / 4 · 1.0 MiB / 4.0 MiB')
        expect(view.fraction).toBe(0.25)
        expect(view.counters.map(counter => [counter.key, counter.value])).toEqual([
            ['bytes', '↑ 1.0 MiB · ↓ 0 B'],
            ['rate', '512 KiB/s'],
            ['files', '1 / 4'],
            ['elapsed', '01:05'],
        ])
    })
    it('uses the file count when downloads report no byte total', () => {
        const view = serverSyncProgressView(attempt({ stages: ['downloading'], active: ['downloading'], current: 'downloading', lanes: [lane('receive', { active: true, step: 'downloading', filesDone: 3, filesTotal: 12, bytesDone: 3000 })] }), text, 0)
        expect(view.label).toBe(text.activity.downloading)
        expect(view.fraction).toBe(0.25)
    })
    it('reports no share while listing, and counts what was read', () => {
        const view = serverSyncProgressView(attempt({ stages: ['downloading'], active: ['downloading'], current: 'downloading', lanes: [lane('receive', { active: true, step: 'listing', listed: 300 })] }), text, 0)
        expect(view.label).toBe(text.activity.enumerating)
        expect(view.detail).toBe(text.itemsCount.replace('{0}', '300'))
        expect(view.fraction).toBeNull()
        const empty = serverSyncProgressView(attempt({ stages: ['downloading'], active: ['downloading'], current: 'downloading', lanes: [lane('receive', { active: true, step: 'listing' })] }), text, 0)
        expect(empty.detail).toBe('')
    })
    it('falls back to the stage label for work without a native step', () => {
        const view = serverSyncProgressView(attempt({ stages: ['downloading', 'applying'], active: ['applying'], current: 'applying', lanes: [lane('receive')] }), text, 0)
        expect(view.label).toBe(text.progress.applying)
        expect(view.detail).toBe('')
        expect(view.fraction).toBeNull()
    })
    it('marks every running stage active and the rest done, in the order they started', () => {
        const view = serverSyncProgressView(attempt({ stages: ['downloading', 'publishing', 'applying'], active: ['publishing'], current: 'applying' }), text, 0)
        expect(view.stages.map(stage => [stage.stage, stage.state])).toEqual([['downloading', 'done'], ['publishing', 'active'], ['applying', 'done']])
        expect(view.label).toBe(text.progress.publishing)
    })
    it('shows only the elapsed time before the first sample', () => {
        const view = serverSyncProgressView(attempt({}), text, 2_000)
        expect(view.counters).toEqual([{ key: 'elapsed', label: text.elapsed, value: '00:02' }])
    })
    it('lists items only when the step knows their total', () => {
        const counted = serverSyncProgressView(attempt({ lanes: [lane('send', { active: true, step: 'confirming', itemsDone: 0, itemsTotal: 5 })] }), text, 0)
        expect(counted.label).toBe(text.activity.confirming)
        expect(counted.counters.find(counter => counter.key === 'items')?.value).toBe('0 / 5')
        const uncounted = serverSyncProgressView(attempt({ lanes: [lane('send', { active: true, step: 'preparing' })] }), text, 0)
        expect(uncounted.counters.some(counter => counter.key === 'items')).toBe(false)
    })
})

describe('server sync routine bar', () => {
    const routine = (fields: Partial<ServerSyncAttempt>) => attempt({ mode: 'routine', ...fields })
    it('has no bar while nothing is sent or received', () => {
        expect(serverSyncRoutineView(routine({ lanes: [lane('send', { active: true, step: 'preparing' }), lane('receive')] }), text, false)).toBeUndefined()
        expect(serverSyncRoutineView(routine({}), text, true)).toBeUndefined()
    })
    it('fills toward the upload planned when publishing began', () => {
        const view = serverSyncRoutineView(routine({ plannedSend: 40, lanes: [lane('send', { active: true, step: 'confirming', itemsDone: 10, itemsTotal: 20 })] }), text, false)
        expect(view).toEqual({ label: text.running, fraction: 0.25, complete: false })
    })
    it('measures a receive by the server changes it has left, not by what an earlier attempt left', () => {
        const receiving = routine({ stages: ['downloading'], active: ['downloading'], current: 'downloading', lanes: [lane('receive', { active: true, step: 'downloading', backlogDone: 30, backlogLeft: 70, itemsDone: 3, itemsTotal: 3 })] })
        expect(serverSyncRoutineView(receiving, text, false)?.fraction).toBe(0.3)
        const stale = routine({ stages: ['publishing'], lanes: [lane('receive', { backlogLeft: 50 })] })
        expect(serverSyncRoutineView(stale, text, false)).toBeUndefined()
    })
    it('has no bar for a receive that only reads back a push from this device', () => {
        const view = serverSyncRoutineView(routine({ stages: ['downloading'], active: ['downloading'], current: 'downloading', lanes: [lane('receive', { active: true, step: 'downloading', listed: 2, itemsDone: 2, itemsTotal: 2 })] }), text, false)
        expect(view).toBeUndefined()
    })
    it('turns into an asset bar while the assets that followed download', () => {
        const lanes = [lane('receive', { backlogDone: 100 }), lane('hydrate', { active: true, step: 'downloading', assetScope: { id: 1, done: 1, total: 4, settled: false } })]
        const view = serverSyncRoutineView(routine({ stages: ['downloading', 'applying', 'assets'], active: ['assets'], current: 'assets', lanes }), text, false)
        expect(view).toEqual({ label: text.progress.assets, fraction: 0.25, complete: false })
    })
    it('holds the furthest point a bar reached when more work turns up', () => {
        let state = routine({ plannedSend: 4, lanes: [lane('send', { active: true, step: 'confirming', itemsDone: 3, itemsTotal: 4 })] })
        state = { ...state, peak: routinePeak(state) }
        expect(state.peak).toEqual({ changes: 0.75 })
        state = { ...state, lanes: [lane('send', { active: true, step: 'confirming', itemsDone: 4, itemsTotal: 12 })] }
        expect(serverSyncRoutineView(state, text, false)?.fraction).toBe(0.75)
        expect(routinePeak(state)).toEqual({ changes: 0.75 })
        state = { ...state, lanes: [lane('send', { active: true, step: 'confirming', itemsDone: 11, itemsTotal: 12 })] }
        expect(routinePeak(state)?.changes).toBeCloseTo(11 / 12)
    })
    it('uses one item scope across transfer groups and keeps bytes separate', () => {
        const current = lane('hydrate', { active: true, step: 'downloading', filesDone: 64, filesTotal: 64, bytesDone: 1000, bytesTotal: 1000, receivedBytes: 1050, assetScope: { id: 1, done: 65, total: 130, settled: false } })
        const state = routine({ active: ['assets'], current: 'assets', lanes: [current] })
        expect(serverSyncRoutineView(state, text, false)?.fraction).toBe(0.5)
        const full = serverSyncProgressView(state, text, 0)
        expect(full.fraction).toBe(0.5)
        expect(full.detail).toBe('65 / 130')
        expect(full.counters.find(counter => counter.key === 'bytes')?.value).toBe('↑ 0 B · ↓ 1.0 KiB')
    })
    it('does not carry a completed asset peak into a later hydration or its discovery', () => {
        const scope = { id: 2, done: 1, total: 8, settled: false }
        let state = routine({ active: ['assets'], current: 'assets', peak: { assets: 1 }, lanes: [lane('hydrate', { active: true, step: 'downloading', assetScope: scope })] })
        expect(serverSyncRoutineView(state, text, false)?.fraction).toBe(0.125)
        expect(routinePeak(state)?.assets).toBe(0.125)
        state = { ...state, lanes: [lane('hydrate', { active: true, step: 'downloading', bytesTotal: 100, bytesDone: 100, assetScope: { ...scope, total: null } })] }
        expect(serverSyncRoutineView(state, text, false)?.fraction).toBeNull()
        expect(serverSyncProgressView(state, text, 0).fraction).toBeNull()
    })
    it('reads scoped counts without subtracting an earlier attempt baseline', () => {
        const current = lane('hydrate', { assetScope: { id: 2, done: 2, total: 5, settled: false }, receivedBytes: 120 })
        const baseline = lane('hydrate', { assetScope: { id: 1, done: 100, total: 100, settled: true }, receivedBytes: 100 })
        expect(laneDeltas([current], [baseline])[0]).toEqual({ ...current, receivedBytes: 20 })
    })
    it('does not show an unchanged completed scope from before this attempt', () => {
        const prior = lane('hydrate', { assetScope: { id: 1, done: 20, total: 20, settled: true } })
        const lanes = laneDeltas([prior], [prior])
        expect(lanes[0].assetScope).toBeNull()
        expect(serverSyncRoutineView(routine({ lanes }), text, false)).toBeUndefined()
    })
    it('requires settlement after the last item and never marks failed scopes complete', () => {
        const scope = { id: 1, done: 4, total: 4, settled: false }
        const state = routine({ active: [], lanes: [lane('hydrate', { assetScope: scope })] })
        expect(serverSyncRoutineView(state, text, true)?.complete).toBe(false)
        expect(serverSyncRoutineView({ ...state, lanes: [lane('hydrate', { assetScope: { ...scope, settled: true } })] }, text, true)?.complete).toBe(true)
    })
    it('shows a finished attempt as a full, completed bar', () => {
        const view = serverSyncRoutineView(routine({ active: [], lanes: [lane('send', { itemsDone: 2, itemsTotal: 2 })] }), text, true)
        expect(view).toEqual({ label: text.complete, fraction: 1, complete: true })
    })
})
