import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import type { PlatformShape } from 'src/ts/platform.testSupport'
import type { AppCleanupStatus } from 'src/ts/storage/appCleanup'

const idle = (): AppCleanupStatus => ({ pending: false, mode: null, error: null, canCancel: false, profile: null, namedProfiles: false })
const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    platform: { runtime: 'tauri', os: 'windows' } as PlatformShape,
    status: undefined as AppCleanupStatus | undefined,
    requestError: undefined as unknown,
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('src/ts/platform', async () => (await import('src/ts/platform.testSupport')).platformModule(() => mocks.platform))
import Component from './LocalDataReset.svelte'

const text = languageEnglish.risuNest.cleanup
let component: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
    vi.resetAllMocks()
    mocks.platform = { runtime: 'tauri', os: 'windows' }
    mocks.status = undefined
    mocks.requestError = undefined
})
async function setup() {
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'app_cleanup_status') return mocks.status ?? idle()
        if (command === 'app_cleanup_request' && mocks.requestError !== undefined) throw mocks.requestError
    })
    const target = document.createElement('div')
    document.body.append(target)
    component = mount(Component, { target })
    await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith('app_cleanup_status'))
    await tick()
    return target
}
async function confirmRemoval(target: HTMLElement) {
    const checkbox = target.querySelector('input')!
    checkbox.checked = true
    checkbox.dispatchEvent(new Event('change', { bubbles: true }))
    await tick()
    target.querySelector('button')!.click()
    await tick()
}
const requested = () => mocks.invoke.mock.calls.filter(([command]) => command === 'app_cleanup_request')
describe('local data reset control', () => {
    it.each([
        ['android', 'cleanup-webview-update-required', text.webViewUpdateRequired],
        ['android', 'synthetic-private-path', text.failed],
        ['windows', 'cleanup-other-profiles-running', text.otherProfilesRunning],
    ] as const)('maps native error %s/%s without exposing unrelated native details', async (os, cause, expected) => {
        mocks.platform = { runtime: 'tauri', os }
        mocks.requestError = cause
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
        const target = document.createElement('div')
        document.body.append(target)
        component = mount(Component, { target })
        await tick()
        expect(target.querySelector('button')).toBeNull()
        expect(mocks.invoke).not.toHaveBeenCalled()
    })
    it.each(['android', 'ios'] as const)('offers reset without desktop removal on %s', async os => {
        mocks.platform = { runtime: 'tauri', os }
        const target = await setup()
        expect(target.querySelector('input')).toBeNull()
        expect(target.querySelector('button')?.textContent).toContain(text.title)
    })
    it('confirms deletion scope and preserves cancellation before a removal request', async () => {
        const target = await setup()
        const checkbox = target.querySelector('input')!
        checkbox.checked = true
        checkbox.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()
        expect(target.querySelector('button')?.textContent).toContain(text.deleteAndExit)
        target.querySelector('button')!.click()
        await tick()
        expect(target.textContent).toContain(text.scope)
        expect(target.textContent).toContain(text.removalHelp)
        expect(target.textContent).toContain(text.confirmRemoval)
        expect(requested()).toEqual([])
        target.querySelectorAll('button')[1].click()
        await tick()
        expect(requested()).toEqual([])
        target.querySelector('button')!.click()
        await tick()
        target.querySelector('button')!.click()
        await vi.waitFor(() => expect(requested()).toEqual([['app_cleanup_request', { mode: 'prepare-removal' }]]))
        expect(target.querySelector('button')!.disabled).toBe(true)
    })
    it.each([
        [null, text.scope],
        ['work', text.scopeProfile],
    ])('describes every profile in removal when named profiles exist (running %s)', async (profile, scope) => {
        mocks.status = { ...idle(), profile, namedProfiles: true }
        const target = await setup()
        await confirmRemoval(target)
        expect(target.textContent).toContain(scope)
        expect(target.textContent).toContain(text.removalHelpProfiles)
        expect(target.textContent).toContain(text.confirmRemovalProfiles)
        expect(target.textContent).not.toContain(text.removalHelp)
        expect(target.textContent).not.toContain(text.confirmRemoval)
    })
})
