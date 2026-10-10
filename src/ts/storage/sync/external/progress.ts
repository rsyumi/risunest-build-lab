import { Channel, invoke } from '@tauri-apps/api/core'
import type { ExternalJobSummary, ExternalSnapshotExportProgress } from './types'
import { externalJobIsActive, externalJobProgress } from './connection'
import type { TransferRateSample } from '../transferRate'

export type ExternalStage = 'checking' | 'preparing' | 'uploading' | 'downloading' | 'applying' | 'finalizing' | 'waiting'
export interface ExternalTransferSnapshot {
    sequence: number
    stage: ExternalStage
    preparedBytes: string
    uploadedBytes: string
    downloadedBytes: string
    uploadedObjects: string
    downloadedObjects: string
    network?: TransferRateSample
}
export interface ExternalOperationProgress {
    id: string
    connectionId: string
    kind: 'sync' | 'binding' | 'download' | 'export'
    authority?: string
    stage: ExternalStage
    state: 'running' | 'complete' | 'failed' | 'cancelled'
    visible: boolean
    amounts: Omit<ExternalTransferSnapshot, 'sequence' | 'stage' | 'network'>
    network?: TransferRateSample
    completedItems?: number
    totalItems?: number
    exported?: ExternalSnapshotExportProgress
    previous?: ExternalOperationProgress
}
const emptyAmounts = () => ({ preparedBytes: '0', uploadedBytes: '0', downloadedBytes: '0', uploadedObjects: '0', downloadedObjects: '0' })
const operations = new Map<string, ExternalOperationProgress>()
const listeners = new Set<(value: ReadonlyMap<string, ExternalOperationProgress>) => void>()
const key = (id: string, kind: ExternalOperationProgress['kind']) => `${id}:${kind}`
export function validTransferRateSample(sample: TransferRateSample | undefined): sample is TransferRateSample {
    return !!sample && typeof sample.id === 'string' && !!sample.id && Number.isSafeInteger(sample.atMs) && sample.atMs >= 0
        && [sample.sentBytes, sample.receivedBytes].every(value => /^\d+$/.test(value) && Number.isSafeInteger(Number(value)))
        && typeof sample.sending === 'boolean' && typeof sample.receiving === 'boolean'
}
function emit() { for (const listener of listeners) listener(new Map(operations)) }
export function subscribeExternalProgress(listener: (value: ReadonlyMap<string, ExternalOperationProgress>) => void) {
    listeners.add(listener)
    listener(new Map(operations))
    return () => { listeners.delete(listener) }
}
export function externalProgressFor(value: ReadonlyMap<string, ExternalOperationProgress>, id: string, kind: 'sync' | 'download' | 'export' = 'sync') {
    const binding = kind === 'sync' ? value.get(key(id, 'binding')) : undefined
    const current = value.get(key(id, kind))
    const progress = binding?.state === 'running' ? binding : current?.visible || current?.previous ? current : binding
    return progress?.visible ? progress : progress?.previous
}
export function clearExternalProgress(connectionId: string) {
    for (const kind of ['sync', 'binding', 'download', 'export'] as const) operations.delete(key(connectionId, kind))
    emit()
}
export function clearExternalSyncProgress(connectionId: string) {
    operations.delete(key(connectionId, 'sync'))
    emit()
}

/** One operation owns its observations; screens only subscribe. */
export function beginExternalProgress(connectionId: string, kind: ExternalOperationProgress['kind']) {
    const slot = key(connectionId, kind)
    const prior = operations.get(slot)
    const previous = kind === 'sync' && prior?.state === 'complete' && prior.visible ? { ...prior, previous: undefined } : prior?.previous
    let value: ExternalOperationProgress = { id: crypto.randomUUID(), connectionId, kind, stage: 'checking', state: 'running', visible: kind !== 'sync', amounts: emptyAmounts(), previous }
    let ended = false
    let phase = 0
    const owns = () => operations.get(slot)?.id === value.id
    const current = () => !ended && owns()
    const update = (patch: Partial<ExternalOperationProgress>) => {
        if (!owns()) return
        value = { ...value, previous: operations.get(slot)?.previous, ...patch }
        if (!ended && (['uploading', 'downloading', 'applying'].includes(value.stage)
            || Object.values(value.amounts).some(amount => BigInt(amount) > 0n))) value.visible = true
        operations.set(slot, value)
        emit()
    }
    operations.set(slot, value)
    emit()
    return {
        stage(stage: ExternalStage, authority?: string) { if (current()) { phase++; update({ stage, ...(authority ? { authority } : {}) }) } },
        items(completedItems: number, totalItems: number) { if (current()) update({ stage: 'downloading', completedItems, totalItems }) },
        exported(exported: ExternalSnapshotExportProgress) { if (current()) update({ stage: 'downloading', exported }) },
        networkChannel() {
            let sequence = -1
            const channel = new Channel<ExternalTransferSnapshot>()
            channel.onmessage = reading => {
                if (!current() || reading.sequence <= sequence || !validTransferRateSample(reading.network)) return
                sequence = reading.sequence
                update({ network: reading.network })
            }
            return channel
        },
        async invoke<T>(command: string, args: Record<string, unknown>, authority?: string): Promise<T> {
            if (current()) update({ ...(authority ? { authority } : {}) })
            const samplePhase = ++phase
            const sampleAuthority = value.authority
            let accepted = emptyAmounts()
            let sequence = -1
            let uploading = false
            const progress = new Channel<ExternalTransferSnapshot>()
            progress.onmessage = reading => {
                if (!owns() || sampleAuthority !== value.authority || reading.sequence <= sequence) return
                if (!['checking', 'preparing', 'uploading', 'downloading', 'applying', 'finalizing'].includes(reading.stage)) return
                const amounts = emptyAmounts()
                for (const field of Object.keys(amounts) as (keyof typeof amounts)[]) {
                    if (!/^\d+$/.test(reading[field])) return
                    const delta = BigInt(reading[field]) - BigInt(accepted[field])
                    if (delta < 0n) return
                    amounts[field] = (BigInt(value.amounts[field]) + delta).toString()
                }
                sequence = reading.sequence
                accepted = { ...reading }
                uploading ||= reading.stage === 'uploading'
                // Replies and channel messages can arrive separately. Late counts must not rewind a local phase.
                const stage = reading.stage === 'preparing' && uploading ? 'uploading' : reading.stage
                update({ amounts, ...(!ended && samplePhase === phase ? { stage, ...(validTransferRateSample(reading.network) ? { network: reading.network } : {}) } : {}) })
            }
            return invoke<T>(command, { ...args, progress })
        },
        finish(state: Exclude<ExternalOperationProgress['state'], 'running'>, meaningful = true) {
            if (!current()) return
            ended = true
            update({ state, visible: state !== 'complete' || meaningful || kind !== 'sync' })
            // Recovery notices live in the connection/job state; this panel retains only a brief result.
            setTimeout(() => {
                const latest = operations.get(slot)
                if (latest?.id === value.id) { operations.delete(slot); emit() }
                else if (latest?.previous?.id === value.id) { operations.set(slot, { ...latest, previous: undefined }); emit() }
            }, 4000)
        },
    }
}
export type ExternalProgressRun = ReturnType<typeof beginExternalProgress>
export const externalProgressFailure = (error: unknown) => {
    const e = error as { name?: string; code?: string; kind?: string } | null
    return e?.name === 'AbortError' || e?.code === 'cancelled' || e?.kind === 'cancelled' ? 'cancelled' : 'failed'
}
export const externalJobTotalGrows = (job: ExternalJobSummary) => job.kind === 'backup' && job.counters === 'transferred' && externalJobIsActive(job)
export function externalJobFraction(job: ExternalJobSummary): number | null {
    return (job.kind === 'restore' && ['preparing-local', 'applying-local', 'awaiting-adoption'].includes(job.phase)) || externalJobTotalGrows(job) ? null : externalJobProgress(job)
}
