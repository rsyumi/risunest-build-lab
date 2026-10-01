import { describe, expect, it, vi } from 'vitest'
import { mergeExternalHistoryItems } from 'src/ts/storage/sync/external/connection'
import type {
    ExternalConflictSummary,
    ExternalConnectionSummary,
    ExternalHistoryItem,
} from 'src/ts/storage/sync/external/types'
import {
    abandonExternalOnboardingSelection,
    loadExternalOnboardingConflict,
    externalOnboardingAction,
    externalOnboardingConflictStep,
    externalOnboardingRestorable,
    externalOnboardingRestoreAreas,
    externalOnboardingRestoreRestarts,
    externalOnboardingSyncOutcome,
} from './externalStorageOnboardingFlow'

function connection(
    purpose: 'backup' | 'sync',
): Pick<ExternalConnectionSummary, 'purpose'> {
    return { purpose }
}

function item(
    id: string,
    createdAtMs: string,
    overrides: Partial<ExternalHistoryItem> = {},
): ExternalHistoryItem {
    return {
        id,
        snapshotId: id,
        kind: 'recovery-candidate',
        createdAtMs: createdAtMs as ExternalHistoryItem['createdAtMs'],
        logicalRevision: '1' as ExternalHistoryItem['logicalRevision'],
        pinned: false,
        complete: true,
        verified: true,
        includedSections: [],
        sameDevice: false,
        ...overrides,
    }
}

function conflict(
    overrides: Partial<ExternalConflictSummary> = {},
): ExternalConflictSummary {
    return {
        id: 'conflict-1',
        connectionId: 'connection-1',
        detectedAtMs: '1' as ExternalConflictSummary['detectedAtMs'],
        localRevision: '2' as ExternalConflictSummary['localRevision'],
        remoteRevision: '9' as ExternalConflictSummary['remoteRevision'],
        localAvailable: true,
        remoteAvailable: true,
        remotePointConfirmed: true,
        resolved: false,
        ...overrides,
    }
}

describe('external storage onboarding action', () => {
    it('joins a synchronization repository and reads a backup repository back', () => {
        expect(externalOnboardingAction(connection('sync'))).toBe('sync')
        expect(externalOnboardingAction(connection('backup'))).toBe('restore')
    })

    it('replaces only what a backup covers, so no restore restarts the app', () => {
        expect(externalOnboardingRestoreAreas(item('one', '1'))).toEqual([
            'library',
            'referencedAssets',
        ])
        expect(
            externalOnboardingRestoreAreas(
                item('two', '1', {
                    includedSections: ['hypa', 'local-plugins', 'local-settings'],
                }),
            ),
        ).toEqual(['library', 'referencedAssets', 'hypa', 'local-plugins'])
        expect(
            externalOnboardingRestoreAreas(
                item('three', '1', {
                    includedSections: ['hypa', 'local-settings'],
                    sameDevice: true,
                }),
            ),
        ).toEqual(['library', 'referencedAssets', 'hypa', 'local-settings'])
        expect(externalOnboardingRestoreRestarts()).toBe(false)
    })
})

describe('external storage onboarding backups', () => {
    it('offers the newest readable backup first', () => {
        const merged = mergeExternalHistoryItems([], [
            item('older', '1000'),
            item('newest', '3000'),
            item('middle', '2000'),
        ])

        expect(externalOnboardingRestorable(merged).map(entry => entry.id))
            .toEqual(['newest', 'middle', 'older'])
    })

    it('preserves the restore snapshot identity when a retained point suppresses its recovery candidate', () => {
        const merged = mergeExternalHistoryItems([], [
            item('point-id', '2', { snapshotId: 'snapshot-id', kind: 'backup-point' }),
            item('snapshot-id', '1', { snapshotId: 'snapshot-id', kind: 'recovery-candidate' }),
        ])
        const restored = externalOnboardingRestorable(merged)
        expect(restored).toHaveLength(1)
        expect(restored[0]).toMatchObject({ id: 'point-id', snapshotId: 'snapshot-id' })
    })

    it('leaves out entries a restore cannot read back', () => {
        const merged = mergeExternalHistoryItems([], [
            item('partial', '3000', { complete: false }),
            item('unverified', '2000', { verified: false }),
            item('usable', '1000'),
        ])

        expect(externalOnboardingRestorable(merged).map(entry => entry.id)).toEqual(['usable'])
    })
})

describe('external storage onboarding first synchronization', () => {
    it('reads the repository side before it can be chosen', () => {
        expect(externalOnboardingConflictStep(conflict({
            remotePointConfirmed: false,
        }))).toBe('receive-repository')
        expect(externalOnboardingConflictStep(conflict())).toBe('take-repository')
        expect(externalOnboardingConflictStep(conflict({ remoteAvailable: false }))).toBe('wait')
    })

    it('separates a finished attempt, a first-attach decision and a failure', () => {
        expect(externalOnboardingSyncOutcome({ kind: 'complete' })).toBe('complete')
        expect(externalOnboardingSyncOutcome({
            kind: 'blocked',
            reason: 'external-storage-conflict',
        })).toBe('conflict')
        expect(externalOnboardingSyncOutcome({ kind: 'blocked', reason: 'transient' })).toBe('error')
        expect(externalOnboardingSyncOutcome({ kind: 'cancelled' })).toBe('error')
    })
})

describe('onboarding selection ownership', () => {
    const owner = { connectionId: 'connection-1', selectionEpoch: 'epoch-1' }
    function bridge(jobState = 'failed') {
        const job = { id: 'job', connectionId: owner.connectionId, state: jobState }
        return {
            getState: vi.fn().mockResolvedValue({ selection: { kind: 'external', ...owner }, jobs: [job] }),
            cancelJob: vi.fn().mockResolvedValue({ ...job, state: 'cancelled' }),
            getJob: vi.fn(), setSyncTarget: vi.fn(),
        }
    }
    it('clears only the selection made by this onboarding attempt after stopping owned work', async () => {
        const native = bridge('waiting')
        await abandonExternalOnboardingSelection(owner, native)
        expect(native.cancelJob).toHaveBeenCalledWith('job')
        expect(native.setSyncTarget).toHaveBeenCalledWith(null, 'epoch-1')
    })
    it('preserves a stopped conflict record while releasing the attempt selection', async () => {
        const native = bridge('waiting')
        native.cancelJob.mockResolvedValue({ id: 'job', connectionId: owner.connectionId,
            state: 'waiting', phase: 'conflict-preservation-paused' })
        await abandonExternalOnboardingSelection(owner, native)
        expect(native.getJob).not.toHaveBeenCalled()
        expect(native.setSyncTarget).toHaveBeenCalledWith(null, owner.selectionEpoch)
    })
    it('keeps an uncertain publication and blocks navigation', async () => {
        const native = bridge('uncertain')
        await expect(abandonExternalOnboardingSelection(owner, native)).rejects.toMatchObject({ kind: 'preconditionFailed' })
        expect(native.cancelJob).not.toHaveBeenCalled()
        expect(native.setSyncTarget).not.toHaveBeenCalled()
    })
    it('does not clear a replacement selection made outside the attempt', async () => {
        const native = bridge()
        native.getState.mockResolvedValue({ selection: { kind: 'external', ...owner, selectionEpoch: 'new-epoch' }, jobs: [] })
        await abandonExternalOnboardingSelection(owner, native)
        expect(native.setSyncTarget).not.toHaveBeenCalled()
    })
    it('keeps navigation blocked if native admission refuses clearing the target', async () => {
        const native = bridge()
        native.setSyncTarget.mockRejectedValue({ kind: 'preconditionFailed' })
        await expect(abandonExternalOnboardingSelection(owner, native)).rejects.toMatchObject({ kind: 'preconditionFailed' })
    })
})

describe('onboarding conflict identity', () => {
    it('loads the exact conflict from later pages instead of another conflict on the connection', async () => {
        const wanted = conflict({ id: 'wanted', connectionId: 'connection-1' })
        const bridge = { listConflicts: vi.fn()
            .mockResolvedValueOnce({ conflicts: [conflict({ id: 'other', connectionId: 'connection-1' })], nextCursor: { createdAtMs: 1, id: 'other' } })
            .mockResolvedValueOnce({ conflicts: [wanted] }) }
        await expect(loadExternalOnboardingConflict('connection-1', 'wanted', bridge)).resolves.toEqual(wanted)
        expect(bridge.listConflicts).toHaveBeenCalledTimes(2)
        expect(bridge.listConflicts.mock.calls[1]).toEqual([{ createdAtMs: 1, id: 'other' }, 50])
    })
    it('does not infer a conflict when the completed job omitted its identity', async () => {
        const bridge = { listConflicts: vi.fn() }
        await expect(loadExternalOnboardingConflict('connection-1', undefined, bridge)).rejects.toMatchObject({ kind: 'corrupt' })
        expect(bridge.listConflicts).not.toHaveBeenCalled()
    })
})
