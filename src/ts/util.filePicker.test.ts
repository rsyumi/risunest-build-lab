import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { PlatformShape } from './platform.testSupport'

const settings = vi.hoisted(() => ({ allowAllExtentionFiles: false }))
const native = vi.hoisted(() => ({
    platform: { runtime: 'web', os: 'windows' } as PlatformShape,
    invoke: vi.fn(),
    open: vi.fn(),
}))
vi.mock('@tauri-apps/api/core', async importOriginal => ({
    ...await importOriginal<typeof import('@tauri-apps/api/core')>(), invoke: native.invoke,
}))
vi.mock('@tauri-apps/plugin-fs', async () => (await import('./storage/tauriFs.testSupport')).pluginFsModule({ open: native.open }))
vi.mock('./storage/database.svelte', () => ({ getDatabase: () => settings }))
vi.mock('./characters', () => ({ createBlankChar: vi.fn(), getCharImage: vi.fn() }))
vi.mock('./stores.svelte', () => ({ DBState: { db: {} }, selectedCharID: { subscribe: vi.fn() } }))
vi.mock('./platform', async () => (await import('./platform.testSupport')).platformModule(() => native.platform))
vi.mock('src/lib/UI/PopupList.svelte', () => ({ default: {} }))

import { language } from 'src/lang'
import { selectFileByDom, selectSingleFile, selectMultipleFile } from './util'
import { isLibraryFileOperationReserved } from './storage/libraryFileOperation'
import { selectDomFiles } from './storage/fileSelection'
import { readableFile } from './storage/tauriFs.testSupport'

const bytes = new Uint8Array([0, 128, 255])
const picked = (name: string) => ({ path: `/owned/${name}/source`, name, bytes: bytes.length, mimeType: '' })

function input(): HTMLInputElement {
    const element = document.querySelector<HTMLInputElement>('input[type="file"]')
    if (!element) throw new Error('Picker input was not mounted')
    return element
}

beforeEach(() => {
    vi.spyOn(HTMLInputElement.prototype, 'click').mockImplementation(() => {})
    settings.allowAllExtentionFiles = false
    native.platform = { runtime: 'web', os: 'windows' }
    native.invoke.mockReset().mockResolvedValue(undefined)
    native.open.mockReset().mockImplementation(async () => readableFile(bytes))
})

describe('mobile file wrappers', () => {
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
afterEach(() => {
    vi.restoreAllMocks()
    document.body.replaceChildren()
})

describe('DOM file picker', () => {
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
