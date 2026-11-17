import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { mount, unmount, tick, type ComponentProps } from 'svelte'
import { language } from 'src/lang'
import AppLockScreen from './AppLockScreen.svelte'
import AppLockCover from './AppLockCover.svelte'

let host: HTMLDivElement
let mounted: ReturnType<typeof mount> | undefined
beforeEach(() => {
    host = document.createElement('div')
    document.body.append(host)
})
afterEach(async () => {
    if (mounted) await unmount(mounted)
    mounted = undefined
    host.remove()
})

async function screen(props: ComponentProps<typeof AppLockScreen> = {}) {
    mounted = mount(AppLockScreen, { target: host, props })
    await tick()
    return host.querySelector<HTMLElement>('[role="dialog"]')!
}
function key(target: HTMLElement, value: string, options: KeyboardEventInit = {}) {
    const event = new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true, ...options })
    target.dispatchEvent(event)
    return event
}
function button(text: string): HTMLButtonElement {
    const found = [...host.querySelectorAll('button')].find(item => item.textContent?.trim() === text || item.getAttribute('aria-label') === text)
    expect(found).toBeDefined()
    return found!
}

describe('shared app PIN screen', () => {
    it('accepts physical and onscreen digits and delete without a text field', async () => {
        let entered = ''
        const root = await screen({ onDigit: digit => { entered += digit }, onDelete: () => { entered = entered.slice(0, -1) } })
        expect(document.activeElement).toBe(root)
        expect(host.querySelector('input')).toBeNull()
        key(root, '1')
        key(root, '2', { code: 'Numpad2' })
        button('3').click()
        expect(entered).toBe('123')
        key(root, 'Backspace')
        button(language.risuNest.appLock.deleteDigit).click()
        expect(entered).toBe('1')
        expect(key(root, '4', { ctrlKey: true }).defaultPrevented).toBe(true)
        expect(entered).toBe('1')
    })

    it('cycles focus and activates real buttons through its capture dispatcher callback', async () => {
        let surface: { element: HTMLElement; keydown: (event: KeyboardEvent) => void } | null = null
        let entered = ''
        await screen({ onDigit: digit => { entered += digit }, onSurface: value => { surface = value } })
        const dispatch = (value: string, shiftKey = false) => {
            const event = new KeyboardEvent('keydown', { key: value, shiftKey, cancelable: true })
            surface!.keydown(event)
            expect(event.defaultPrevented).toBe(true)
        }
        dispatch('Tab')
        expect(document.activeElement).toBe(button('1'))
        dispatch('Enter')
        dispatch(' ')
        expect(entered).toBe('11')
        dispatch('Tab', true)
        expect(document.activeElement).toBe(button(language.risuNest.appLock.deleteDigit))
        dispatch('Escape')
        expect(host.querySelector('[role="dialog"]')).not.toBeNull()
        await unmount(mounted!)
        mounted = undefined
        expect(surface).toBeNull()
    })

    it.each(['windowsHello', 'faceId', 'touchId', 'device'] as const)('offers one native action for %s', async method => {
        const labels = { windowsHello: 'openWithWindowsHello', faceId: 'openWithFaceId', touchId: 'openWithTouchId', device: 'openWithDevice' } as const
        let requested = 0
        await screen({ nativeAction: 'unlock', nativeMethod: method, onNative: () => { requested++ } })
        button(language.risuNest.appLock[labels[method]]).click()
        expect(requested).toBe(1)
        expect(host.textContent).not.toContain(language.risuNest.appLock.resetPin)
    })

    it('offers reset as the exclusive alternative to native unlock', async () => {
        let resets = 0
        await screen({ nativeAction: 'reset', onReset: () => { resets++ } })
        button(language.risuNest.appLock.resetPin).click()
        expect(resets).toBe(1)
        expect(host.textContent).not.toContain(language.risuNest.appLock.openWithDevice)
    })

    it('keeps PIN progress while save failure disables entry and permits retry or cancel', async () => {
        let entered = ''
        let retried = false
        let cancelled = false
        await screen({ mode: 'setup', phase: 'confirm', count: 6, saveFailed: true,
            onDigit: digit => { entered += digit }, onRetry: () => { retried = true }, onCancel: () => { cancelled = true } })
        expect(host.querySelector('[role="img"]')?.getAttribute('aria-label')).toBe(language.risuNest.appLock.progress.replace('{0}', '6'))
        expect(host.textContent).toContain(language.risuNest.appLock.confirmPin)
        expect(host.textContent).toContain(language.risuNest.localData.applyFailed)
        expect(button('1').disabled).toBe(true)
        button('1').click()
        key(host.querySelector('[role="dialog"]')!, '2')
        expect(entered).toBe('')
        button(language.retry).click()
        button(language.cancel).click()
        expect(retried && cancelled).toBe(true)
    })

    it.each(['setup', 'change', 'reset'] as const)('uses the %s title with shared PIN entry', async mode => {
        await screen({ mode })
        const title = mode === 'setup' ? language.risuNest.appLock.setupTitle : mode === 'change' ? language.risuNest.appLock.changeTitle : language.risuNest.appLock.resetPin
        expect(host.querySelector('[role="dialog"]')?.getAttribute('aria-label')).toBe(title)
        expect(host.textContent).toContain(language.risuNest.appLock.newPin)
        expect(button(language.cancel)).toBeDefined()
    })

    it('shows only retry and supported recovery on an unreadable record cover', async () => {
        let retried = false
        mounted = mount(AppLockCover, { target: host, props: { readFailed: true, canReset: true, onRetry: () => { retried = true } } })
        await tick()
        expect(host.textContent).toContain(language.risuNest.localData.loadFailed)
        expect(host.querySelectorAll('button')).toHaveLength(2)
        button(language.retry).click()
        expect(retried).toBe(true)
        expect(host.querySelector('.keypad')).toBeNull()
    })
})