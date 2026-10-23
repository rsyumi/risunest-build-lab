import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'

const mocks = vi.hoisted(() => ({
    capture: vi.fn(),
    acquire: vi.fn(),
    flushMarkers: vi.fn(),
    reserve: vi.fn(),
    releaseReservation: vi.fn(),
    releaseFence: vi.fn(),
}))

vi.mock('../process/generationState', () => ({ doingChat: writable(false) }))
vi.mock('./nativeFileJobManager', () => ({ nativeFileOperation: writable(null) }))
vi.mock('./persistentDataRuntime.svelte', () => ({
    persistentWorkingSetInputBlocked: writable(false),
    getPersistentDataRuntime: () => ({
        capturePersistentMutationToken: mocks.capture,
        acquireDestructiveReplacementFence: mocks.acquire,
    }),
}))
vi.mock('./deviceMarkers', () => ({ getDeviceMarkers: () => ({ flush: mocks.flushMarkers }) }))
vi.mock('./libraryFileOperation', () => ({ reserveLibraryFileOperation: mocks.reserve }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() => ({ runtime: 'tauri', os: 'android' })))
vi.mock('../androidNativeControl', () => ({}))

import { beginDeferredMobileBackgroundTask } from '../mobileBackgroundTask'
import { doingChat } from '../process/generationState'
import { nativeFileOperation } from './nativeFileJobManager'
import { persistentWorkingSetInputBlocked } from './persistentDataRuntime.svelte'
import { refreshWebView } from './webViewRefresh'

describe('WebView refresh save protection', () => {
    const blocked = persistentWorkingSetInputBlocked as ReturnType<typeof writable<boolean>>
    afterEach(() => { delete window.RisuBackgroundTasks })

    beforeEach(() => {
        vi.restoreAllMocks()
        vi.resetAllMocks()
        doingChat.set(false)
        nativeFileOperation.set(null)
        blocked.set(false)
        mocks.capture.mockResolvedValue({ revision: 7, mutationGeneration: 2 })
        mocks.acquire.mockResolvedValue({ release: mocks.releaseFence })
        mocks.flushMarkers.mockResolvedValue(undefined)
        mocks.reserve.mockReturnValue(mocks.releaseReservation)
        vi.spyOn(window.location, 'reload').mockImplementation(() => {})
        window.RisuBackgroundTasks = { begin: async () => 'deferred', progress: async () => {}, end: async () => {} }
    })

    it('waits for local data and device settings, retaining the edit fence through navigation', async () => {
        let save!: () => void
        let saveSettings!: () => void
        mocks.capture.mockImplementationOnce(() => new Promise(resolve => {
            save = () => resolve({ revision: 7, mutationGeneration: 2 })
        }))
        mocks.flushMarkers.mockImplementationOnce(() => new Promise<void>(resolve => { saveSettings = resolve }))

        const pending = refreshWebView()
        expect(mocks.capture).toHaveBeenCalledWith('webview-refresh', { publishOfficial: false })
        expect(mocks.acquire).not.toHaveBeenCalled()
        expect(window.location.reload).not.toHaveBeenCalled()
        save()
        await vi.waitFor(() => expect(mocks.flushMarkers).toHaveBeenCalledOnce())
        expect(mocks.acquire).toHaveBeenCalledWith({ revision: 7, mutationGeneration: 2 })
        expect(window.location.reload).not.toHaveBeenCalled()
        saveSettings()
        await pending
        expect(window.location.reload).toHaveBeenCalledOnce()
        expect(mocks.releaseFence).not.toHaveBeenCalled()
        expect(mocks.releaseReservation).not.toHaveBeenCalled()
    })

    it.each(['generation', 'file-operation', 'replacement'] as const)('rejects during %s before starting a save', async kind => {
        if (kind === 'generation') doingChat.set(true)
        if (kind === 'file-operation') nativeFileOperation.set({
            kind: 'import', startedAt: 0, observedStages: [], blocking: true,
            cancelRequested: false, partialWritesPossible: false, presentation: 'dialog',
        })
        if (kind === 'replacement') blocked.set(true)
        await expect(refreshWebView()).rejects.toThrow('webview-refresh-busy')
        expect(mocks.reserve).not.toHaveBeenCalled()
        expect(mocks.capture).not.toHaveBeenCalled()
        expect(window.location.reload).not.toHaveBeenCalled()
    })

    it('does not save or reload if another library operation owns the reservation', async () => {
        mocks.reserve.mockImplementationOnce(() => { throw new Error('library-file-operation-busy') })
        await expect(refreshWebView()).rejects.toThrow('library-file-operation-busy')
        expect(mocks.capture).not.toHaveBeenCalled()
        expect(window.location.reload).not.toHaveBeenCalled()
    })

    it.each(['capture', 'acquire', 'flushMarkers'] as const)('cancels reload and releases acquired protection when %s fails', async stage => {
        mocks[stage].mockRejectedValueOnce(new Error('save failed'))
        await expect(refreshWebView()).rejects.toThrow('save failed')
        expect(window.location.reload).not.toHaveBeenCalled()
        expect(mocks.releaseReservation).toHaveBeenCalledOnce()
        expect(mocks.releaseFence).toHaveBeenCalledTimes(stage === 'flushMarkers' ? 1 : 0)
        await refreshWebView()
        expect(window.location.reload).toHaveBeenCalledOnce()
    })

    it('cancels when an operation starts while settings are being saved', async () => {
        mocks.flushMarkers.mockImplementationOnce(async () => { doingChat.set(true) })
        await expect(refreshWebView()).rejects.toThrow('webview-refresh-busy')
        expect(window.location.reload).not.toHaveBeenCalled()
        expect(mocks.releaseFence).toHaveBeenCalledOnce()
        expect(mocks.releaseReservation).toHaveBeenCalledOnce()
    })

    it.each(['before-save', 'during-save'] as const)('keeps a deferred mobile owner alive when refresh is requested %s', async timing => {
        let task: Awaited<ReturnType<typeof beginDeferredMobileBackgroundTask>> | undefined
        if (timing === 'before-save') task = await beginDeferredMobileBackgroundTask('sync')
        else mocks.flushMarkers.mockImplementationOnce(async () => { task = await beginDeferredMobileBackgroundTask('sync') })
        try {
            await expect(refreshWebView()).rejects.toThrow('webview-refresh-busy')
            expect(window.location.reload).not.toHaveBeenCalled()
            if (timing === 'during-save') {
                expect(mocks.releaseFence).toHaveBeenCalledOnce()
                expect(mocks.releaseReservation).toHaveBeenCalledOnce()
            }
        } finally { await task?.dispose() }
        await refreshWebView()
        expect(window.location.reload).toHaveBeenCalledOnce()
    })

    it('releases protection if the WebView rejects navigation', async () => {
        vi.mocked(window.location.reload).mockImplementationOnce(() => { throw new Error('navigation failed') })
        await expect(refreshWebView()).rejects.toThrow('navigation failed')
        expect(mocks.releaseFence).toHaveBeenCalledOnce()
        expect(mocks.releaseReservation).toHaveBeenCalledOnce()
    })
})
