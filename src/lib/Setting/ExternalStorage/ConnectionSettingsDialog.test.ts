// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import QRCode from 'qrcode'
import ConnectionSettingsDialog from './ConnectionSettingsDialog.svelte'
import { externalStorageStrings } from './strings'

vi.mock('qrcode', async importOriginal => {
    const actual = await importOriginal<{ default: typeof import('qrcode') }>()
    return { default: { ...actual.default, toDataURL: vi.fn(async () => 'data:image/png;base64,c3ludGhldGlj') } }
})

const strings = externalStorageStrings('en')
const payload = 'synthetic-lowercase-payload-'.repeat(4).slice(0, 100)
let target: HTMLDivElement
let component: ReturnType<typeof mount>
let resize: () => void
let width: number
let height: number
const copy = vi.fn()
const save = vi.fn()
const close = vi.fn()

async function settle() {
    for (let step = 0; step < 4; step++) { await Promise.resolve(); await tick() }
}

async function open(value = payload) {
    component = mount(ConnectionSettingsDialog, { target, props: { payload: value, strings, onsave: save, onclose: close } })
    await settle()
}

function button(label: string) {
    return [...target.querySelectorAll('button')].find(item => item.textContent?.trim() === label)!
}

beforeEach(() => {
    width = 800
    height = 1000
    copy.mockResolvedValue(undefined)
    save.mockResolvedValue(undefined)
    vi.spyOn(navigator.clipboard, 'writeText').mockImplementation(copy)
    vi.stubGlobal('ResizeObserver', class {
        constructor(callback: () => void) { resize = callback }
        observe() {}
        disconnect() {}
    })
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(() => width)
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(() => height)
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.classList.contains('heading') ? 64 : this.classList.contains('controls') ? 120 : 34
    })
    vi.spyOn(window, 'getComputedStyle').mockReturnValue({
        paddingTop: '0', paddingBottom: '0', paddingLeft: '0', paddingRight: '0', rowGap: '16',
    } as CSSStyleDeclaration)
    target = document.createElement('div')
    document.body.append(target)
})

afterEach(() => {
    if (component) unmount(component)
    target.remove()
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
    vi.clearAllMocks()
})

describe('connection settings transfer', () => {
    it('fits a QR at twice the previous size, then responds to width and height changes', async () => {
        await open()
        const modules = QRCode.create(payload).modules.size + 8
        const base = Math.ceil(240 / modules) * modules
        const qr = target.querySelector('img')!
        resize()
        await tick()
        expect(qr.width).toBe(base * 2)
        expect(qr.height).toBe(base * 2)
        width = 280
        resize()
        await tick()
        expect(qr.width).toBe(Math.floor(width / modules) * modules)
        width = 800
        height = 600
        resize()
        await tick()
        expect(qr.width).toBe(Math.floor((height - 268) / modules) * modules)
        expect(qr.height).toBe(qr.width)
        height = 300
        resize()
        await tick()
        expect(qr.width).toBe(base)
    })

    it.each([100, 2400, 40000])('copies the entire %i-character transfer, with or without a QR', async length => {
        const value = payload.repeat(400).slice(0, length)
        await open(value)
        const textarea = target.querySelector('textarea')!
        expect(textarea.readOnly).toBe(true)
        expect(textarea.value).toBe(value)
        expect(!!target.querySelector('img')).toBe(length <= 2300)
        button(strings.copyConnectionSettings).click()
        await settle()
        expect(copy).toHaveBeenCalledWith(value)
        expect(target.querySelector('[role=status]')?.textContent).toBe(strings.connectionSettingsCopied)
    })

    it('keeps text and saving available when QR generation fails', async () => {
        vi.mocked(QRCode.toDataURL).mockRejectedValueOnce(new Error('canvas unavailable'))
        await open()
        expect(target.querySelector('img')).toBeNull()
        expect(target.textContent).toContain(strings.connectionSettingsFileOnly)
        expect(target.querySelector('textarea')!.value).toBe(payload)
        button(strings.saveConnectionSettings).click()
        await settle()
        expect(save).toHaveBeenCalledOnce()
    })

    it('selects the full text for manual copying after clipboard rejection', async () => {
        copy.mockRejectedValue(new Error('denied'))
        await open()
        button(strings.copyConnectionSettings).click()
        await settle()
        const textarea = target.querySelector('textarea')!
        expect(document.activeElement).toBe(textarea)
        expect(textarea.selectionStart).toBe(0)
        expect(textarea.selectionEnd).toBe(payload.length)
        expect(target.querySelector('[role=alert]')?.textContent).toBe(strings.connectionSettingsCopyFailed)
        expect(target.querySelector('[role=status]')?.textContent).toBe('')
    })

    it('permits saving again after cancellation and shows failures inside the dialog', async () => {
        save.mockRejectedValueOnce({ kind: 'cancelled' }).mockRejectedValueOnce({ kind: 'transient' })
        await open()
        button(strings.saveConnectionSettings).click()
        await settle()
        expect(target.querySelector('[role=alert]')).toBeNull()
        button(strings.saveConnectionSettings).click()
        await settle()
        expect(target.querySelector('[role=alert]')?.textContent).toBe(strings.retry)
        button(strings.saveConnectionSettings).click()
        await settle()
        expect(save).toHaveBeenCalledTimes(3)
        expect(target.querySelector('textarea')!.value).toBe(payload)
    })

    it('keeps keyboard navigation within the dialog and closes with Escape', async () => {
        await open()
        const panel = target.querySelector<HTMLElement>('[role=dialog]')!
        panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true }))
        expect(document.activeElement).toBe(button(strings.closeRecovery))
        button(strings.closeRecovery).dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }))
        expect(document.activeElement).toBe(target.querySelector('textarea'))
        panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
        expect(close).toHaveBeenCalledOnce()
    })
})
