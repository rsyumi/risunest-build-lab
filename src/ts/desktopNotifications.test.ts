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
    it('invokes only commands the desktop capability allows in the main window', async () => {
        invoke.mockResolvedValue('granted')
        await notifyDesktop('Synthetic reply')
        await requestDesktopNotifications()
        const commands = invoke.mock.calls.map(([command]) => String(command))
        expect(commands).toHaveLength(2)
        for (const command of commands) {
            const [, plugin, name] = /^plugin:([^|]+)\|(.+)$/.exec(command) ?? []
            expect(desktopCapability.permissions, command).toContain(`${plugin}:allow-${name?.replaceAll('_', '-')}`)
        }
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
