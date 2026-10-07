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
        for (const key of ['userAgent', 'language']) Reflect.deleteProperty(navigator, key)
    })
    const stubNavigator = (values: { userAgent: string, language?: string }) => {
        for (const [key, value] of Object.entries(values)) Object.defineProperty(navigator, key, { value, configurable: true })
    }
    const windowsAgent = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36'
    const androidAgent = 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36'
    const statusText = () => document.querySelector('[role="status"]')?.textContent
    const cancelButton = () => Array.from(document.querySelectorAll('button')).find(button => button.textContent === 'Cancel reset' || button.textContent === '초기화 취소')!
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
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: null, canCancel: true })
            .mockRejectedValueOnce(new Error('synthetic-private-path'))
            .mockResolvedValueOnce({ pending: true, mode: 'reset', error: 'synthetic-private-path', canCancel: true })
            .mockResolvedValue(undefined)
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(document.querySelector('button')?.disabled).toBe(false))
        expect(document.body.textContent).not.toContain('synthetic-private-path')
        document.querySelector('button')!.click()
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(4))
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

    it('allows normal startup when only cleanup is unavailable', async () => {
        vi.mocked(invoke).mockResolvedValue({ pending: false, mode: null, error: 'cleanup-path-redirected', canCancel: false })
        await appCleanupBeforeBootstrap()
        expect(document.querySelector('main')).toBeNull()
    })

    it('offers cancellation only when native status proves roots are intact', async () => {
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: 'secret-cleanup-unavailable', canCancel: true })
            .mockResolvedValue(undefined)
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(document.body.textContent).toContain('start and unlock the keyring'))
        expect(document.body.textContent).toContain('reconnect sync and external storage')
        const cancel = Array.from(document.querySelectorAll('button')).find(button => button.textContent === 'Cancel reset')!
        expect(cancel.hidden).toBe(false)
        cancel.click()
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('app_cleanup_cancel'))
        expect(bootstrap).not.toHaveBeenCalled()
    })

    it('refreshes cancellation eligibility after a retry enters root deletion', async () => {
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: 'secret-cleanup-unavailable', canCancel: true })
            .mockRejectedValueOnce('cleanup-files-busy-or-denied')
            .mockResolvedValueOnce({ pending: true, mode: 'reset', error: 'cleanup-files-busy-or-denied', canCancel: false })
        void appCleanupBeforeBootstrap()
        await vi.waitFor(() => expect(document.querySelector('button')?.disabled).toBe(false))
        document.querySelector('button')!.click()
        await vi.waitFor(() => expect(document.body.textContent).toContain('Check file permissions'))
        const cancel = Array.from(document.querySelectorAll('button')).find(button => button.textContent === 'Cancel reset')!
        expect(cancel.hidden).toBe(true)
        expect(invoke).not.toHaveBeenCalledWith('app_cleanup_cancel')
    })

    it('keeps a corrupt journal fail closed and names the record to remove outside the app', async () => {
        stubNavigator({ userAgent: windowsAgent, language: 'en-US' })
        vi.mocked(invoke).mockResolvedValue({ pending: true, mode: null, error: 'cleanup-journal-corrupt', canCancel: false })
        const bootstrap = vi.fn()
        void appCleanupBeforeBootstrap().then(bootstrap)
        await vi.waitFor(() => expect(statusText()).toBe(
            'The deletion record is damaged, so the reset cannot continue. Quit the app, delete the %LOCALAPPDATA%\\RisuNest-cleanup folder, and start RisuNest again. You can then run the reset again from the settings.',
        ))
        expect(cancelButton().hidden).toBe(true)
        expect(bootstrap).not.toHaveBeenCalled()
    })

    it('tells Android users to clear the app data when the record is damaged', async () => {
        stubNavigator({ userAgent: androidAgent, language: 'ko-KR' })
        vi.mocked(invoke).mockResolvedValue({ pending: true, mode: null, error: 'cleanup-journal-corrupt', canCancel: false })
        void appCleanupBeforeBootstrap()
        await vi.waitFor(() => expect(statusText()).toBe(
            '삭제 기록이 손상되어 초기화를 진행할 수 없습니다. 앱 정보 > 저장공간에서 데이터를 삭제해주세요.',
        ))
        expect(cancelButton().hidden).toBe(true)
    })

    it('points to Cancel reset when a damaged credential record left the data intact', async () => {
        stubNavigator({ userAgent: windowsAgent, language: 'ko-KR' })
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: 'secret-index-corrupt', canCancel: true })
            .mockResolvedValue(undefined)
        void appCleanupBeforeBootstrap()
        await vi.waitFor(() => expect(statusText()).toBe(
            '삭제 기록이 손상되어 초기화를 완료하지 못했습니다. 초기화 취소를 눌러 앱을 시작해주세요.',
        ))
        expect(cancelButton().hidden).toBe(false)
        cancelButton().click()
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('app_cleanup_cancel'))
    })

    it('falls back to removing the record outside the app when cancellation is unavailable', async () => {
        stubNavigator({ userAgent: windowsAgent, language: 'en-US' })
        vi.mocked(invoke).mockResolvedValue({ pending: true, mode: 'reset', error: 'secret-index-corrupt', canCancel: false })
        void appCleanupBeforeBootstrap()
        await vi.waitFor(() => expect(statusText()).toContain('Quit the app, delete the %LOCALAPPDATA%\\RisuNest-cleanup folder'))
        expect(statusText()).not.toContain('Cancel reset')
        expect(cancelButton().hidden).toBe(true)
    })

    it('reloads safely if cancellation cleared the journal but navigation failed', async () => {
        const reload = vi.spyOn(location, 'reload').mockImplementation(() => {})
        vi.mocked(invoke).mockResolvedValueOnce({ pending: true, mode: 'reset', error: 'secret-cleanup-unavailable', canCancel: true })
            .mockRejectedValueOnce('cleanup-navigation-failed')
            .mockResolvedValueOnce({ pending: false, mode: null, error: null, canCancel: false })
        void appCleanupBeforeBootstrap()
        await vi.waitFor(() => expect(document.querySelectorAll('button')).toHaveLength(2))
        document.querySelectorAll('button')[1].click()
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3))
        document.querySelector('button')!.click()
        await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce())
        expect(invoke).not.toHaveBeenCalledWith('app_cleanup_resume')
        reload.mockRestore()
    })
})
