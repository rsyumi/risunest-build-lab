import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, unmount } from 'svelte'
const mocked = vi.hoisted(() => ({
    db: { notification: false }, error: vi.fn(), request: vi.fn(), iosRequest: vi.fn(),
    platform: { isTauriAndroid: true, isTauriIOS: false },
}))
vi.mock('src/lang', () => ({ language: { notification: 'Notification', permissionDenied: 'Permission denied' } }))
vi.mock('src/ts/platform', () => mocked.platform)
vi.mock('src/ts/iosNative', () => ({ requestIOSNotifications: mocked.iosRequest }))
vi.mock('src/ts/androidGenerationKeepAlive', () => ({ requestAndroidGenerationNotifications: mocked.request }))
vi.mock('src/ts/alert', () => ({ alertError: mocked.error }))
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: mocked.db } }))
import NotificationToggle from './NotificationToggle.svelte'
let component: ReturnType<typeof mount> | undefined
beforeEach(() => {
    vi.clearAllMocks()
    mocked.db.notification = false
    mocked.platform.isTauriAndroid = true
    vi.stubGlobal('Notification', undefined)
})
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    delete window.RisuCompletionNotifications
    document.body.replaceChildren()
    vi.unstubAllGlobals()
})
function enable() {
    const target = document.createElement('div')
    document.body.append(target)
    component = mount(NotificationToggle, { target })
    target.querySelector<HTMLInputElement>('input')!.click()
}
describe('notification enablement', () => {
    it.each([true, false])('uses native Android permission with missing browser API, granted=%s', async granted => {
        window.RisuCompletionNotifications = { enabled: vi.fn(async () => granted), notify: vi.fn() }
        mocked.request.mockResolvedValue(undefined)
        enable()
        await vi.waitFor(() => expect(window.RisuCompletionNotifications?.enabled).toHaveBeenCalledOnce())
        expect(mocked.db.notification).toBe(granted)
        expect(mocked.error).toHaveBeenCalledTimes(granted ? 0 : 1)
    })
    it('reverts unavailable browser notifications without throwing', async () => {
        mocked.platform.isTauriAndroid = false
        enable()
        await vi.waitFor(() => expect(mocked.error).toHaveBeenCalledWith('Permission denied'))
        expect(mocked.db.notification).toBe(false)
    })
    it('requests permission in the browser prompt state', async () => {
        mocked.platform.isTauriAndroid = false
        const requestPermission = vi.fn(async () => 'granted')
        vi.stubGlobal('Notification', { permission: 'default', requestPermission })
        enable()
        await vi.waitFor(() => expect(requestPermission).toHaveBeenCalledOnce())
        expect(mocked.db.notification).toBe(true)
    })
})
