import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    updateColorScheme: vi.fn(),
    updateTextThemeAndCSS: vi.fn(),
    exclusions: [] as string[],
}))

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
