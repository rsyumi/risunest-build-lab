import { beforeEach, expect, it, vi } from 'vitest'
const dialog = vi.hoisted(() => vi.fn())
vi.mock('src/ts/alert', () => ({ alertCheckboxConfirm: dialog }))
import { confirmSyncBindingReplacement } from './bindingDialog'
import { language } from 'src/lang'
beforeEach(() => dialog.mockReset())
it('uses one required acknowledgement with the settled copy', async () => {
    dialog.mockResolvedValue({ confirmed: true, checked: true })
    expect(await confirmSyncBindingReplacement()).toBe(true)
    expect(dialog).toHaveBeenCalledOnce()
    expect(dialog).toHaveBeenCalledWith({ title: language.lwwSync.replaceTitle, description: language.lwwSync.replaceDescription, checkboxLabel: language.lwwSync.replaceAcknowledge, actionLabel: language.lwwSync.replaceAction, cancelLabel: language.lwwSync.cancelAction, requireChecked: true })
})
it.each([{ confirmed: false, checked: false }, { confirmed: false, checked: true }, { confirmed: true, checked: false }])('cannot replace without explicit acknowledgement %j', async result => {
    dialog.mockResolvedValue(result)
    expect(await confirmSyncBindingReplacement()).toBe(false)
})
it('describes a restored server instead of an ordinary replacement and keeps the acknowledgement', async () => {
    dialog.mockResolvedValue({ confirmed: true, checked: true })
    expect(await confirmSyncBindingReplacement('server-restored')).toBe(true)
    expect(dialog).toHaveBeenCalledWith({ title: language.lwwSync.replaceTitle, description: language.lwwSync.serverRestoredDescription, checkboxLabel: language.lwwSync.replaceAcknowledge, actionLabel: language.lwwSync.replaceAction, cancelLabel: language.lwwSync.cancelAction, requireChecked: true })
})
