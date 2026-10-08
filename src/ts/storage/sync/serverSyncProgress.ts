import { invoke } from '@tauri-apps/api/core'
import type { languageEnglish } from 'src/lang/en'
import { formatElapsed } from '../../gui/nativeFileJobDialogModel'
import { formatRisuNestStorageBytes } from '../risuNestStorageDashboard'
import { formatRemaining } from './remainingTime'

export type ServerSyncLaneName = 'send' | 'receive' | 'hydrate' | 'binding' | 'assets'
export type ServerSyncNativeStep = 'idle' | 'preparing' | 'checking' | 'uploading' | 'confirming' | 'listing' | 'downloading' | 'staging'
/** One native transport lane. Every count only grows; a view subtracts what it read first. */
export interface ServerSyncLane {
    lane: ServerSyncLaneName
    active: boolean
    step: ServerSyncNativeStep
    listed: number
    /** Units a state read will list, as its pin reported them. */
    listedTotal: number
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

const COUNTS = ['listed', 'listedTotal', 'itemsDone', 'itemsTotal', 'filesDone', 'filesTotal', 'bytesDone', 'bytesTotal', 'sentBytes', 'receivedBytes', 'backlogDone'] as const

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
    /** Set while the attempt waits for the user, whose answer does not count as elapsed time. */
    pausedAt?: number
    /** Time the attempt waited for the user before `pausedAt`. */
    pausedMs?: number
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
    /** Changes a watched attempt had to upload when it began publishing. */
    plannedSend?: number
    /** Server changes after the receive cursor, as a watched attempt first read them. */
    plannedReceive?: number
    /** Time left of the running asset download, once its rate is known. */
    remainingMs?: number
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
    counters: { key: 'bytes' | 'rate' | 'items' | 'files' | 'elapsed' | 'remaining'; label: string; value: string }[]
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
type Work = { done: number; total: number }
const capped = (done: number, total: number): Work => ({ done: Math.min(done, total), total })
const share = (work: Work) => work.total > 0 ? Math.min(1, work.done / work.total) : 0
const findLane = (attempt: ServerSyncAttempt, name: ServerSyncLaneName) => attempt.lanes?.find(entry => entry.lane === name)
/**
 * Items a lane processed against the one total fixed before it ran: the upload planned when
 * publishing began, the server changes a receive first reported, or the changes a binding read.
 * Undefined while that total is unknown. Totals a lane adds page by page never count here.
 */
function itemWork(attempt: ServerSyncAttempt, name: 'send' | 'receive' | 'binding'): Work | undefined {
    const lane = findLane(attempt, name)
    if (!lane) return undefined
    if (name === 'send') return attempt.plannedSend ? capped(lane.itemsDone, attempt.plannedSend) : undefined
    // A receive that only reads back this device's own push reports no known work.
    if (name === 'receive') return lane.backlogDone > 0 ? capped(lane.backlogDone, attempt.plannedReceive ?? lane.backlogDone + lane.backlogLeft) : undefined
    return lane.itemsTotal > 0 ? capped(lane.itemsDone, lane.itemsTotal) : undefined
}
/** The work of the lane running a stage, against a total fixed before it ran. */
function stageWork(attempt: ServerSyncAttempt, lane: ServerSyncLane): Work | undefined {
    if (lane.assetScope) return lane.assetScope.total ? capped(lane.assetScope.done, lane.assetScope.total) : undefined
    if (lane.lane === 'send' || lane.lane === 'receive') return itemWork(attempt, lane.lane)
    if (lane.lane !== 'binding') return undefined
    if (lane.step === 'listing') return lane.listedTotal > 0 ? capped(lane.listed, lane.listedTotal) : undefined
    // A binding counts the changes it reads; the bodies it downloads after the last one and what it stages are not counted.
    const items = lane.step === 'downloading' ? itemWork(attempt, 'binding') : undefined
    return items && items.done < items.total ? items : undefined
}
/**
 * The running step measured in bytes against a total planned once: an asset download scope.
 * Uploads and receives plan their bytes page by page, so they have none.
 */
export function bytePhase(attempt: ServerSyncAttempt): { key: string; done: number; total: number; items: Work } | undefined {
    const lane = stageLane(attempt, attempt.active.at(-1) ?? attempt.current)
    if (!lane?.assetScope?.total || lane.step !== 'downloading' || lane.bytesTotal <= 0) return undefined
    return { key: `${lane.lane}:${lane.assetScope.id}`, done: lane.bytesDone, total: lane.bytesTotal, items: { done: lane.assetScope.done, total: lane.assetScope.total } }
}
function laneDetail(attempt: ServerSyncAttempt, lane: ServerSyncLane, text: Text): string {
    const work = stageWork(attempt, lane)
    if (work) return ratio(work.done, work.total)
    return lane.step === 'listing' && lane.listed > 0 ? text.itemsCount.replace('{0}', count(lane.listed)) : ''
}

/** Everything the progress panel shows for a running attempt. Only totals fixed before their work ran are shown. */
export function serverSyncProgressView(attempt: ServerSyncAttempt, text: Text, now: number): ServerSyncProgressView {
    const current = attempt.active.at(-1) ?? attempt.current
    const lane = stageLane(attempt, current)
    const work = lane && stageWork(attempt, lane)
    const stages = attempt.stages.map(stage => ({ stage, label: text.stage[stage], state: attempt.active.includes(stage) ? 'active' as const : 'done' as const }))
    const lanes = attempt.lanes ?? []
    const sum = (key: (typeof COUNTS)[number]) => lanes.reduce((total, entry) => total + entry[key], 0)
    const sent = sum('sentBytes'), received = sum('receivedBytes')
    const items = (['send', 'receive', 'binding'] as const).map(name => itemWork(attempt, name)).filter(entry => entry !== undefined)
    // Changes sent before their planned total is known are counted without one.
    const unplanned = attempt.plannedSend ? 0 : findLane(attempt, 'send')?.itemsDone ?? 0
    const itemsDone = items.reduce((total, entry) => total + entry.done, unplanned)
    const itemsTotal = items.reduce((total, entry) => total + entry.total, 0)
    const filesDone = sum('filesDone')
    const counters: ServerSyncProgressView['counters'] = []
    if (attempt.lanes) {
        counters.push({ key: 'bytes', label: text.verifiedBytes, value: `↑ ${bytes(sent)} · ↓ ${bytes(received)}` })
        counters.push({ key: 'rate', label: text.transferRate, value: attempt.rate === undefined ? '-' : `${bytes(attempt.rate)}/s` })
        if (unplanned > 0) counters.push({ key: 'items', label: text.progressItems, value: count(itemsDone) })
        else if (itemsTotal > 0) counters.push({ key: 'items', label: text.progressItems, value: ratio(itemsDone, itemsTotal) })
        // Files are planned per transfer group, so they have no total for the whole attempt.
        if (filesDone > 0) counters.push({ key: 'files', label: text.progressFiles, value: count(filesDone) })
    }
    counters.push({ key: 'elapsed', label: text.elapsed, value: formatElapsed((attempt.pausedAt ?? now) - attempt.startedAt - (attempt.pausedMs ?? 0)) })
    if (attempt.remainingMs !== undefined) counters.push({ key: 'remaining', label: text.remaining, value: formatRemaining(attempt.remainingMs) })
    return {
        // Asset downloads have one native step, so their stage names them more precisely.
        label: lane && current !== 'assets' ? text.activity[ACTIVITY[lane.step as Exclude<ServerSyncNativeStep, 'idle'>]] : text.progress[current],
        detail: lane ? laneDetail(attempt, lane, text) : '',
        fraction: work ? share(work) : null,
        stages,
        counters,
    }
}

/** What a routine attempt did and has left: changes sent and received, and assets downloaded after them. */
export function routineWork(attempt: ServerSyncAttempt): Record<ServerSyncRoutinePhase, Work> {
    const send = itemWork(attempt, 'send'), receive = itemWork(attempt, 'receive')
    return {
        changes: { done: (send?.done ?? 0) + (receive?.done ?? 0), total: (send?.total ?? 0) + (receive?.total ?? 0) },
        assets: assetWork(findLane(attempt, 'hydrate')),
    }
}
/** Whether a routine attempt moved anything, including changes sent before their total was known. */
export function routineMoved(attempt: ServerSyncAttempt, work = routineWork(attempt)): boolean {
    return work.changes.total + work.assets.total > 0 || (findLane(attempt, 'send')?.itemsDone ?? 0) > 0
}
function assetWork(lane: ServerSyncLane | undefined): Work {
    const scope = lane?.assetScope
    return scope?.total ? { done: scope.done, total: scope.total } : { done: 0, total: 0 }
}
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
    if (!routineMoved(attempt, work) && !attempt.active.includes('assets')) return undefined
    complete = complete && attempt.active.length === 0 && !attempt.lanes?.some(lane => lane.active || (lane.assetScope && !lane.assetScope.settled))
    if (complete) return { label: text.complete, fraction: 1, complete }
    const phase = routinePhase(attempt, work)
    if (phase === 'assets' && work.assets.total === 0) return { label: text.progress.assets, fraction: null, complete: false }
    // Changes sent before their planned total is known show no bar yet.
    if (phase === 'changes' && work.changes.total === 0) return undefined
    return { label: phase === 'assets' ? text.progress.assets : text.running, fraction: phase === 'assets' ? share(work.assets) : Math.max(attempt.peak?.changes ?? 0, share(work.changes)), complete }
}
