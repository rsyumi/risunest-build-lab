import { afterEach, describe, expect, it, vi } from 'vitest'

const platform = vi.hoisted(() => ({ android: true }))

vi.mock('./platform', async () => (await import('./platform.testSupport')).platformModule(() => ({
    runtime: 'tauri', os: platform.android ? 'android' : 'windows',
})))

import { androidGenerationNotificationsEnabled, requestAndroidGenerationNotifications } from './androidGenerationKeepAlive'

describe('Android generation notification bridge', () => {
    afterEach(() => {
        platform.android = true
        delete window.RisuGenerationKeepAlive
    })

    it('does not use an injected bridge outside Tauri Android', async () => {
        const requestNotifications = vi.fn(async () => {})
        const notificationsEnabled = vi.fn(() => true)
        window.RisuGenerationKeepAlive = { requestNotifications, notificationsEnabled } as any
        platform.android = false

        await requestAndroidGenerationNotifications()
        expect(await androidGenerationNotificationsEnabled()).toBeNull()

        expect(requestNotifications).not.toHaveBeenCalled()
        expect(notificationsEnabled).not.toHaveBeenCalled()
    })

    it('reads an unavailable or rejecting bridge as unknown', async () => {
        expect(await androidGenerationNotificationsEnabled()).toBeNull()
        window.RisuGenerationKeepAlive = {
            requestNotifications: async () => { throw new Error('bridge unavailable') },
            notificationsEnabled: async () => { throw new Error('bridge unavailable') },
        } as any
        await expect(requestAndroidGenerationNotifications()).resolves.toBeUndefined()
        expect(await androidGenerationNotificationsEnabled()).toBeNull()
    })
})
