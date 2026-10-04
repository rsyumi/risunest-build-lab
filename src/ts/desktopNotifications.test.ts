import { beforeEach, describe, expect, it, vi } from 'vitest'

const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

import { notifyDesktop, requestDesktopNotifications } from './desktopNotifications'

beforeEach(() => invoke.mockReset())

describe('desktop notifications', () => {
    it('sends the completion text through the notification plugin', async () => {
        invoke.mockResolvedValue(undefined)
        await notifyDesktop('Done')
        expect(invoke).toHaveBeenCalledExactlyOnceWith(
            'plugin:notification|notify',
            { options: { title: 'RisuNest', body: 'Done' } },
        )
    })

    it.each([['granted', true], ['denied', false], ['prompt', false]] as const)(
        'reports the plugin permission %s as enabled=%s',
        async (state, enabled) => {
            invoke.mockResolvedValue(state)
            await expect(requestDesktopNotifications()).resolves.toBe(enabled)
            expect(invoke).toHaveBeenCalledExactlyOnceWith('plugin:notification|request_permission')
        },
    )
})
