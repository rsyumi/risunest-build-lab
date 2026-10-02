import { alertCheckboxConfirm } from 'src/ts/alert'
import { language } from 'src/lang'

export async function confirmSyncBindingReplacement(): Promise<boolean> {
    const result = await alertCheckboxConfirm({
        title: language.lwwSync.replaceTitle,
        description: language.lwwSync.replaceDescription,
        checkboxLabel: language.lwwSync.replaceAcknowledge,
        actionLabel: language.lwwSync.replaceAction,
        cancelLabel: language.lwwSync.cancelAction,
        requireChecked: true,
    })
    return result.confirmed && result.checked
}
