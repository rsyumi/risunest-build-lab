import {afterEach, beforeEach, describe, expect, it, vi} from 'vitest'
import {writable} from 'svelte/store'
const dialog = vi.hoisted(() => vi.fn())
const actionDialog = vi.hoisted(() => vi.fn())
const host = vi.hoisted(() => ({store: undefined as unknown as import('svelte/store').Writable<'dialog' | 'onboarding'>}))
const libraryContent = vi.hoisted(() => vi.fn())
vi.mock('../alert', () => ({alertCheckboxConfirm:dialog, alertActionConfirm:actionDialog}))
vi.mock('./nativeFileJobManager', () => ({get nativeFileJobHost() { return host.store }}))
vi.mock('./sync/bindingLocalData', () => ({hasLocalLibraryContent:libraryContent}))
host.store = writable<'dialog' | 'onboarding'>('dialog')
import {confirmUpstreamLibraryReplacement} from './upstreamReplacement'
import {language} from '../../lang'

beforeEach(() => {
    libraryContent.mockReset().mockResolvedValue(false)
})
afterEach(() => {
    host.store.set('dialog')
    dialog.mockReset()
    actionDialog.mockReset()
})

describe('upstream import acknowledgement', () => {
    it.each([false,true])('requires a checked acknowledgement and includes every warning, bound=%s', async bound => {
        dialog.mockResolvedValueOnce({confirmed:true,checked:true})
        await expect(confirmUpstreamLibraryReplacement(bound,['cold warning','inlay warning'])).resolves.toBe(true)
        expect(dialog).toHaveBeenLastCalledWith(expect.objectContaining({
            description:[bound ? language.lwwSync.restoreDescriptionBound : language.lwwSync.restoreDescription,'cold warning','inlay warning'].join('\n\n'),
            checkboxLabel:language.lwwSync.restoreAcknowledge,requireChecked:true,
        }))
    })
    it('declines cancellation before activation', async () => {
        dialog.mockResolvedValueOnce({confirmed:false,checked:false})
        await expect(confirmUpstreamLibraryReplacement(true)).resolves.toBe(false)
    })
    it('restores without asking in the onboarding when nothing is bound and the library holds no content', async () => {
        host.store.set('onboarding')
        await expect(confirmUpstreamLibraryReplacement(false)).resolves.toBe(true)
        expect(libraryContent).toHaveBeenCalledOnce()
        expect(dialog).not.toHaveBeenCalled()
        expect(actionDialog).not.toHaveBeenCalled()
    })
    it.each([true,false])('shows only the backup warnings in the onboarding, answer=%s', async answer => {
        host.store.set('onboarding')
        actionDialog.mockResolvedValueOnce(answer)
        await expect(confirmUpstreamLibraryReplacement(false,['cold warning','inlay warning'])).resolves.toBe(answer)
        expect(dialog).not.toHaveBeenCalled()
        expect(actionDialog).toHaveBeenLastCalledWith({
            title:language.lwwSync.restoreTitle, description:['cold warning','inlay warning'].join('\n\n'),
            actionLabel:language.lwwSync.restoreAction, cancelLabel:language.lwwSync.cancelAction,
        })
    })
    it('still asks for the acknowledgement in the onboarding when a sync binding would carry the restore', async () => {
        host.store.set('onboarding')
        dialog.mockResolvedValueOnce({confirmed:true,checked:true})
        await expect(confirmUpstreamLibraryReplacement(true)).resolves.toBe(true)
        expect(dialog).toHaveBeenLastCalledWith(expect.objectContaining({checkboxLabel:language.lwwSync.restoreAcknowledge,requireChecked:true}))
        expect(libraryContent).not.toHaveBeenCalled()
    })
    it.each([
        ['holds content', () => libraryContent.mockResolvedValue(true)],
        ['cannot be read', () => libraryContent.mockRejectedValue(new Error('unreadable'))],
    ])('still asks for the acknowledgement in the onboarding when the library %s', async (_name, arrange) => {
        host.store.set('onboarding')
        arrange()
        dialog.mockResolvedValueOnce({confirmed:true,checked:true})
        await expect(confirmUpstreamLibraryReplacement(false,['inlay warning'])).resolves.toBe(true)
        expect(actionDialog).not.toHaveBeenCalled()
        expect(dialog).toHaveBeenLastCalledWith(expect.objectContaining({
            description:[language.lwwSync.restoreDescription,'inlay warning'].join('\n\n'),
            checkboxLabel:language.lwwSync.restoreAcknowledge, requireChecked:true,
        }))
    })
})
