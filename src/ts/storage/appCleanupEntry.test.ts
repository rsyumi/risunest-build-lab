import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { appCleanupBeforeBootstrap } from './appCleanupEntry'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
const nativeWindow = window as Window & { __TAURI_INTERNALS__?: unknown }

describe('app cleanup startup gate', () => {
    beforeEach(() => {
        vi.resetAllMocks()
        nativeWindow.__TAURI_INTERNALS__ = {}
        document.body.innerHTML = '<div id="preloading"></div>'
    })
    afterEach(() => {
        delete nativeWindow.__TAURI_INTERNALS__
        document.body.innerHTML = ''
    })
    it('skips native calls on web', async () => {
        delete nativeWindow.__TAURI_INTERNALS__
        await appCleanupBeforeBootstrap()
        expect(invoke).not.toHaveBeenCalled()
    })
    it('allows normal startup only for a nonpending status', async () => {
        vi.mocked(invoke).mockResolvedValue({ pending: false, mode: null, error: null })
        await appCleanupBeforeBootstrap()
        expect(invoke).toHaveBeenCalledExactlyOnceWith('app_cleanup_status')
        expect(document.querySelector('main')).toBeNull()
    })
    it('automatically resumes pending cleanup once and never bootstraps after resolution', async () => {
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: null }).mockResolvedValue(undefined)
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('app_cleanup_resume'))
        expect(invoke).toHaveBeenCalledTimes(2)
        expect(bootstrap).not.toHaveBeenCalled()
        expect(document.querySelector('button')?.disabled).toBe(true)
        expect(document.getElementById('preloading')).toBeNull()
    })
    it('offers retry after failure without exposing native error strings', async () => {
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: null }).mockRejectedValueOnce(new Error('synthetic-private-path')).mockResolvedValue(undefined)
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(document.querySelector('button')?.disabled).toBe(false))
        expect(document.body.textContent).not.toContain('synthetic-private-path')
        document.querySelector('button')!.click()
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3))
        expect(bootstrap).not.toHaveBeenCalled()
    })
    it('blocks normal startup when cleanup status is unavailable', async () => {
        vi.mocked(invoke).mockRejectedValue(new Error('status-unavailable'))
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(document.querySelector('button')?.disabled).toBe(false))
        expect(bootstrap).not.toHaveBeenCalled()
        expect(invoke).not.toHaveBeenCalledWith('app_cleanup_resume')
    })
    it('shows actionable WebView update guidance and still blocks bootstrap', async () => {
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: null }).mockRejectedValue('cleanup-webview-update-required')
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(document.body.textContent).toContain('Update Android System WebView'))
        expect(document.querySelector('button')?.disabled).toBe(false)
        expect(bootstrap).not.toHaveBeenCalled()
    })
    it.each([
        ['cleanup-webview-update-required', 'Update Android System WebView'],
        ['synthetic-private-path', 'Local data deletion could not finish'],
    ])('waits for explicit retry when a previous cleanup failed with %s', async (error, message) => {
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'prepare-removal', error }).mockResolvedValue(undefined)
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(document.body.textContent).toContain(message))
        expect(invoke).toHaveBeenCalledExactlyOnceWith('app_cleanup_status')
        expect(document.body.textContent).not.toContain('synthetic-private-path')
        expect(document.querySelector('button')?.disabled).toBe(false)
        document.querySelector('button')!.click()
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('app_cleanup_resume'))
        expect(invoke).toHaveBeenCalledTimes(2)
        expect(bootstrap).not.toHaveBeenCalled()
    })
})
