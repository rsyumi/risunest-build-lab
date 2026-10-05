import { beforeEach, describe, expect, it, vi } from 'vitest'
const { invoke, alertError, waitAlert } = vi.hoisted(() => ({ invoke: vi.fn(), alertError: vi.fn(), waitAlert: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
vi.mock('../alert', () => ({ alertError, waitAlert }))
vi.mock('../gui/nativeFileJobDialogModel', () => ({ failureReason: (code: string) => `reason:${code}` }))
import { exportIOSFile, pickIOSFile, getIOSPublication, pickIOSBackupSource, materializeIOSBackupSource, reportInterruptedIOSBackupSources } from './iosFiles'
beforeEach(() => {
    invoke.mockReset()
    alertError.mockReset()
    waitAlert.mockReset().mockResolvedValue(undefined)
})
describe('iOS file publication', () => {
    it('returns native custody without an app-owned copy or a descriptor in JavaScript', async () => {
        invoke.mockImplementation(async command => command === 'native_portable_source_cleanup_orphans' ? 0
            : {cancelled:false, token:'selected-token', name:'synthetic.risunest', bytes:4_294_967_296})
        await expect(pickIOSBackupSource()).resolves.toEqual({token:'selected-token', name:'synthetic.risunest', bytes:4_294_967_296})
        expect(invoke.mock.calls.map(call => call[0])).toEqual(['native_portable_source_cleanup_orphans','plugin:ios-native|pick_backup_source'])
        expect(alertError).not.toHaveBeenCalled()
    })
    it('says an earlier import did not finish before the picker opens', async () => {
        const order: string[] = []
        invoke.mockImplementation(async command => {
            order.push(command)
            return command === 'native_portable_source_cleanup_orphans' ? 1
                : {cancelled:false, token:'selected-token', name:'synthetic.risunest', bytes:42}
        })
        alertError.mockImplementation(message => order.push(`alert:${message}`))
        waitAlert.mockImplementation(async () => { order.push('dismissed') })
        await pickIOSBackupSource()
        expect(order).toEqual(['native_portable_source_cleanup_orphans','alert:reason:import-interrupted','dismissed','plugin:ios-native|pick_backup_source'])
    })
    it('reports sources a reloaded page left unimported once', async () => {
        invoke.mockResolvedValueOnce(2).mockResolvedValueOnce(0)
        await reportInterruptedIOSBackupSources()
        await reportInterruptedIOSBackupSources()
        expect(invoke.mock.calls).toEqual([['native_portable_source_cleanup_orphans'],['native_portable_source_cleanup_orphans']])
        expect(alertError).toHaveBeenCalledExactlyOnceWith('reason:import-interrupted')
    })
    it('releases native and scoped custody after cancellation during selection', async () => {
        const controller = new AbortController()
        invoke.mockImplementation(async command => {
            if (command === 'plugin:ios-native|pick_backup_source') {
                controller.abort()
                return {cancelled:false, token:'selected-token', name:'synthetic.risunest', bytes:42}
            }
            if (command === 'native_portable_source_cleanup_orphans') return 0
            return true
        })
        await expect(pickIOSBackupSource(controller.signal)).rejects.toMatchObject({name:'AbortError'})
        expect(invoke).toHaveBeenLastCalledWith('native_portable_source_discard', {source:{type:'iosScoped', token:'selected-token'}})
    })
    it('requests an upstream compatibility copy by token, never reopening a selected URL', async () => {
        invoke.mockResolvedValue({path:'/owned/synthetic.risudat',name:'synthetic.risudat',bytes:42})
        await expect(materializeIOSBackupSource('selected-token')).resolves.toEqual({path:'/owned/synthetic.risudat',name:'synthetic.risudat',bytes:42})
        expect(invoke).toHaveBeenCalledExactlyOnceWith('plugin:ios-native|materialize_backup_source',{token:'selected-token'})
    })
    it('reports picker cancellation without publishing success', async () => {
        invoke.mockResolvedValue({ cancelled: true })
        await expect(
            exportIOSFile({
                sourcePath: '/owned/file',
                suggestedName: 'backup.risunest',
            }),
        ).rejects.toMatchObject({ name: 'AbortError' })
    })
    it('cleans the app-owned import copy when cancellation arrives during the picker', async () => {
        const controller = new AbortController()
        invoke.mockImplementation(async (command) => {
            if (command.endsWith('pick_file')) {
                controller.abort()
                return {
                    path: '/staged/file',
                    name: 'synthetic',
                    bytes: 1,
                    cancelled: false,
                }
            }
        })
        await expect(pickIOSFile(controller.signal)).rejects.toMatchObject({
            name: 'AbortError',
        })
        expect(invoke).toHaveBeenLastCalledWith(
            'plugin:ios-native|discard_file',
            { path: '/staged/file' },
        )
    })
    it('keeps a completed publication successful after a late abort', async () => {
        const controller = new AbortController()
        invoke.mockImplementation(async () => {
            controller.abort()
            return { cancelled: false, bytes: 42 }
        })
        await expect(
            exportIOSFile({
                sourcePath: '/owned/file',
                suggestedName: 'backup.risunest',
                signal: controller.signal,
            }),
        ).resolves.toEqual({ bytes: 42 })
    })
    it('retains recoverable receipts until the caller has stored its publication result', async () => {
        invoke.mockResolvedValue({ cancelled: false, bytes: 42 })
        await exportIOSFile({ sourcePath: '/owned/file', suggestedName: 'backup.risunest', requestId: 'caller-owned' })
        expect(invoke).toHaveBeenCalledOnce()
        invoke.mockClear()
        await exportIOSFile({ sourcePath: '/owned/file', suggestedName: 'backup.risunest' })
        expect(invoke).toHaveBeenLastCalledWith('plugin:ios-native|acknowledge_publication', { id: expect.any(String) })
    })
    it('never treats a pending publication receipt as success', async () => {
        invoke.mockResolvedValue({ state: 'pending' })
        await expect(getIOSPublication('receipt')).resolves.toBeNull()
        invoke.mockResolvedValue({ state: 'succeeded', bytes: 42 })
        await expect(getIOSPublication('receipt')).resolves.toEqual({
            bytes: 42,
        })
    })
})
