import { alertCheckboxConfirm } from 'src/ts/alert'
import { language } from 'src/lang'
import type { ReplacementReason } from './bindingFlow'

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
