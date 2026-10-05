import { describe, expect, it } from 'vitest'
import { mergeExternalHistoryItems } from 'src/ts/storage/sync/external/connection'
import type {
    ExternalConnectionSummary,
    ExternalHistoryItem,
} from 'src/ts/storage/sync/external/types'
import {
    externalOnboardingRestorable,
    externalOnboardingRestoreAreas,
    externalOnboardingRestoreRestarts,
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
        includedSections: ['hypa', 'local-plugins', 'local-settings'],
        sameDevice: false,
        ...overrides,
    }
}
describe('external storage onboarding action', () => {
    it('restores the complete backup scope and rejects incomplete entries', () => {
        expect(externalOnboardingRestoreAreas(item('one', '1'))).toEqual([
            'library', 'referencedAssets', 'hypa', 'local-plugins', 'local-settings',
        ])
        expect(() => externalOnboardingRestoreAreas(item('two', '1', {
            includedSections: ['hypa', 'local-settings'],
        }))).toThrow('complete restore scope')
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
