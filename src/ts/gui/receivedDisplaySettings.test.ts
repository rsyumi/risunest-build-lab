import { get } from 'svelte/store'
import { textAreaSize, sideBarSize, textAreaTextSize } from './guisize'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    updateColorScheme: vi.fn(),
    updateTextThemeAndCSS: vi.fn(),
    exclusions: [] as string[],
    database: { animationSpeed: 0.25, heightMode: 'dvh', sideBarSize: 2, textAreaSize: 3, textAreaTextSize: 4 },
}))

vi.mock('../storage/database.svelte', () => ({ getDatabase: () => mocks.database }))

vi.mock('./colorscheme', () => ({
    updateColorScheme: mocks.updateColorScheme,
    updateTextThemeAndCSS: mocks.updateTextThemeAndCSS,
}))
vi.mock('../storage/deviceSettings', () => ({
    getStartupExclusions: () => mocks.exclusions,
}))
vi.mock('../storage/recoveryMode.svelte', () => ({
    isStartupExcluded: (name: string, persisted: readonly string[]) => persisted.includes(name),
}))

import { applyReceivedDisplaySettings } from './receivedDisplaySettings'

describe('received display settings', () => {
    beforeEach(() => {
        mocks.updateColorScheme.mockClear()
        mocks.updateTextThemeAndCSS.mockClear()
        mocks.exclusions = []
        document.documentElement.removeAttribute('style')
        textAreaSize.set(0)
        sideBarSize.set(0)
        textAreaTextSize.set(0)
    })

    it('applies animation, height and all GUI sizes from the installed database while the theme is excluded', async () => {
        mocks.exclusions = ['theme']
        await applyReceivedDisplaySettings(new Set(['animationSpeed', 'heightMode', 'sideBarSize', 'textAreaSize', 'textAreaTextSize']))

        expect(document.documentElement.style.getPropertyValue('--risu-animation-speed')).toBe('0.25s')
        expect(document.documentElement.style.getPropertyValue('--risu-height-size')).toBe('100dvh')
        expect(document.documentElement.style.getPropertyValue('--sidebar-size')).toBe('32rem')
        expect(get(sideBarSize)).toBe(2)
        expect(get(textAreaSize)).toBe(3)
        expect(get(textAreaTextSize)).toBe(4)
        expect(mocks.updateColorScheme).not.toHaveBeenCalled()
        expect(mocks.updateTextThemeAndCSS).not.toHaveBeenCalled()
    })

    it.each(['sideBarSize', 'textAreaSize', 'textAreaTextSize'])('updates GUI sizes when only %s changes', async (field) => {
        await applyReceivedDisplaySettings(new Set([field]))
        expect(get(sideBarSize)).toBe(2)
        expect(get(textAreaSize)).toBe(3)
        expect(get(textAreaTextSize)).toBe(4)
        expect(document.documentElement.style.getPropertyValue('--risu-animation-speed')).toBe('')
        expect(document.documentElement.style.getPropertyValue('--risu-height-size')).toBe('')
    })

    it('updates the colors and the text theme that follows them when a color scheme setting changes', async () => {
        await applyReceivedDisplaySettings(new Set(['colorSchemeName']))

        expect(mocks.updateColorScheme).toHaveBeenCalledOnce()
        expect(mocks.updateTextThemeAndCSS).toHaveBeenCalledOnce()
    })

    it('updates only the text theme and CSS when one of them changes', async () => {
        await applyReceivedDisplaySettings(new Set(['customCSS']))

        expect(mocks.updateColorScheme).not.toHaveBeenCalled()
        expect(mocks.updateTextThemeAndCSS).toHaveBeenCalledOnce()
    })

    it('updates nothing when no display setting changed', async () => {
        await applyReceivedDisplaySettings(new Set(['openAIKey', 'loreBookDepth']))

        expect(mocks.updateColorScheme).not.toHaveBeenCalled()
        expect(mocks.updateTextThemeAndCSS).not.toHaveBeenCalled()
    })

    it('keeps the text theme and CSS off while startup excludes the theme', async () => {
        mocks.exclusions = ['theme']

        await applyReceivedDisplaySettings(new Set(['colorScheme', 'textTheme', 'customCSS']))

        expect(mocks.updateColorScheme).toHaveBeenCalledOnce()
        expect(mocks.updateTextThemeAndCSS).not.toHaveBeenCalled()
    })
})
