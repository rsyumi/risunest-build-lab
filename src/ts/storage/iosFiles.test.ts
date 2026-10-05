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
        invoke.mockImplementation(async command => command === 'native_portable_source_cleanup_orphans' ? {names:[],cleanupFailed:false}
            : {cancelled:false, token:'selected-token', name:'synthetic.risunest', bytes:4_294_967_296})
        await expect(pickIOSBackupSource()).resolves.toEqual({token:'selected-token', name:'synthetic.risunest', bytes:4_294_967_296})
        expect(invoke.mock.calls.map(call => call[0])).toEqual(['native_portable_source_cleanup_orphans','plugin:ios-native|pick_backup_source'])
        expect(alertError).not.toHaveBeenCalled()
    })
    it('says an earlier import did not finish before the picker opens', async () => {
        const order: string[] = []
        invoke.mockImplementation(async command => {
            order.push(command)
            return command === 'native_portable_source_cleanup_orphans' ? {names:['earlier.risunest'],cleanupFailed:false}
                : {cancelled:false, token:'selected-token', name:'synthetic.risunest', bytes:42}
        })
        alertError.mockImplementation(message => order.push(`alert:${message}`))
        waitAlert.mockImplementation(async () => { order.push('dismissed') })
        await pickIOSBackupSource()
        expect(order).toEqual(['native_portable_source_cleanup_orphans','alert:earlier.risunest: reason:import-interrupted','dismissed','plugin:ios-native|pick_backup_source'])
    })
    it('reports sources a reloaded page left unimported once', async () => {
        invoke.mockResolvedValueOnce({names:['one.risunest','two.risunest'],cleanupFailed:false}).mockResolvedValueOnce({names:[],cleanupFailed:false})
        await reportInterruptedIOSBackupSources()
        await reportInterruptedIOSBackupSources()
        expect(invoke.mock.calls).toEqual([['native_portable_source_cleanup_orphans'],['native_portable_source_cleanup_orphans']])
        expect(alertError.mock.calls).toEqual([['one.risunest: reason:import-interrupted'], ['two.risunest: reason:import-interrupted']])
    })
    it('keeps unavailable names anonymous and never displays a source path', async () => {
        invoke.mockResolvedValueOnce({names:[null],cleanupFailed:false})
        await reportInterruptedIOSBackupSources()
        expect(alertError).toHaveBeenCalledExactlyOnceWith('reason:import-interrupted')
        invoke.mockResolvedValueOnce({names:['/private/synthetic.risunest'],cleanupFailed:false})
        await expect(reportInterruptedIOSBackupSources()).rejects.toThrow('receipt is invalid')
        expect(alertError).toHaveBeenCalledOnce()
    })
    it('delivers cleaned receipts before refusing the next pick and retries only unfinished cleanup', async () => {
        invoke.mockResolvedValueOnce({names:['first.risunest'],cleanupFailed:true})
        await expect(pickIOSBackupSource()).rejects.toThrow('cleanup failed')
        expect(invoke).not.toHaveBeenCalledWith('plugin:ios-native|pick_backup_source')
        invoke.mockResolvedValueOnce({names:['second.risunest'],cleanupFailed:false})
            .mockResolvedValueOnce({cancelled:true})
        await expect(pickIOSBackupSource()).resolves.toBeNull()
        expect(alertError.mock.calls).toEqual([['first.risunest: reason:import-interrupted'], ['second.risunest: reason:import-interrupted']])
    })
    it('joins simultaneous startup and picker notices until dismissal', async () => {
        let dismiss!: () => void
        waitAlert.mockImplementationOnce(() => new Promise<void>(resolve => { dismiss = resolve }))
        invoke.mockResolvedValueOnce({names:['first.risunest'],cleanupFailed:false})
            .mockResolvedValueOnce({cancelled:true})
        const notice = reportInterruptedIOSBackupSources()
        const pick = pickIOSBackupSource()
        await vi.waitFor(() => expect(alertError).toHaveBeenCalledOnce())
        expect(invoke).toHaveBeenCalledOnce()
        dismiss()
        await Promise.all([notice, pick])
        expect(invoke).toHaveBeenLastCalledWith('plugin:ios-native|pick_backup_source')
    })
    it('releases native and scoped custody after cancellation during selection', async () => {
        const controller = new AbortController()
        invoke.mockImplementation(async command => {
            if (command === 'plugin:ios-native|pick_backup_source') {
                controller.abort()
                return {cancelled:false, token:'selected-token', name:'synthetic.risunest', bytes:42}
            }
            if (command === 'native_portable_source_cleanup_orphans') return {names:[],cleanupFailed:false}
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
