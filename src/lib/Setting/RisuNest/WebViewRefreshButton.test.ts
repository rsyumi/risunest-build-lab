import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { language } from 'src/lang'

const mocks = vi.hoisted(() => ({ refresh: vi.fn(), alertError: vi.fn() }))
vi.mock('src/ts/storage/webViewRefresh', () => ({ refreshWebView: mocks.refresh }))
vi.mock('src/ts/alert', () => ({ alertError: mocks.alertError }))

import WebViewRefreshButton from './WebViewRefreshButton.svelte'

describe('WebView refresh button', () => {
    let component: ReturnType<typeof mount> | undefined

    afterEach(async () => {
        if (component) await unmount(component)
        component = undefined
        document.body.replaceChildren()
        vi.resetAllMocks()
    })

    function render(): HTMLButtonElement {
        component = mount(WebViewRefreshButton, { target: document.body })
        const buttons = document.querySelectorAll('button')
        expect(buttons).toHaveLength(1)
        expect(buttons[0].textContent?.trim()).toBe(language.risuNest.platform.refreshScreen)
        return buttons[0]
    }

    it('blocks repeated clicks during saving and keeps the button busy until navigation', async () => {
        let complete!: () => void
        mocks.refresh.mockImplementationOnce(() => new Promise<void>(resolve => { complete = resolve }))
        const button = render()
        button.click()
        button.click()
        await vi.waitFor(() => expect(mocks.refresh).toHaveBeenCalledOnce())
        await tick()
        expect(button.disabled).toBe(true)
        expect(button.getAttribute('aria-busy')).toBe('true')
        button.click()
        complete()
        await tick()
        expect(mocks.refresh).toHaveBeenCalledOnce()
        expect(button.disabled).toBe(true)
        expect(mocks.alertError).not.toHaveBeenCalled()
    })

    it('shows a failure and allows a retry when saving fails', async () => {
        mocks.refresh.mockRejectedValueOnce(new Error('save failed'))
        const button = render()
        button.click()
        await vi.waitFor(() => expect(mocks.alertError).toHaveBeenCalledWith(language.risuNest.platform.refreshFailed))
        await tick()
        expect(button.disabled).toBe(false)
        button.click()
        await vi.waitFor(() => expect(mocks.refresh).toHaveBeenCalledTimes(2))
    })

    it('explains when ongoing work prevents a refresh', async () => {
        mocks.refresh.mockRejectedValueOnce(new Error('webview-refresh-busy'))
        const button = render()
        button.click()
        await vi.waitFor(() => expect(mocks.alertError).toHaveBeenCalledWith(language.risuNest.platform.refreshBusy))
        await tick()
        expect(button.disabled).toBe(false)
    })
})
