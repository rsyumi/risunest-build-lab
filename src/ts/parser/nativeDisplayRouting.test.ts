// @vitest-environment jsdom

import { beforeEach, expect, it, vi } from 'vitest'
import type { PlatformShape } from '../platform.testSupport'

const shape = vi.hoisted(() => ({ current: { runtime: 'tauri', os: 'windows' } as PlatformShape }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() => shape.current))
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => shape.current.os }))

import { createLocalDeviceMarkers } from '../storage/deviceMarkers'
import { loadDeviceSettings, updateNativeDisplayRender } from '../storage/deviceSettings'
import { routesDisplayNatively } from './nativeDisplayRouting'

/** `length` units holding `blocks` code blocks fenced with `fence` and `spans` `$$` spans. */
function message(length: number, blocks: number, spans = 0, fence = '```'): string {
    const text = `${fence}js\nf()\n${fence}\n`.repeat(blocks) + '$$x$$ '.repeat(spans)
    return text + 'a'.repeat(length - text.length)
}

beforeEach(() => {
    localStorage.clear()
    loadDeviceSettings(createLocalDeviceMarkers(localStorage))
    shape.current = { runtime: 'tauri', os: 'windows' }
})

it('routes a long message with a fenced block or a $$ span per 1,000 units to the app core on Windows', () => {
    expect(routesDisplayNatively(message(65_536, 66))).toBe(true)
    expect(routesDisplayNatively(message(65_535, 66))).toBe(false)
    expect(routesDisplayNatively(message(65_536, 65))).toBe(false)
    expect(routesDisplayNatively(message(65_536, 33, 33))).toBe(true)
    expect(routesDisplayNatively(message(65_536, 33, 32))).toBe(false)
    expect(routesDisplayNatively(message(65_536, 66, 0, '~~~'))).toBe(true)
})

it('leaves every other platform and the web build on TS', () => {
    for (const os of ['macos', 'linux', 'android', 'ios'] as const) {
        shape.current = { runtime: 'tauri', os }
        expect(routesDisplayNatively(message(65_536, 66)), os).toBe(false)
    }
    shape.current = { runtime: 'web', os: 'windows' }
    expect(routesDisplayNatively(message(65_536, 66))).toBe(false)
})

it('takes the route the device setting forces, and the rule again once it is cleared', () => {
    shape.current = { runtime: 'tauri', os: 'linux' }
    expect(updateNativeDisplayRender(true)).toBe(true)
    expect(routesDisplayNatively('short')).toBe(true)
    shape.current = { runtime: 'tauri', os: 'windows' }
    expect(updateNativeDisplayRender(false)).toBe(true)
    expect(routesDisplayNatively(message(65_536, 66))).toBe(false)
    expect(updateNativeDisplayRender(null)).toBe(true)
    expect(routesDisplayNatively(message(65_536, 66))).toBe(true)
    expect(updateNativeDisplayRender(true)).toBe(true)
    shape.current = { runtime: 'web', os: 'windows' }
    expect(routesDisplayNatively('short')).toBe(false)
})
