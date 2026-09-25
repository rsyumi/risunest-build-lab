import { afterEach, describe, expect, it, vi } from 'vitest'
import { readFileSync } from 'node:fs'
import { writable } from 'svelte/store'

const mocks = vi.hoisted(() => ({ database: { colorScheme: undefined as unknown } }))
vi.mock('../storage/database.svelte', () => ({ getDatabase: () => mocks.database, setDatabase: vi.fn() }))
vi.mock('../globalApi.svelte', () => ({ downloadFile: vi.fn() }))
vi.mock('../util', () => ({ BufferToText: vi.fn(), selectSingleFile: vi.fn() }))
vi.mock('../alert', () => ({ alertError: vi.fn() }))
vi.mock('../lite', () => ({ isLite: writable(false) }))
vi.mock('../stores.svelte', () => ({ CustomCSSStore: writable(''), DBState: { db: {} }, SafeModeStore: writable(false) }))
vi.mock('./windowsAppearance', () => ({ scheduleWindowsAppearance: vi.fn() }))

import { colorSchemePresets, defaultColorScheme, updateColorScheme, type ColorScheme } from './colorscheme'

const styles = readFileSync('src/styles.css', 'utf8')

function rootToken(name: string): string {
    const root = styles.slice(styles.indexOf(':root {'))
    const match = root.match(new RegExp(`--risu-theme-${name}:\\s*(#[0-9a-fA-F]{6});`))
    if (!match) throw new Error(`Missing --risu-theme-${name}`)
    return match[1]
}

function luminance(hex: string): number {
    const [r, g, b] = [1, 3, 5].map((index) => {
        const channel = parseInt(hex.slice(index, index + 2), 16) / 255
        return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
    })
    return 0.2126 * r + 0.7152 * g + 0.0722 * b
}

function contrast(a: string, b: string): number {
    const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x)
    return (light + 0.05) / (dark + 0.05)
}

const schemes: [string, ColorScheme][] = [
    ['default', defaultColorScheme],
    ...Object.entries(colorSchemePresets).map(([name, scheme]) => [name, scheme as ColorScheme] as [string, ColorScheme]),
    [
        'custom light',
        { ...defaultColorScheme, type: 'light', bgcolor: '#fdf6e3', darkbutton: '#eee8d5', textcolor: '#073642' },
    ],
    [
        'custom dark',
        { ...defaultColorScheme, type: 'dark', bgcolor: '#101418', darkbutton: '#2a3038', textcolor: '#e6edf3' },
    ],
]

afterEach(() => {
    document.documentElement.removeAttribute('style')
})

describe('primary foreground theme role', () => {
    it('is a Tailwind color backed by a theme variable', () => {
        expect(styles).toContain('--color-primary-foreground: var(--risu-theme-primary-foreground);')
    })

    it('contrasts with the primary backgrounds it is drawn on', () => {
        const foreground = rootToken('primary-foreground')
        expect(contrast(foreground, rootToken('primary-500'))).toBeGreaterThanOrEqual(3)
        expect(contrast(foreground, rootToken('primary-600'))).toBeGreaterThanOrEqual(3)
    })

    it.each(schemes)('comes from the shared root palette under the %s scheme', (_, scheme) => {
        mocks.database.colorScheme = scheme
        updateColorScheme()
        const style = document.documentElement.style
        expect(style.getPropertyValue('--risu-theme-darkbutton')).toBe(scheme.darkbutton)
        expect(style.getPropertyValue('--risu-theme-primary-500')).toBe('')
        expect(style.getPropertyValue('--risu-theme-primary-foreground')).toBe('')
    })

    it.each(schemes)('leaves the off switch thumb visible on the %s track', (_, scheme) => {
        expect(contrast(scheme.textcolor, scheme.darkbutton)).toBeGreaterThanOrEqual(3)
    })
})
