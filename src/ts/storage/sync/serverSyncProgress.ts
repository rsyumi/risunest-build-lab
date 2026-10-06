import { invoke } from '@tauri-apps/api/core'
import type { languageEnglish } from 'src/lang/en'
import { formatElapsed } from '../../gui/nativeFileJobDialogModel'
import { formatRisuNestStorageBytes } from '../risuNestStorageDashboard'

export type ServerSyncLaneName = 'send' | 'receive' | 'hydrate' | 'binding' | 'assets'
export type ServerSyncNativeStep = 'idle' | 'preparing' | 'checking' | 'uploading' | 'confirming' | 'listing' | 'downloading' | 'staging'
/** One native transport lane. Every count only grows; a view subtracts what it read first. */
export interface ServerSyncLane {
    lane: ServerSyncLaneName
    active: boolean
    step: ServerSyncNativeStep
    listed: number
    itemsDone: number
    itemsTotal: number
    filesDone: number
    filesTotal: number
    bytesDone: number
    /** Planned only where sizes are known before the transfer: uploads and asset downloads. */
    bytesTotal: number
    sentBytes: number
    receivedBytes: number
    /** Server changes after the receive cursor. */
    backlogDone: number
    /** What remains of that work now. It falls as well as grows, so a view reads it as it is. */
    backlogLeft: number
    /** A finite hydration call, independent of cumulative transfer counters. */
    assetScope?: { id: number; done: number; total: number | null; settled: boolean } | null
}
export const readServerSyncLanes = () => invoke<ServerSyncLane[]>('server_sync_progress')

const COUNTS = ['listed', 'itemsDone', 'itemsTotal', 'filesDone', 'filesTotal', 'bytesDone', 'bytesTotal', 'sentBytes', 'receivedBytes', 'backlogDone'] as const

/** What each lane did after `baseline` was read. */
export function laneDeltas(current: ServerSyncLane[], baseline: ServerSyncLane[]): ServerSyncLane[] {
    return current.map(lane => {
        const base = baseline.find(entry => entry.lane === lane.lane)
        const delta = { ...lane }
        for (const key of COUNTS) delta[key] = Math.max(0, lane[key] - (base?.[key] ?? 0))
        const baseScope = base?.assetScope
        if (!lane.active && lane.assetScope && baseScope && lane.assetScope.id === baseScope.id && lane.assetScope.done === baseScope.done && lane.assetScope.settled === baseScope.settled) delta.assetScope = null
        return delta
    })
}

/** Bytes per second over the last few seconds of samples. */
export function createRateMeter(windowMs = 3000) {
    const samples: { at: number; bytes: number }[] = []
    return {
        add(at: number, bytes: number) {
            samples.push({ at, bytes })
            while (samples.length > 2 && at - samples[1].at >= windowMs) samples.shift()
        },
        rate(): number | undefined {
            if (samples.length < 2) return undefined
            const first = samples[0], last = samples[samples.length - 1]
            return last.at > first.at ? Math.max(0, last.bytes - first.bytes) * 1000 / (last.at - first.at) : undefined
        },
        reset() { samples.length = 0 },
    }
}

export type ServerSyncStage = 'preparing' | 'downloading' | 'applying' | 'refreshing' | 'publishing' | 'assets'
/** A routine sync shows one bar: for its changes, or for the assets it downloads after them. */
export type ServerSyncRoutinePhase = 'changes' | 'assets'
export interface ServerSyncAttempt {
    /** `full` connects a library or downloads every asset and shows each step; `routine` is automatic sync. */
    mode: 'full' | 'routine'
    startedAt: number
    /** When a finished attempt ended. */
    endedAt?: number
    /** Stages in the order this attempt first entered them. */
    stages: ServerSyncStage[]
    /** Stages running now, the latest last. */
    active: ServerSyncStage[]
    /** The stage entered last, shown while none is running. */
    current: ServerSyncStage
    /** Lane counts since the attempt was first watched, once a sample arrived. */
    lanes?: ServerSyncLane[]
    rate?: number
    /** Changes a watched routine attempt had to upload when it began publishing. */
    plannedSend?: number
    /** Change progress keeps its peak; asset progress belongs to its current finite scope. */
    peak?: Partial<Record<ServerSyncRoutinePhase, number>>
}

type Text = (typeof languageEnglish)['risuNest']['serverSync']
export interface ServerSyncProgressView {
    label: string
    detail: string
    /** Share done, or null while the running step reports no total. */
    fraction: number | null
    stages: { stage: ServerSyncStage; label: string; state: 'done' | 'active' }[]
    counters: { key: 'bytes' | 'rate' | 'items' | 'files' | 'elapsed'; label: string; value: string }[]
}

const STAGE_LANES: Record<ServerSyncStage, ServerSyncLaneName[]> = {
    preparing: ['binding'],
    downloading: ['binding', 'receive'],
    applying: [],
    refreshing: [],
    publishing: ['send'],
    assets: ['hydrate', 'assets'],
}
const ACTIVITY: Record<Exclude<ServerSyncNativeStep, 'idle'>, keyof Text['activity']> = {
    preparing: 'preparing',
    checking: 'verifying',
    uploading: 'uploading',
    confirming: 'confirming',
    listing: 'enumerating',
    downloading: 'downloading',
    staging: 'staging',
}
const count = (value: number) => value.toLocaleString()
const ratio = (done: number, total: number) => `${count(Math.min(done, total))} / ${count(total)}`
const bytes = (value: number) => formatRisuNestStorageBytes(value)

/** The lane doing the work of `stage` right now, if a native step runs it. */
function stageLane(attempt: ServerSyncAttempt, stage: ServerSyncStage): ServerSyncLane | undefined {
    return STAGE_LANES[stage]
        .map(name => attempt.lanes?.find(lane => lane.lane === name))
        .find(lane => lane?.active && lane.step !== 'idle')
}
function laneDetail(lane: ServerSyncLane, text: Text): string {
    if (lane.assetScope) return lane.assetScope.total === null ? '' : ratio(lane.assetScope.done, lane.assetScope.total)
    if (lane.step === 'listing') return lane.listed > 0 ? text.itemsCount.replace('{0}', count(lane.listed)) : ''
    if ((lane.step === 'uploading' || lane.step === 'downloading') && lane.filesTotal > 0) {
        const transferred = lane.bytesTotal > 0 ? `${bytes(lane.bytesDone)} / ${bytes(lane.bytesTotal)}` : bytes(lane.bytesDone)
        return `${ratio(lane.filesDone, lane.filesTotal)} · ${transferred}`
    }
    return lane.itemsTotal > 0 ? ratio(lane.itemsDone, lane.itemsTotal) : ''
}
function laneFraction(lane: ServerSyncLane): number | null {
    if (lane.assetScope) return lane.assetScope.total ? Math.min(1, lane.assetScope.done / lane.assetScope.total) : null
    if (lane.step !== 'uploading' && lane.step !== 'downloading') return null
    if (lane.bytesTotal > 0) return Math.min(1, lane.bytesDone / lane.bytesTotal)
    return lane.filesTotal > 0 ? Math.min(1, lane.filesDone / lane.filesTotal) : null
}

/** Everything the progress panel shows for a running attempt. */
export function serverSyncProgressView(attempt: ServerSyncAttempt, text: Text, now: number): ServerSyncProgressView {
    const current = attempt.active.at(-1) ?? attempt.current
    const lane = stageLane(attempt, current)
    const stages = attempt.stages.map(stage => ({ stage, label: text.stage[stage], state: attempt.active.includes(stage) ? 'active' as const : 'done' as const }))
    const lanes = attempt.lanes ?? []
    const sum = (key: (typeof COUNTS)[number]) => lanes.reduce((total, entry) => total + entry[key], 0)
    const sent = sum('sentBytes'), received = sum('receivedBytes')
    const itemsDone = sum('itemsDone'), itemsTotal = sum('itemsTotal')
    const filesDone = sum('filesDone'), filesTotal = sum('filesTotal')
    const counters: ServerSyncProgressView['counters'] = []
    if (attempt.lanes) {
        counters.push({ key: 'bytes', label: text.verifiedBytes, value: `↑ ${bytes(sent)} · ↓ ${bytes(received)}` })
        counters.push({ key: 'rate', label: text.transferRate, value: attempt.rate === undefined ? '-' : `${bytes(attempt.rate)}/s` })
        if (itemsTotal > 0) counters.push({ key: 'items', label: text.progressItems, value: ratio(itemsDone, itemsTotal) })
        if (filesTotal > 0) counters.push({ key: 'files', label: text.progressFiles, value: ratio(filesDone, filesTotal) })
    }
    counters.push({ key: 'elapsed', label: text.elapsed, value: formatElapsed(now - attempt.startedAt) })
    return {
        // Asset downloads have one native step, so their stage names them more precisely.
        label: lane && current !== 'assets' ? text.activity[ACTIVITY[lane.step as Exclude<ServerSyncNativeStep, 'idle'>]] : text.progress[current],
        detail: lane ? laneDetail(lane, text) : '',
        fraction: lane ? laneFraction(lane) : null,
        stages,
        counters,
    }
}

type Work = { done: number; total: number }
/** Work a lane measured before running it, once this attempt has seen that lane at work. */
function backlog(lane: ServerSyncLane | undefined): Work | undefined {
    if (!lane || !(lane.active || lane.backlogDone > 0) || lane.backlogDone + lane.backlogLeft === 0) return undefined
    return { done: lane.backlogDone, total: lane.backlogDone + lane.backlogLeft }
}
/** What a routine attempt did and has left: changes sent and received, and assets downloaded after them. */
export function routineWork(attempt: ServerSyncAttempt): Record<ServerSyncRoutinePhase, Work> {
    const lane = (name: ServerSyncLaneName) => attempt.lanes?.find(entry => entry.lane === name)
    const send = lane('send')
    const sent = send?.itemsDone ?? 0
    // A receive that only reads back this device's own push reports no known work.
    const received = backlog(lane('receive')) ?? { done: 0, total: 0 }
    return {
        changes: { done: sent + received.done, total: Math.max(sent, send?.itemsTotal ?? 0, attempt.plannedSend ?? 0) + received.total },
        assets: assetWork(lane('hydrate')),
    }
}
function assetWork(lane: ServerSyncLane | undefined): Work {
    const scope = lane?.assetScope
    return scope?.total ? { done: scope.done, total: scope.total } : { done: 0, total: 0 }
}
const share = (work: Work) => work.total > 0 ? Math.min(1, work.done / work.total) : 0
function routinePhase(attempt: ServerSyncAttempt, work: Record<ServerSyncRoutinePhase, Work>): ServerSyncRoutinePhase {
    return attempt.active.includes('assets') || (work.assets.total > 0 && work.changes.total === 0) ? 'assets' : 'changes'
}
/** `peak` after the latest counts, for the bar the attempt shows now. */
export function routinePeak(attempt: ServerSyncAttempt): ServerSyncAttempt['peak'] {
    const work = routineWork(attempt), phase = routinePhase(attempt, work)
    return { ...attempt.peak, [phase]: phase === 'assets' ? share(work.assets) : Math.max(attempt.peak?.changes ?? 0, share(work.changes)) }
}

export interface ServerSyncRoutineView {
    label: string
    fraction: number | null
    complete: boolean
}
/** The one bar of a routine attempt, or undefined while it has nothing to move. */
export function serverSyncRoutineView(attempt: ServerSyncAttempt, text: Text, complete: boolean): ServerSyncRoutineView | undefined {
    const work = routineWork(attempt)
    if (work.changes.total + work.assets.total === 0 && !attempt.active.includes('assets')) return undefined
    complete = complete && attempt.active.length === 0 && !attempt.lanes?.some(lane => lane.active || (lane.assetScope && !lane.assetScope.settled))
    if (complete) return { label: text.complete, fraction: 1, complete }
    const phase = routinePhase(attempt, work)
    if (phase === 'assets' && work.assets.total === 0) return { label: text.progress.assets, fraction: null, complete: false }
    return { label: phase === 'assets' ? text.progress.assets : text.running, fraction: phase === 'assets' ? share(work.assets) : Math.max(attempt.peak?.changes ?? 0, share(work.changes)), complete }
}
