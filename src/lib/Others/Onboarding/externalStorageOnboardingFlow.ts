/**
 * What the external storage screen of the onboarding shows. The connection
 * summary and the repository history are the only inputs, so the rules can be
 * checked without a DOM.
 */

import { bindSyncTarget } from 'src/ts/storage/sync/bindingRegistry'
import type { BindingOutcome } from 'src/ts/storage/sync/bindingFlow'
import { restorableExternalHistoryItems } from 'src/ts/storage/sync/external/connection'
import {
    externalRestorableSections,
    externalRestoreAreas,
} from 'src/ts/storage/sync/external/restoreScope'
import type {
    ExternalConnectionSummary,
    ExternalHistoryItem,
    ExternalRestoreArea,
} from 'src/ts/storage/sync/external/types'

/**
 * `connect` collects the recovery key, `choose` picks what to bring over,
 * `working` runs it and `conflict` asks whose contents to keep.
 */
export type ExternalOnboardingStage = 'connect' | 'choose' | 'working' | 'error'

/**
 * A synchronization repository hands its contents over through its own head,
 * so this device joins it. A backup repository is only read back from history.
 */


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

export function bindExternalOnboardingTarget(connectionId: string): Promise<BindingOutcome> {
    return bindSyncTarget({ kind: 'external', connectionId })
}
