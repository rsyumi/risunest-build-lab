/**
 * What the external storage screen of the onboarding shows. The connection
 * summary and the repository history are the only inputs, so the rules can be
 * checked without a DOM.
 */

import { externalConflictActions, externalJobIsPaused, restorableExternalHistoryItems } from 'src/ts/storage/sync/external/connection'
import {
    externalRestorableSections,
    externalRestoreAreas,
} from 'src/ts/storage/sync/external/restoreScope'
import type {
    ExternalConflictSummary,
    ExternalConnectionSummary,
    ExternalHistoryItem,
    ExternalRestoreArea,
} from 'src/ts/storage/sync/external/types'

/**
 * `connect` collects the recovery key, `choose` picks what to bring over,
 * `working` runs it and `conflict` asks whose contents to keep.
 */
export type ExternalOnboardingStage = 'connect' | 'choose' | 'working' | 'conflict' | 'error'

/**
 * A synchronization repository hands its contents over through its own head,
 * so this device joins it. A backup repository is only read back from history.
 */
export function externalOnboardingAction(
    connection: Pick<ExternalConnectionSummary, 'purpose'>,
): 'sync' | 'restore' {
    return connection.purpose === 'sync' ? 'sync' : 'restore'
}

/**
 * History pages are grouped by repository object identifier and the first one
 * holds kept copies only, so the newest entry is whatever the loaded pages
 * carry. The list is already newest first once merged.
 */
export function externalOnboardingRestorable(
    items: readonly ExternalHistoryItem[],
): ExternalHistoryItem[] {
    return restorableExternalHistoryItems(items)
}

/**
 * The areas a restore replaces, following what the selected backup covers.
 * This screen runs on a device with nothing of its own to keep, so everything
 * the backup carries and this device may take comes over.
 */
export function externalOnboardingRestoreAreas(
    item: Pick<ExternalHistoryItem, 'includedSections' | 'sameDevice'>,
): ExternalRestoreArea[] {
    return externalRestoreAreas(item, externalRestorableSections(item))
}

/** True when a restore replaces device sections, which restarts the app. */
export function externalOnboardingRestoreRestarts(): boolean {
    return false
}

/**
 * The first synchronization of a device that already holds its own defaults is
 * answered with a conflict. Only the repository side is offered here, because
 * keeping this device would publish its empty library to every other device.
 */
export type ExternalOnboardingConflictStep = 'receive-repository' | 'take-repository' | 'wait'

export function externalOnboardingConflictStep(
    conflict: ExternalConflictSummary,
): ExternalOnboardingConflictStep {
    const actions = externalConflictActions(conflict)
    if (actions.includes('retry-sync')) return 'receive-repository'
    if (actions.includes('use-remote')) return 'take-repository'
    return 'wait'
}

/** Reads the outcome of a synchronization the reader started from this screen. */
export function externalOnboardingSyncOutcome(
    result: { kind: 'complete' } | { kind: 'blocked'; reason: string } | { kind: 'cancelled' },
): 'complete' | 'conflict' | 'error' {
    if (result.kind === 'complete') return 'complete'
    if (result.kind === 'blocked' && result.reason === 'external-storage-conflict') return 'conflict'
    return 'error'
}

export interface ExternalOnboardingSelectionOwner {
    connectionId: string
    selectionEpoch: string
}

export async function abandonExternalOnboardingSelection(
    owner: ExternalOnboardingSelectionOwner,
    bridge: Pick<import('src/ts/storage/sync/external/bridge').ExternalStorageBridge,
        'getState' | 'getJob' | 'cancelJob' | 'setSyncTarget'>,
    wait: () => Promise<void> = () => new Promise(resolve => setTimeout(resolve, 100)),
): Promise<void> {
    const state = await bridge.getState()
    const selection = state.selection
    if (selection.kind !== 'external' || selection.connectionId !== owner.connectionId
        || selection.selectionEpoch !== owner.selectionEpoch) return
    const pending = state.jobs.filter(job => job.connectionId === owner.connectionId
        && !['succeeded', 'failed', 'cancelled', 'conflict'].includes(job.state))
    if (pending.some(job => job.state === 'uncertain' || job.error?.reason === 'publication-unknown')) {
        throw { kind: 'preconditionFailed' }
    }
    for (const initial of pending) {
        let job = await bridge.cancelJob(initial.id)
        while (job.state === 'queued' || job.state === 'running'
            || (job.state === 'waiting' && !externalJobIsPaused(job))) {
            await wait()
            job = await bridge.getJob(job.id)
        }
        if (job.state === 'uncertain' || job.error?.reason === 'publication-unknown') {
            throw { kind: 'preconditionFailed' }
        }
    }
    await bridge.setSyncTarget(null, owner.selectionEpoch)
}
