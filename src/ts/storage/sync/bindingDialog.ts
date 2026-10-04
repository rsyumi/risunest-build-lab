import { alertCheckboxConfirm } from 'src/ts/alert'
import { language } from 'src/lang'
import type { PreviousStorageFilesChoice, PreviousStorageFilesContext, ReplacementReason } from './bindingFlow'
import { downloadRemoteAssets, getAssetResidencyStatus } from './serverAssetResidency'

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
    let held = 0
    try {
        const status = await getAssetResidencyStatus()
        const target = context.target
        // A file several storages hold counts for none of them, so it is left
        // in whatever the target alone does not hold.
        held = status.remoteObjects - (target.kind === 'external'
            ? status.externalObjects.find(entry => entry.connectionId === target.connectionId)?.objects ?? 0
            : 0)
    } catch {
        return 'connect'
    }
    if (!held) return 'connect'
    const result = await alertCheckboxConfirm({
        title: language.lwwSync.previousFilesTitle,
        description: language.lwwSync.previousFilesDescription,
        checkboxLabel: language.lwwSync.downloadThenConnect,
        actionLabel: language.risuNest.serverSync.connect,
        cancelLabel: language.lwwSync.cancelAction,
        requireChecked: false,
    })
    if (!result.confirmed) return 'cancel'
    return result.checked ? 'download-then-connect' : 'connect'
}

export async function downloadPreviousStorageFiles(): Promise<void> {
    await downloadRemoteAssets()
}
