import { afterEach, beforeEach, expect, test, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    checkNativeStartupStatus: vi.fn(),
    invoke: vi.fn(),
}))

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('../../nativeStartup', () => ({
    checkNativeStartupStatus: mocks.checkNativeStartupStatus,
}))

import { deviceMaintenanceBeforeBootstrap } from './entry'

beforeEach(() => {
    Object.defineProperty(window, '__TAURI_INTERNALS__', {
        configurable: true,
        value: {},
    })
    vi.clearAllMocks()
})

afterEach(() => {
    Reflect.deleteProperty(window, '__TAURI_INTERNALS__')
})

test('returns to the normal app entry when native setup failed before device recovery', async () => {
    mocks.checkNativeStartupStatus.mockRejectedValueOnce(
        new Error('synthetic native setup failure'),
    )

    await expect(deviceMaintenanceBeforeBootstrap()).resolves.toBeUndefined()

    expect(mocks.checkNativeStartupStatus).toHaveBeenCalledOnce()
    expect(mocks.invoke).not.toHaveBeenCalled()
    expect(document.querySelector('main[role="status"]')).toBeNull()
})

test('covers the viewport with the retry panel when device recovery cannot finish', async () => {
    // The page keeps #app at full height and does not let the body scroll.
    const app = document.createElement('div')
    app.id = 'app'
    document.body.append(app)
    mocks.checkNativeStartupStatus.mockResolvedValueOnce(undefined)
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'native_device_backup_bootstrap') return {mode: 'maintenance', session: {sessionId: 'synthetic-session', action: 'native-complete', includesLibrary: true}}
        if (command === 'native_device_backup_recovery_complete') throw new Error('synthetic recovery failure')
        throw new Error(`Unexpected command: ${command}`)
    })
    const reload = vi.fn()
    void deviceMaintenanceBeforeBootstrap({reload})

    const panel = await vi.waitFor(() => {
        const found = document.querySelector<HTMLElement>('main[role="status"]')
        expect(found).not.toBeNull()
        return found!
    })
    expect(panel.style.position).toBe('fixed')
    expect(panel.style.inset).toBe('0')
    expect(panel.style.overflow).toBe('auto')
    expect(panel.style.background).toContain('--risu-theme-bgcolor')
    expect(panel.style.color).toContain('--risu-theme-textcolor')
    const retry = [...panel.querySelectorAll('button')].find(button => button.textContent === 'Retry recovery')!
    retry.click()
    expect(reload).toHaveBeenCalledOnce()
    panel.remove()
    app.remove()
    localStorage.removeItem('risuNestServerSyncRestoreHold')
})
