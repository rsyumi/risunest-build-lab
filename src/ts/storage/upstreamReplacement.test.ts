import {describe, expect, it, vi} from 'vitest'
const dialog = vi.hoisted(() => vi.fn())
vi.mock('../alert', () => ({alertCheckboxConfirm:dialog}))
import {confirmUpstreamLibraryReplacement} from './upstreamReplacement'
import {language} from '../../lang'

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
})
