import { describe, expect, it } from 'vitest'

import type { ServerSyncSnapshot } from 'src/ts/storage/sync/serverSyncController'
import {
    serverSyncOnboardingNext,
    serverSyncOnboardingOpening,
    serverSyncOnboardingOutcome,
    serverSyncOnboardingResume,
} from './serverSyncOnboardingFlow'

const head = {
    libraryId: 'library',
    epoch: 'epoch',
    seq: '1',
    headId: 'head',
    minRetainedSeq: '0',
    sections: {
      hypa: { stateId: 'hypa-state', changedSeq: '0', gcFloor: '0' },
      library: { stateId: 'library-state', changedSeq: '0', gcFloor: '0' },
      'local-plugins': { stateId: 'plugins-state', changedSeq: '0', gcFloor: '0' },
    },
}
const identity = {
    endpoint: 'https://sync.example/',
    libraryId: 'library',
    deviceId: 'device',
}
function finished(phase: 'idle' | 'pending' | 'conflict'): ServerSyncSnapshot {
    return {
        running: false,
        paused: false,
        error: '',
        attemptId: 1,
        attemptIdentity: identity,
        initialSyncComplete: phase === 'idle',
        status: {
            localRevision: 3,
            reconciling: false,
            configured: true,
            ...identity,
            head,
            dirtyRecords: 0,
            pendingDeviceSections: false,
            fullScan: false,
            registrationRequired: false,
            operationPending: phase === 'pending',
        },
        result: {
            endpoint: identity.endpoint,
            phase,
            localRevision: 3,
            head,
            conflictCount: phase === 'conflict' ? 1 : 0,
            conflicts: [],
            appliedRecords: 0,
            proposedRecords: 0,
        },
    }
}

describe('sync server onboarding outcome', () => {
    it('keeps the progress screen while the attempt runs', () => {
        expect(serverSyncOnboardingOutcome({ ...finished('idle'), running: true })).toBe(
            'syncing',
        )
    })

    it('finishes only on the verified completion of the attempt', () => {
        expect(serverSyncOnboardingOutcome(finished('idle'))).toBe('complete')
        const stale = finished('idle')
        stale.status!.dirtyRecords = 1
        expect(serverSyncOnboardingOutcome(stale)).toBe('error')
    })

    it('offers the conflict choice when the first comparison found one', () => {
        expect(serverSyncOnboardingOutcome(finished('conflict'))).toBe('conflict')
    })

    it('treats a stop the reader asked for as paused, not as a failure', () => {
        expect(
            serverSyncOnboardingOutcome({
                ...finished('idle'),
                paused: true,
                error: 'cancelled',
                initialSyncComplete: false,
            }),
        ).toBe('paused')
    })

    it('leaves the sync screen only for a completed attempt', () => {
        expect(serverSyncOnboardingNext('complete')).toBe('done')
        // A paused attempt stays on the screen so it can be continued there.
        for (const outcome of ['syncing', 'paused', 'conflict', 'pending', 'error', undefined] as const)
            expect(serverSyncOnboardingNext(outcome)).toBeUndefined()
    })

    it('separates an unfinished server job from a failure', () => {
        expect(serverSyncOnboardingOutcome(finished('pending'))).toBe('pending')
        expect(
            serverSyncOnboardingOutcome({ ...finished('pending'), error: 'server-unreachable' }),
        ).toBe('error')
    })
})

describe('sync server onboarding on a connected device', () => {
    it('continues the connection instead of asking for a code again', () => {
        expect(serverSyncOnboardingResume({ ...finished('pending'), running: true })).toBe('show')
        // A held library waits for the settings to continue it.
        expect(serverSyncOnboardingResume({ ...finished('pending'), paused: true })).toBe('show')
        expect(serverSyncOnboardingResume({ ...finished('pending'), result: undefined })).toBe('retry')
        expect(
            serverSyncOnboardingResume({ ...finished('idle'), error: 'library-operation-busy' }),
        ).toBe('retry')
    })

    it('asks for a code while the device is not connected', () => {
        expect(serverSyncOnboardingResume(undefined)).toBeUndefined()
        expect(serverSyncOnboardingResume({ running: false, paused: false, error: '' })).toBeUndefined()
        const disconnected = finished('idle')
        expect(
            serverSyncOnboardingResume({
                ...disconnected,
                status: { ...disconnected.status!, configured: false },
            }),
        ).toBeUndefined()
    })

    it('opens on the sync screen only from the first screen of a connected device', () => {
        const running = { ...finished('pending'), running: true }
        expect(serverSyncOnboardingOpening('home', running)).toBe('sync-hub')
        expect(serverSyncOnboardingOpening('home', finished('idle'))).toBe('sync-hub')
        expect(serverSyncOnboardingOpening('import', running)).toBeUndefined()
        expect(serverSyncOnboardingOpening('sync-hub', running)).toBeUndefined()
        expect(serverSyncOnboardingOpening('home', undefined)).toBeUndefined()
        const disconnected = finished('idle')
        expect(
            serverSyncOnboardingOpening('home', {
                ...disconnected,
                status: { ...disconnected.status!, configured: false },
            }),
        ).toBeUndefined()
    })
})
