// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get } from 'svelte/store'
import type { NativeUpdateEnvironment } from 'src/ts/update/manifest'

const mocks = vi.hoisted(() => ({ environment: vi.fn(), check: vi.fn(), unsubscribe: vi.fn() }))
vi.mock('src/ts/update/native', () => ({ nativeUpdate: { environment: mocks.environment } }))
vi.mock('src/ts/update/controller', () => ({ checkForAppUpdate: mocks.check, clearSkippedAppUpdate: vi.fn() }))
vi.mock('src/ts/globalApi.svelte', () => ({ openURL: vi.fn() }))
vi.mock('src/ts/update/settings', () => ({
    getAppUpdateSettings: () => ({ schema: 'risunest.app-update-settings/v1', autoUpdateCheck: true, skippedVersion: '', lastCheckedAt: 0 }),
    subscribeAppUpdateSettings: () => mocks.unsubscribe,
    updateAppUpdateSettings: vi.fn(),
}))

import { language } from 'src/lang'
import { appUpdateState, initialAppUpdateState } from 'src/ts/update/state.svelte'
import Component from './RisuNestUpdateSettings.svelte'

let component: ReturnType<typeof mount> | undefined
const environment: NativeUpdateEnvironment = {
    currentVersion: '9.8.7', installStrategy: 'self-install', configured: true, disabledReason: null,
}

beforeEach(() => {
    vi.clearAllMocks()
    appUpdateState.set({ ...initialAppUpdateState })
})
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
})

async function setup() {
    const target = document.createElement('div')
    document.body.append(target)
    component = mount(Component, { target })
    await tick()
    return target
}
function row(target: HTMLElement, label: string): HTMLElement {
    const labelElement = [...target.querySelectorAll('div')].find(item => item.childElementCount === 0 && item.textContent === label)
    expect(labelElement, `Missing settings row: ${label}`).toBeDefined()
    return labelElement!.parentElement!.parentElement!
}

describe('update settings environment', () => {
    it('shows loading until the native version and installation method resolve', async () => {
        let resolve!: (value: NativeUpdateEnvironment) => void
        mocks.environment.mockImplementationOnce(() => new Promise<NativeUpdateEnvironment>(next => { resolve = next }))
        const target = await setup()
        const text = language.risuNest.update
        expect(row(target, text.currentVersion).textContent).toContain(language.loading)
        expect(row(target, text.installMethod).textContent).toContain(language.loading)
        expect(row(target, text.installMethod).textContent).not.toContain(text.strategies.disabled)
        expect(row(target, text.currentVersion).textContent).not.toContain('—')
        expect(mocks.environment).toHaveBeenCalledOnce()
        resolve(environment)
        await vi.waitFor(() => expect(row(target, text.currentVersion).textContent).toContain('9.8.7'))
        expect(row(target, text.installMethod).textContent).toContain(text.strategies['self-install'])
        expect(row(target, text.installMethod).textContent).not.toContain(language.loading)
        expect(get(appUpdateState).environment).toEqual(environment)
    })

    it('shows an environment failure without misreporting a disabled install strategy', async () => {
        mocks.environment.mockRejectedValueOnce(new Error('synthetic native failure'))
        const target = await setup()
        const text = language.risuNest.update
        await vi.waitFor(() => expect(row(target, text.currentVersion).textContent).toContain(text.environmentFailed))
        expect(row(target, text.installMethod).textContent).toContain(text.environmentFailed)
        expect(row(target, text.installMethod).textContent).not.toContain(text.strategies.disabled)
        expect(target.textContent).not.toContain('synthetic native failure')
        const check = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === text.checkNow)!
        expect(check.disabled).toBe(false)
        check.click()
        expect(mocks.check).toHaveBeenCalledWith(true)
    })

    it('uses an already known environment without repeating the native read and unsubscribes on unmount', async () => {
        appUpdateState.update(state => ({ ...state, environment }))
        const target = await setup()
        expect(row(target, language.risuNest.update.currentVersion).textContent).toContain('9.8.7')
        expect(mocks.environment).not.toHaveBeenCalled()
        await unmount(component!)
        component = undefined
        expect(mocks.unsubscribe).toHaveBeenCalledOnce()
    })
})
