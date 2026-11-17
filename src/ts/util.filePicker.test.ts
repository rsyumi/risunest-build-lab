import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PlatformShape } from './platform.testSupport'
import { mount, unmount, tick } from 'svelte'
import CustomBackgroundToggle from '../lib/Setting/Pages/Display/CustomBackgroundToggle.svelte'

const alertError = vi.hoisted(() => vi.fn())
vi.mock('./alert', () => ({ alertError }))
const settings = vi.hoisted(() => ({ allowAllExtentionFiles: false, customBackground: '' }))
const saveImage = vi.hoisted(() => vi.fn(async () => 'assets/selected.png'))
const native = vi.hoisted(() => ({
    platform: { runtime: 'web', os: 'windows' } as PlatformShape,
    invoke: vi.fn(),
    open: vi.fn(),
    dialog: vi.fn(),
}))
vi.mock('@tauri-apps/api/core', async importOriginal => ({
    ...await importOriginal<typeof import('@tauri-apps/api/core')>(), invoke: native.invoke,
}))
vi.mock('@tauri-apps/plugin-fs', async () => (await import('./storage/tauriFs.testSupport')).pluginFsModule({ open: native.open }))
vi.mock('@tauri-apps/plugin-dialog', async importOriginal => ({ ...await importOriginal<typeof import('@tauri-apps/plugin-dialog')>(), open: native.dialog }))
vi.mock('./storage/database.svelte', () => ({ getDatabase: () => settings, saveImage }))
vi.mock('./characters', () => ({ createBlankChar: vi.fn(), getCharImage: vi.fn() }))
vi.mock('./stores.svelte', () => ({ DBState: { db: settings }, selectedCharID: { subscribe: vi.fn() } }))
vi.mock('./platform', async () => (await import('./platform.testSupport')).platformModule(() => native.platform))
vi.mock('src/lib/UI/PopupList.svelte', () => ({ default: {} }))

import { language } from 'src/lang'
import { selectFileByDom, selectSingleFile, selectMultipleFile, mapSelectedFiles } from './util'
import { isLibraryFileOperationReserved } from './storage/libraryFileOperation'
import { selectDomFiles } from './storage/fileSelection'
import { readableFile } from './storage/tauriFs.testSupport'
import { imageAliases } from './media/imageFormats'
import { imageFixture } from '../../tests/support/imageFixtures'

const bytes = new Uint8Array([0, 128, 255])
const picked = (name: string) => ({ path: `/owned/${name}/source`, name, bytes: bytes.length, mimeType: '' })

function input(): HTMLInputElement {
    const element = document.querySelector<HTMLInputElement>('input[type="file"]')
    if (!element) throw new Error('Picker input was not mounted')
    return element
}

beforeEach(() => {
    vi.spyOn(HTMLInputElement.prototype, 'click').mockImplementation(() => {})
    alertError.mockReset()
    settings.allowAllExtentionFiles = false
    settings.customBackground = ''
    saveImage.mockClear()
    native.dialog.mockReset().mockResolvedValue([])
    native.platform = { runtime: 'web', os: 'windows' }
    native.invoke.mockReset().mockResolvedValue(undefined)
    native.open.mockReset().mockImplementation(async () => readableFile(bytes))
})

describe('mobile file wrappers', () => {
    it('reports unsupported image metadata without reading or rejecting the selection promise', async () => {
        native.platform = { runtime: 'tauri', os: 'android' }
        window.RisuNativeFileSelection = {
            pick: async () => ({ cancelled: false, files: [{ ...picked('document.pdf'), mimeType: 'application/pdf' }] }),
            discard: async () => {},
        }
        try {
            expect(await selectSingleFile(imageAliases, { image: true })).toBeNull()
            expect(alertError).toHaveBeenCalledExactlyOnceWith(language.risuNest.imageFormats.unsupported)
            expect(native.open).not.toHaveBeenCalled()
        } finally { delete window.RisuNativeFileSelection }
    })

    it('still propagates unexpected image read errors', async () => {
        native.platform = { runtime: 'tauri', os: 'android' }
        window.RisuNativeFileSelection = {
            pick: async () => ({ cancelled: false, files: [picked('image.png')] }),
            discard: async () => {},
        }
        native.open.mockRejectedValueOnce(new Error('read failed'))
        try {
            await expect(selectSingleFile(imageAliases, { image: true })).rejects.toThrow('read failed')
            expect(alertError).not.toHaveBeenCalled()
        } finally { delete window.RisuNativeFileSelection }
    })

    it.each(['image/avif', 'application/octet-stream', ''])('accepts an extensionless Android image with MIME %j and validates its bytes', async mimeType => {
        native.platform = { runtime: 'tauri', os: 'android' }
        const data = imageFixture('avif')
        window.RisuNativeFileSelection = {
            pick: async () => ({ cancelled: false, files: [{ ...picked('provider-item'), bytes: data.length, mimeType }] }),
            discard: async () => {},
        }
        native.open.mockImplementation(async () => readableFile(data))
        try {
            expect((await selectSingleFile(imageAliases, { image: true }))?.data).toEqual(data)
            native.open.mockImplementation(async () => readableFile(new Uint8Array(data.length)))
            await expect(selectSingleFile(imageAliases, { image: true })).resolves.toBeNull()
            expect(alertError).toHaveBeenLastCalledWith(language.risuNest.imageFormats.unsupported)
        } finally { delete window.RisuNativeFileSelection }
    })

    it('keeps valid Android images and reports rejected metadata and headers together', async () => {
        native.platform = { runtime: 'tauri', os: 'android' }
        const data = imageFixture('webp')
        const good = { ...picked('valid.WEBP'), bytes: data.length }
        window.RisuNativeFileSelection = {
            pick: async () => ({ cancelled: false, files: [good, picked('broken.PNG'), { ...picked('file.pdf'), mimeType: 'application/pdf' }] }),
            discard: async () => {},
        }
        native.open.mockImplementation(async path => readableFile(path === good.path ? data : bytes))
        try {
            const failures: string[] = []
            const files = await selectMultipleFile(imageAliases, { image: true, failures })
            expect(files).toEqual([{ name: good.name, data, mimeType: '' }])
            expect(alertError).toHaveBeenCalledExactlyOnceWith([
                `file.pdf: ${language.risuNest.imageFormats.unsupported}`,
                `broken.PNG: ${language.risuNest.imageFormats.unsupported}`,
            ].join('\n'))
            alertError.mockClear()
            await mapSelectedFiles(files!, async file => file, failures)
            expect(alertError).not.toHaveBeenCalled()
            await mapSelectedFiles(files!, async () => { throw new Error('cannot store') }, failures)
            expect(alertError).toHaveBeenCalledExactlyOnceWith('valid.WEBP: cannot store')
        } finally { delete window.RisuNativeFileSelection }
    })

    it('returns the picked bytes for single and multiple selection without a WebView file read', async () => {
        native.platform = { runtime: 'tauri', os: 'ios' }
        native.invoke.mockImplementation(async (command: string) => command === 'plugin:ios-native|pick_files'
            ? { cancelled: false, files: [picked('first.risup'), picked('second.json')] }
            : undefined)
        const read = vi.spyOn(FileReader.prototype, 'readAsArrayBuffer')
        expect(await selectSingleFile(['json', 'risup'])).toEqual({ name: 'first.risup', data: bytes, mimeType: '' })
        expect(await selectMultipleFile(['json', 'risup'])).toEqual([
            { name: 'first.risup', data: bytes, mimeType: '' },
            { name: 'second.json', data: bytes, mimeType: '' },
        ])
        expect(HTMLInputElement.prototype.click).not.toHaveBeenCalled()
        expect(read).not.toHaveBeenCalled()
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('returns null from the single-file wrapper when the picker is cancelled', async () => {
        native.platform = { runtime: 'tauri', os: 'ios' }
        native.invoke.mockResolvedValue({ cancelled: true })
        expect(await selectSingleFile(['risup'])).toBeNull()
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('refuses a file outside the listed extensions before reading it', async () => {
        native.platform = { runtime: 'tauri', os: 'android' }
        const discard = vi.fn(async () => {})
        window.RisuNativeFileSelection = {
            pick: vi.fn(async () => ({ cancelled: false, files: [picked('card.png')] })),
            discard,
        }
        try {
            await expect(selectSingleFile(['json', 'risup'])).rejects.toThrow(language.risuNest.importDialog.reasonUnsupportedFormat)
        } finally {
            delete window.RisuNativeFileSelection
        }
        expect(native.open).not.toHaveBeenCalled()
        expect(discard).toHaveBeenCalledExactlyOnceWith('/owned/card.png/source')
    })
})

describe('desktop multiple-file filters', () => {
    it.each(['windows', 'macos', 'linux'] as const)('omits native filters for allow-all on %s', async os => {
        native.platform = { runtime: 'tauri', os }
        settings.allowAllExtentionFiles = true
        await selectMultipleFile(imageAliases, { image: true })
        expect(native.dialog).toHaveBeenLastCalledWith({ multiple: true, filters: undefined })
        settings.allowAllExtentionFiles = false
        await selectMultipleFile(imageAliases, { image: true })
        expect(native.dialog).toHaveBeenLastCalledWith({ multiple: true, filters: [{ name: imageAliases.join(', '), extensions: imageAliases }] })
    })
})

describe('custom background selection', () => {
    it.each(['unsupported', 'damaged', 'cancel', 'valid'] as const)('keeps temporary %s selection state out of stored settings', async outcome => {
        const component = mount(CustomBackgroundToggle, { target: document.body })
        try {
            const check = document.querySelector<HTMLInputElement>('input[type="checkbox"]')!
            check.checked = true
            check.dispatchEvent(new Event('change', { bubbles: true }))
            await tick()
            expect(settings.customBackground).toBe('')
            const picker = input()
            if (outcome === 'cancel') picker.dispatchEvent(new Event('cancel'))
            else {
                const data = outcome === 'valid' ? imageFixture('png') : outcome === 'damaged' ? imageFixture('png').slice(0, 8) : bytes
                Object.defineProperty(picker, 'files', { value: [new File([data], 'background.png')] })
                picker.dispatchEvent(new Event('change'))
            }
            await vi.waitFor(() => expect(document.querySelector('input[type="file"]')).toBeNull())
            await vi.waitFor(() => expect(settings.customBackground).toBe(outcome === 'valid' ? 'assets/selected.png' : ''))
            await vi.waitFor(() => expect(check.checked).toBe(outcome === 'valid'))
            expect(saveImage).toHaveBeenCalledTimes(outcome === 'valid' ? 1 : 0)
            if (outcome === 'unsupported' || outcome === 'damaged') expect(alertError).toHaveBeenCalledExactlyOnceWith(language.risuNest.imageFormats[outcome])
        } finally { await unmount(component) }
    })
})
afterEach(() => {
    vi.restoreAllMocks()
    document.body.replaceChildren()
})

describe('DOM file picker', () => {
    it.each([false, true])('validates image content with allow-all %s', async allowAll => {
        settings.allowAllExtentionFiles = allowAll
        const data = imageFixture('jpg')
        const pending = selectSingleFile(imageAliases, { image: true })
        const element = input()
        expect(element.accept).toBe(allowAll ? '*' : imageAliases.map(ext => `.${ext}`).join(','))
        Object.defineProperty(element, 'files', { value: [new File([data], 'renamed.PNG')] })
        element.dispatchEvent(new Event('change'))
        expect((await pending)?.data).toEqual(data)
    })

    it('keeps admission through focus and visibility events until the actual picker result', async () => {
        const pending = selectFileByDom(['png'])
        expect(isLibraryFileOperationReserved()).toBe(true)
        window.dispatchEvent(new Event('blur'))
        document.dispatchEvent(new Event('visibilitychange'))
        window.dispatchEvent(new Event('focus'))
        await Promise.resolve()
        expect(isLibraryFileOperationReserved()).toBe(true)
        input().dispatchEvent(new Event('cancel'))
        await pending
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('settles abort without leaving admission or a late picker callback behind', async () => {
        const controller = new AbortController()
        const pending = selectDomFiles({ signal: controller.signal })
        const element = input()
        controller.abort()
        expect(await pending).toEqual([])
        expect(isLibraryFileOperationReserved()).toBe(false)
        element.dispatchEvent(new Event('change'))
        expect(document.querySelector('input')).toBeNull()
    })
    it('settles cancellation and removes the input and its event listeners', async () => {
        const pending = selectFileByDom(['png'])
        const element = input()
        const removed = vi.spyOn(element, 'removeEventListener')
        element.dispatchEvent(new Event('cancel'))
        expect(await pending).toEqual([])
        expect(element.isConnected).toBe(false)
        expect(removed).toHaveBeenCalledWith('change', expect.any(Function))
        expect(removed).toHaveBeenCalledWith('cancel', expect.any(Function))
    })

    it('settles an empty change event without retaining an input', async () => {
        const pending = selectFileByDom(['png'])
        input().dispatchEvent(new Event('change'))
        expect(await pending).toEqual([])
        expect(document.querySelector('input')).toBeNull()
    })

    it('returns the selected files, retaining extension filtering and multiple selection', async () => {
        const pending = selectFileByDom(['png'], 'multiple')
        const element = input()
        const accepted = new File(['synthetic'], 'CARD.PNG')
        Object.defineProperty(element, 'files', { value: [accepted, new File(['other'], 'other.txt')] })
        expect(element.multiple).toBe(true)
        expect(element.accept).toBe('.png')
        element.dispatchEvent(new Event('change'))
        expect(await pending).toEqual([accepted])
        expect(element.isConnected).toBe(false)
    })

    it.each(['setting', 'ios', 'wildcard'] as const)('keeps the %s accept-all path', async (mode) => {
        settings.allowAllExtentionFiles = mode === 'setting'
        if (mode === 'ios') native.platform = { runtime: 'web', os: 'ios' }
        const pending = selectFileByDom(mode === 'wildcard' ? ['*'] : ['png'])
        const element = input()
        const file = new File(['synthetic'], 'file.without-known-extension')
        Object.defineProperty(element, 'files', { value: [file] })
        element.dispatchEvent(new Event('change'))
        expect(await pending).toEqual([file])
    })

    it('returns null from the single-file wrapper after cancellation', async () => {
        const pending = selectSingleFile(['png'])
        input().dispatchEvent(new Event('cancel'))
        expect(await pending).toBeNull()
    })

    it('rejects an opening failure without leaking the input', async () => {
        vi.mocked(HTMLInputElement.prototype.click).mockImplementation(() => { throw new Error('opening failed') })
        await expect(selectFileByDom(['png'])).rejects.toThrow('opening failed')
        expect(document.querySelector('input')).toBeNull()
    })
})
