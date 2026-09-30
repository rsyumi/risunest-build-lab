/**
 * What the sync-server screen of the onboarding shows once a connection has
 * started. The controller snapshot is the only input, so the rules can be
 * checked without a DOM.
 */

import type { ServerSyncSnapshot } from 'src/ts/storage/sync/serverSyncController'
import { completedServerSyncAttempt } from 'src/ts/storage/sync/serverSyncPresenter'
import type { OnboardingState } from './onboardingFlow'

/** The screen before a connection starts: the code box, then the server check. */
export type ServerSyncOnboardingStage = 'code' | 'review' | 'syncing'

export type ServerSyncOnboardingOutcome =
    | 'syncing'
    | 'complete'
    | 'conflict'
    | 'paused'
    | 'pending'
    | 'continuing'
    | 'error'

/**
 * Reads the attempt that the reader started from this screen. `paused` is a
 * stop the reader asked for, not a failure. `pending` is a server job that
 * has not finished yet and can simply be retried.
 */
export function serverSyncOnboardingOutcome(
    snapshot: ServerSyncSnapshot,
): ServerSyncOnboardingOutcome {
    if (snapshot.running) return 'syncing'
    if (completedServerSyncAttempt(snapshot) !== null) return 'complete'
    if (snapshot.error && snapshot.error !== 'cancelled') return 'error'
    if (snapshot.result?.phase === 'conflict') return 'conflict'
    if (snapshot.paused) return 'paused'
    if (!snapshot.error && snapshot.result?.phase === 'idle') return 'continuing'
    if (!snapshot.error && snapshot.result?.phase === 'pending') return 'pending'
    return 'error'
}

export function createServerSyncOnboardingContinuation() {
    let attempt: number | undefined
    let previousTail: number | undefined
    let unchanged = 0
    return (snapshot: ServerSyncSnapshot): boolean => {
        if (serverSyncOnboardingOutcome(snapshot) !== 'continuing' || attempt === snapshot.attemptId) return false
        attempt = snapshot.attemptId
        const status = snapshot.status
        const tail = (status?.dirtyRecords ?? 0) + Number(status?.pendingDeviceSections) + Number(status?.fullScan)
        unchanged = previousTail !== undefined && tail >= previousTail ? unchanged + 1 : 0
        previousTail = tail
        return unchanged < 2
    }
}

/**
 * Only a completed attempt leads to the last onboarding screen. A paused one
 * stays on the sync screen, where the reader continues it or starts the app
 * and continues it from the settings; the pause lasts across restarts until
 * then.
 */
export function serverSyncOnboardingNext(
    outcome: ServerSyncOnboardingOutcome | undefined,
): 'done' | undefined {
    return outcome === 'complete' ? 'done' : undefined
}

/**
 * What the screen does when it opens on a device that is already connected,
 * where a registration code would be refused: draw the attempt that is
 * running or paused, or start another one. Nothing for a device that is not
 * connected.
 */
export function serverSyncOnboardingResume(
    snapshot: ServerSyncSnapshot | undefined,
): 'show' | 'retry' | undefined {
    if (!snapshot?.status?.configured) return undefined
    return snapshot.running || snapshot.paused ? 'show' : 'retry'
}

/**
 * Where the first screen moves once it learns the device is connected, so a
 * device connected before the app started again opens on its sync instead of
 * offering to start another setup over the library it receives.
 */
export function serverSyncOnboardingOpening(
    state: OnboardingState,
    snapshot: ServerSyncSnapshot | undefined,
): 'sync-hub' | undefined {
    return state === 'home' && serverSyncOnboardingResume(snapshot) ? 'sync-hub' : undefined
}
