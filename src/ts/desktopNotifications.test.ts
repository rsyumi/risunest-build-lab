import { readFileSync } from 'node:fs'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

import { notifyDesktop, requestDesktopNotifications } from './desktopNotifications'

const desktopCapability = JSON.parse(readFileSync('src-tauri/capabilities/desktop.json', 'utf8')) as {
    windows: string[]
    platforms: string[]
    permissions: string[]
}

beforeEach(() => invoke.mockReset())

describe('desktop notifications', () => {
    it('sends content to the native activation owner and preserves permission behavior', async () => {
        invoke.mockResolvedValue('granted')
        await notifyDesktop('Synthetic reply')
        await requestDesktopNotifications()
        expect(invoke.mock.calls).toEqual([
            ['desktop_notify', { body: 'Synthetic reply' }],
            ['plugin:notification|request_permission'],
        ])
        expect(desktopCapability.permissions).toContain('notification:allow-request-permission')
        expect(desktopCapability.windows).toContain('main')
        expect(desktopCapability.platforms).toEqual(expect.arrayContaining(['windows', 'macOS', 'linux']))
    })

    it.each([['granted', true], ['denied', false], ['prompt', false]] as const)(
        'reports the plugin permission %s as enabled=%s',
        async (state, enabled) => {
            invoke.mockResolvedValue(state)
            await expect(requestDesktopNotifications()).resolves.toBe(enabled)
        },
    )
})
