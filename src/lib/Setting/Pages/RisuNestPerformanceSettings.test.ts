// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const deviceSettings = vi.hoisted(() => ({
    getDeviceSettings: vi.fn(() => ({
        performanceProfile: 'normal' as const,
        generationHistoryLimitEnabled: false,
        generationHistoryLimitMultiplier: 2,
    })),
    subscribeDeviceSettings: vi.fn(() => () => undefined),
    updateDeviceSettings: vi.fn(),
}))

vi.mock('src/ts/storage/deviceSettings', () => deviceSettings)

import { language } from 'src/lang'
import RisuNestPerformanceSettings from './RisuNestPerformanceSettings.svelte'

const strings = language.risuNest.perf

describe('RisuNestPerformanceSettings', () => {
    let mounted: ReturnType<typeof mount> | undefined

    afterEach(async () => {
        if (mounted) await unmount(mounted)
        mounted = undefined
        document.body.replaceChildren()
        vi.clearAllMocks()
    })

    it('exposes the selected profile as a pressed button and updates it on activation', async () => {
        const target = document.createElement('div')
        document.body.append(target)
        mounted = mount(RisuNestPerformanceSettings, { target })
        await tick()

        const group = target.querySelector(`[role="group"][aria-label="${strings.profile}"]`)
        const standard = [...target.querySelectorAll<HTMLButtonElement>('button')].find((button) => button.textContent === strings.profileNormal)
        const lowSpec = [...target.querySelectorAll<HTMLButtonElement>('button')].find((button) => button.textContent === strings.profileLowSpec)
        expect(group).not.toBeNull()
        expect(standard?.getAttribute('aria-pressed')).toBe('true')
        expect(lowSpec?.getAttribute('aria-pressed')).toBe('false')
        expect(deviceSettings.updateDeviceSettings).not.toHaveBeenCalled()

        lowSpec?.click()
        await tick()

        expect(standard?.getAttribute('aria-pressed')).toBe('false')
        expect(lowSpec?.getAttribute('aria-pressed')).toBe('true')
        expect(deviceSettings.updateDeviceSettings).toHaveBeenCalledOnce()
        expect(deviceSettings.updateDeviceSettings).toHaveBeenCalledWith({ performanceProfile: 'low-spec' })
    })

    it('shows the loading limit only while the history limit is on and stores the toggle', async () => {
        const target = document.createElement('div')
        document.body.append(target)
        mounted = mount(RisuNestPerformanceSettings, { target })
        await tick()

        const toggle = target.querySelector<HTMLInputElement>(`input[type="checkbox"][aria-label="${strings.historyLimit}"]`)
            ?? [...target.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')].find((input) =>
                input.closest('label')?.textContent?.includes(strings.historyLimit))
        expect(toggle).toBeTruthy()
        expect(target.querySelector('input[type="number"]')).toBeNull()

        toggle!.click()
        await tick()

        expect(deviceSettings.updateDeviceSettings).toHaveBeenCalledWith({ generationHistoryLimitEnabled: true })
        const number = target.querySelector<HTMLInputElement>(`input[type="number"][aria-label="${strings.historyLimitMultiplier}"]`)
        expect(number?.value).toBe('2')

        toggle!.click()
        await tick()

        expect(deviceSettings.updateDeviceSettings).toHaveBeenLastCalledWith({ generationHistoryLimitEnabled: false })
        expect(target.querySelector('input[type="number"]')).toBeNull()
    })

    it('stores a loading limit below one as one and restores the stored value for invalid input', async () => {
        deviceSettings.getDeviceSettings.mockReturnValue({
            performanceProfile: 'normal',
            generationHistoryLimitEnabled: true,
            generationHistoryLimitMultiplier: 2,
        })
        const target = document.createElement('div')
        document.body.append(target)
        mounted = mount(RisuNestPerformanceSettings, { target })
        await tick()

        const number = target.querySelector<HTMLInputElement>(`input[type="number"][aria-label="${strings.historyLimitMultiplier}"]`)!
        number.value = '0.5'
        number.dispatchEvent(new Event('input', { bubbles: true }))
        number.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()

        expect(deviceSettings.updateDeviceSettings).toHaveBeenCalledWith({ generationHistoryLimitMultiplier: 1 })
        expect(number.value).toBe('1')

        number.value = '3.5'
        number.dispatchEvent(new Event('input', { bubbles: true }))
        number.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()
        expect(deviceSettings.updateDeviceSettings).toHaveBeenLastCalledWith({ generationHistoryLimitMultiplier: 3.5 })

        vi.mocked(deviceSettings.updateDeviceSettings).mockClear()
        number.value = ''
        number.dispatchEvent(new Event('input', { bubbles: true }))
        number.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()
        expect(deviceSettings.updateDeviceSettings).not.toHaveBeenCalled()
        expect(number.value).toBe('3.5')
    })
})
