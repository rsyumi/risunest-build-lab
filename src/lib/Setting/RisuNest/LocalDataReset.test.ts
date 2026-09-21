import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), platform: { isTauri: true, isTauriAndroid: false, isTauriIOS: false } }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('src/ts/platform', () => mocks.platform)
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))
import Component from './LocalDataReset.svelte'

let component: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
    vi.resetAllMocks()
    Object.assign(mocks.platform, { isTauri: true, isTauriAndroid: false, isTauriIOS: false })
})
async function setup() {
    const target = document.createElement('div')
    document.body.append(target)
    component = mount(Component, { target })
    await tick()
    return target
}
describe('local data reset control', () => {
    it.each([
        ['cleanup-webview-update-required', languageEnglish.risuNest.cleanup.webViewUpdateRequired],
        ['synthetic-private-path', languageEnglish.risuNest.cleanup.failed],
    ])('maps native error %s without exposing unrelated native details', async (cause, expected) => {
        mocks.platform.isTauriAndroid = true
        mocks.invoke.mockRejectedValueOnce(cause)
        const target = await setup()
        target.querySelector('button')!.click()
        await tick()
        target.querySelector('button')!.click()
        await vi.waitFor(() => expect(target.querySelector('[role=alert]')?.textContent).toBe(expected))
        expect(target.querySelector('button')!.disabled).toBe(false)
        expect(target.textContent).not.toContain('synthetic-private-path')
    })
    it('hides the entire action on web', async () => {
        mocks.platform.isTauri = false
        expect((await setup()).querySelector('button')).toBeNull()
    })
    it.each(['isTauriAndroid', 'isTauriIOS'] as const)('offers reset without desktop removal on %s', async platform => {
        mocks.platform[platform] = true
        const target = await setup()
        expect(target.querySelector('input')).toBeNull()
        expect(target.querySelector('button')?.textContent).toContain('Reset')
    })
    it('confirms deletion scope and preserves cancellation before a removal request', async () => {
        const target = await setup()
        const checkbox = target.querySelector('input')!
        checkbox.checked = true
        checkbox.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()
        expect(target.querySelector('button')?.textContent).toContain('Delete and exit')
        target.querySelector('button')!.click()
        await tick()
        expect(target.textContent).toContain(languageEnglish.risuNest.cleanup.scope)
        expect(target.textContent).toContain(languageEnglish.risuNest.cleanup.confirmRemoval)
        expect(mocks.invoke).not.toHaveBeenCalled()
        target.querySelectorAll('button')[1].click()
        await tick()
        expect(mocks.invoke).not.toHaveBeenCalled()
        target.querySelector('button')!.click()
        await tick()
        target.querySelector('button')!.click()
        await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith('app_cleanup_request', { mode: 'prepare-removal' }))
        expect(target.querySelector('button')!.disabled).toBe(true)
    })
})
