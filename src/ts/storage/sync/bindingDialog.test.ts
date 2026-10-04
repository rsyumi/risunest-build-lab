import { beforeEach, describe, expect, it, vi } from 'vitest'
const dialog = vi.hoisted(() => vi.fn())
const residency = vi.hoisted(() => ({ status: vi.fn(), download: vi.fn() }))
vi.mock('src/ts/alert', () => ({ alertCheckboxConfirm: dialog }))
vi.mock('./serverAssetResidency', () => ({ getAssetResidencyStatus: residency.status, downloadRemoteAssets: residency.download }))
import { confirmPreviousStorageFiles, confirmSyncBindingReplacement, downloadPreviousStorageFiles } from './bindingDialog'
import type { PreviousStorageFilesContext } from './bindingFlow'
import { language } from 'src/lang'
beforeEach(() => { dialog.mockReset(); residency.status.mockReset(); residency.download.mockReset() })
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

describe('files held only by the previous storage', () => {
    const context = (target: PreviousStorageFilesContext['target']): PreviousStorageFilesContext => ({
        target, signal: new AbortController().signal,
        state: { target: { kind: 'none' }, targetAuthority: '0', selectionEpoch: '0', libraryId: null, progress: null },
    })
    const status = (serverObjects: number, externalObjects: { connectionId: string, objects: number }[]) => ({ policy: 'remote', localBytes: 0, remoteBytes: 1,
        remoteObjects: serverObjects + externalObjects.reduce((sum, entry) => sum + entry.objects, 0), serverBytes: 0, serverObjects, externalObjects, unavailableObjects: 0, evictedBytes: 0 })
    const server = { kind: 'server', connectionId: 'server' } as const
    const external = { kind: 'external', connectionId: 'next' } as const
    it.each([
        { name: 'no storage holds files', target: server, held: status(0, []) },
        { name: 'only the new external storage holds files', target: external, held: status(0, [{ connectionId: 'next', objects: 3 }]) },
    ])('connects without asking when $name', async ({ target, held }) => {
        residency.status.mockResolvedValue(held)
        expect(await confirmPreviousStorageFiles(context(target))).toBe('connect')
        expect(dialog).not.toHaveBeenCalled()
    })
    it('connects without asking when the status cannot be read', async () => {
        residency.status.mockRejectedValue({ code: 'local-storage' })
        expect(await confirmPreviousStorageFiles(context(server))).toBe('connect')
        expect(dialog).not.toHaveBeenCalled()
    })
    it.each([
        { name: 'an external storage', target: server, held: status(0, [{ connectionId: 'previous', objects: 1 }]) },
        { name: 'a server', target: external, held: status(2, []) },
        { name: 'another external storage', target: external, held: status(0, [{ connectionId: 'next', objects: 1 }, { connectionId: 'previous', objects: 2 }]) },
    ])('asks once with an unchecked download option when $name holds files', async ({ target, held }) => {
        residency.status.mockResolvedValue(held)
        dialog.mockResolvedValue({ confirmed: true, checked: false })
        expect(await confirmPreviousStorageFiles(context(target))).toBe('connect')
        expect(dialog).toHaveBeenCalledExactlyOnceWith({
            title: language.lwwSync.previousFilesTitle, description: language.lwwSync.previousFilesDescription,
            checkboxLabel: language.lwwSync.downloadThenConnect, actionLabel: language.risuNest.serverSync.connect,
            cancelLabel: language.lwwSync.cancelAction, requireChecked: false,
        })
    })
    it('asks when other storages share files that count for none of them', async () => {
        residency.status.mockResolvedValue({ ...status(0, [{ connectionId: 'next', objects: 1 }]), remoteObjects: 3 })
        dialog.mockResolvedValue({ confirmed: true, checked: false })
        expect(await confirmPreviousStorageFiles(context(external))).toBe('connect')
        expect(dialog).toHaveBeenCalledOnce()
    })
    it.each([
        { result: { confirmed: false, checked: false }, choice: 'cancel' },
        { result: { confirmed: false, checked: true }, choice: 'cancel' },
        { result: { confirmed: true, checked: true }, choice: 'download-then-connect' },
    ])('answers $choice when confirmed is $result.confirmed and checked is $result.checked', async ({ result, choice }) => {
        residency.status.mockResolvedValue(status(1, []))
        dialog.mockResolvedValue(result)
        expect(await confirmPreviousStorageFiles(context(server))).toBe(choice)
    })
    it('downloads from every holder without a connection filter', async () => {
        residency.download.mockResolvedValue(status(0, []))
        await downloadPreviousStorageFiles()
        expect(residency.download).toHaveBeenCalledExactlyOnceWith()
    })
})
