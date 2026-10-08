// @vitest-environment happy-dom
import { readFileSync } from 'node:fs'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync } from 'svelte'

const lang = vi.hoisted(() => ({ current: undefined as unknown }))
vi.mock('src/lang', () => ({ get language() { return lang.current } }))
vi.mock('src/ts/stores.svelte', async () => ({ alertStore: (await import('svelte/store')).writable({ type: 'none', msg: '' }) }))

import merge from 'lodash/merge'
import { languageEnglish } from 'src/lang/en'
import { languageKorean } from 'src/lang/ko'
import { QR_SCAN_ATTRIBUTE, showQrScanOverlay } from './qrScanOverlay'

const overlay = () => document.body.querySelector<HTMLElement>(':scope > .risunest-qr-scan')
const buttonText = (text: string) => [...document.querySelectorAll('button')].find(button => button.textContent?.trim() === text)
let hide: (() => void) | undefined

beforeEach(() => {
    lang.current = languageEnglish
    vi.spyOn(history, 'go').mockImplementation(() => {})
})
afterEach(() => {
    hide?.()
    hide = undefined
    vi.restoreAllMocks()
    history.replaceState(null, '')
    document.head.querySelectorAll('style[data-test]').forEach(style => style.remove())
    document.body.replaceChildren()
})

describe('QR scan overlay', () => {
    it('replaces the page with the frame, the instruction and Cancel until it is removed', () => {
        const page = document.createElement('div')
        document.body.append(page)
        hide = showQrScanOverlay(vi.fn(), 'settings')
        flushSync()
        const shown = overlay()
        expect(shown).not.toBeNull()
        expect(shown!.getAttribute('role')).toBe('dialog')
        expect(document.documentElement.hasAttribute(QR_SCAN_ATTRIBUTE)).toBe(true)
        expect(shown!.querySelector('.frame')).not.toBeNull()
        expect(document.getElementById(shown!.getAttribute('aria-labelledby')!)?.textContent?.trim()).toBe(languageEnglish.risuNest.qrScan.instruction)
        expect(buttonText(languageEnglish.cancel)).toBeDefined()

        hide()
        flushSync()
        expect(overlay()).toBeNull()
        expect(document.documentElement.hasAttribute(QR_SCAN_ATTRIBUTE)).toBe(false)
        expect(document.body.contains(page)).toBe(true)
        expect(() => hide!()).not.toThrow()
    })

    it.each([
        ['settings', 'QR 코드를 사각형 안에 맞추세요.'],
        ['onboarding', 'QR 코드를 사각형 안에 맞춰주세요.'],
    ] as const)('words the instruction for the %s screen', (tone, text) => {
        lang.current = merge({}, languageEnglish, languageKorean)
        hide = showQrScanOverlay(vi.fn(), tone)
        flushSync()
        expect(overlay()!.querySelector('p')?.textContent?.trim()).toBe(text)
        expect(buttonText('취소')).toBeDefined()
    })

    it.each([
        ['Cancel', () => buttonText(languageEnglish.cancel)!.click()],
        ['Escape', () => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))],
        ['Back', () => { history.replaceState(null, ''); window.dispatchEvent(new PopStateEvent('popstate')) }],
    ])('cancels the scan on %s', (_, act) => {
        const cancel = vi.fn()
        hide = showQrScanOverlay(cancel, 'settings')
        flushSync()
        act()
        expect(cancel).toHaveBeenCalledOnce()
    })

    it('leaves only the overlay painted over a transparent page while it is shown', () => {
        const rules = readFileSync('src/styles.css', 'utf8').match(/html\[data-risunest-qr-scan\][\s\S]*?visibility: hidden !important;\s*\}/)
        expect(rules).not.toBeNull()
        const style = document.createElement('style')
        style.dataset.test = ''
        style.textContent = `body { background-color: rgb(40, 42, 54); }\n${rules![0]}`
        document.head.append(style)
        const page = document.createElement('div')
        document.body.append(page)
        expect(getComputedStyle(page).visibility).not.toBe('hidden')

        hide = showQrScanOverlay(vi.fn(), 'settings')
        flushSync()
        expect(getComputedStyle(page).visibility).toBe('hidden')
        expect(getComputedStyle(overlay()!).visibility).not.toBe('hidden')
        expect(getComputedStyle(document.body).backgroundColor).toBe('transparent')

        hide()
        flushSync()
        expect(getComputedStyle(page).visibility).not.toBe('hidden')
        expect(getComputedStyle(document.body).backgroundColor).toBe('rgb(40, 42, 54)')
    })
})
