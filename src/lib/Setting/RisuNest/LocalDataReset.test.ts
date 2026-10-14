import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import type { PlatformShape } from 'src/ts/platform.testSupport'

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), platform: { runtime: 'tauri', os: 'windows' } as PlatformShape }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule(() => mocks.platform))
import Component from './LocalDataReset.svelte'

let component: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
    vi.resetAllMocks()
    mocks.platform = { runtime: 'tauri', os: 'windows' }
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
        mocks.platform = { runtime: 'tauri', os: 'android' }
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
        mocks.platform = { runtime: 'web', os: 'windows' }
        expect((await setup()).querySelector('button')).toBeNull()
    })
    it.each(['android', 'ios'] as const)('offers reset without desktop removal on %s', async os => {
        mocks.platform = { runtime: 'tauri', os }
        const target = await setup()
        expect(target.querySelector('input')).toBeNull()
        expect(target.querySelector('button')?.textContent).toContain(languageEnglish.risuNest.cleanup.title)
    })
    it('confirms deletion scope and preserves cancellation before a removal request', async () => {
        const target = await setup()
        const checkbox = target.querySelector('input')!
        checkbox.checked = true
        checkbox.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()
        expect(target.querySelector('button')?.textContent).toContain(languageEnglish.risuNest.cleanup.deleteAndExit)
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
