import { beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { doingChat, reserveGeneration } from '../../process/generationState'
import { isLibraryFileOperationReserved } from '../libraryFileOperation'
import { cancelActiveNativeFileOperation, dismissNativeFileOperationOutcome, nativeFileOperation, nativeFileOperationOutcome } from '../nativeFileJobManager'
import { publishNativeOfficialAccountBackup, restoreNativeOfficialAccountBackup } from './nativeOfficialAccountOperations'

const mocks = vi.hoisted(() => ({ restore: vi.fn(), publish: vi.fn(), assertAvailable: vi.fn() }))
vi.mock('./nativeOfficialAccountFlow', () => ({ getNativeOfficialAccountFlow: () => mocks }))
vi.mock('./serverSyncProduction', () => ({ getServerSyncController: () => ({ assertFileOperationAvailable: mocks.assertAvailable }) }))
vi.mock('../../mobileBackgroundTask', () => ({
    measuredTaskPercent: () => 0,
    runWithMobileBackgroundTask: (_kind: string, operation: (task: unknown) => Promise<unknown>, signal: AbortSignal) => operation({ signal, progress: vi.fn() }),
}))

beforeEach(() => { vi.clearAllMocks(); dismissNativeFileOperationOutcome(); doingChat.set(false) })

describe('shared official account operations', () => {
    it('keeps publication alive across callers and exposes measured asset progress', async () => {
        let finish!: () => void
        mocks.publish.mockImplementation(async (signal, progress) => {
            progress(2, 5)
            expect(signal.aborted).toBe(false)
            await new Promise<void>(resolve => { finish = resolve })
        })
        const first = publishNativeOfficialAccountBackup()
        const remount = publishNativeOfficialAccountBackup()
        expect(first).toBe(remount)
        expect(mocks.publish).toHaveBeenCalledOnce()
        expect(get(nativeFileOperation)?.status?.detail?.stage).toBe('publishing-destination')
        expect(get(nativeFileOperation)?.status?.detail?.stageCompleted).toBe(2)
        expect(get(nativeFileOperation)?.status?.detail?.stageTotal).toBe(5)
        finish()
        await first
        expect(get(nativeFileOperationOutcome)?.state).toBe('succeeded')
    })

    it('reserves library work, forwards cancellation, and preserves the pre-activation outcome', async () => {
        mocks.restore.mockImplementation(async options => {
            await new Promise((_resolve, reject) => options.signal.addEventListener('abort', () => reject(options.signal.reason)))
        })
        const pending = restoreNativeOfficialAccountBackup()
        expect(isLibraryFileOperationReserved()).toBe(true)
        expect(reserveGeneration()).toBeNull()
        expect(mocks.restore.mock.calls[0][0]).toEqual(expect.objectContaining({ onStatus: expect.any(Function), onBlockingChange: expect.any(Function) }))
        cancelActiveNativeFileOperation()
        await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
        expect(isLibraryFileOperationReserved()).toBe(false)
        expect(get(nativeFileOperationOutcome)?.state).toBe('cancelled')
    })

    it.each(['missing', 'unchanged', 'kept-local'])('returns %s without a false success dialog', async kind => {
        mocks.restore.mockResolvedValue({ kind })
        await expect(restoreNativeOfficialAccountBackup()).resolves.toEqual({ kind })
        expect(get(nativeFileOperationOutcome)).toBeNull()
    })

    it('rejects generation and sync preflight before starting account work', async () => {
        doingChat.set(true)
        await expect(restoreNativeOfficialAccountBackup()).rejects.toMatchObject({ code: 'generation-active' })
        expect(mocks.restore).not.toHaveBeenCalled()
        doingChat.set(false)
        mocks.assertAvailable.mockImplementationOnce(() => { throw new Error('server-sync-busy') })
        expect(() => restoreNativeOfficialAccountBackup()).toThrow('server-sync-busy')
        expect(mocks.restore).not.toHaveBeenCalled()
    })
})
