import { alertCheckboxConfirm } from 'src/ts/alert'
import { language } from 'src/lang'
import type { PreviousStorageFilesChoice, PreviousStorageFilesContext, ReplacementReason } from './bindingFlow'
import { downloadRemoteAssets, getAssetResidencyStatus, type AssetResidencyTarget } from './serverAssetResidency'

const residencyTarget = (context: PreviousStorageFilesContext): AssetResidencyTarget => context.target.kind === 'server'
    ? { kind: 'server', libraryId: context.libraryId, targetId: context.targetId }
    : { kind: 'external', connectionId: context.target.connectionId }

export async function confirmSyncBindingReplacement(reason?: ReplacementReason): Promise<boolean> {
    const result = await alertCheckboxConfirm({
        title: language.lwwSync.replaceTitle,
        description: reason === 'server-restored' ? language.lwwSync.serverRestoredDescription : language.lwwSync.replaceDescription,
        checkboxLabel: language.lwwSync.replaceAcknowledge,
        actionLabel: language.lwwSync.replaceAction,
        cancelLabel: language.lwwSync.cancelAction,
        requireChecked: true,
    })
    return result.confirmed && result.checked
}

export async function confirmPreviousStorageFiles(context: PreviousStorageFilesContext): Promise<PreviousStorageFilesChoice> {
    let held: number | undefined
    try {
        held = (await getAssetResidencyStatus(residencyTarget(context))).previousStorageObjects
    } catch {
        // An unreadable status still asks, since files may be held elsewhere.
    }
    if (held === 0) return 'connect'
    const result = await alertCheckboxConfirm({
        title: language.lwwSync.previousFilesTitle,
        description: held === undefined ? language.lwwSync.previousFilesUnknownDescription : language.lwwSync.previousFilesDescription,
        checkboxLabel: language.lwwSync.downloadThenConnect,
        actionLabel: language.risuNest.serverSync.connect,
        cancelLabel: language.lwwSync.cancelAction,
        requireChecked: false,
    })
    if (!result.confirmed) return 'cancel'
    return result.checked ? 'download-then-connect' : 'connect'
}

export async function downloadPreviousStorageFiles(context: PreviousStorageFilesContext): Promise<void> {
    await downloadRemoteAssets(undefined, { target: residencyTarget(context), signal: context.signal })
}
