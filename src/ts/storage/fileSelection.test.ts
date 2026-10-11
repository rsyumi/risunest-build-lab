import { describe, expect, it, vi } from 'vitest'
const native = vi.hoisted(() => ({ open: vi.fn(), save: vi.fn() }))
vi.mock('@tauri-apps/plugin-dialog', () => native)
import { open, save } from './fileSelection'
import { isLibraryFileOperationReserved } from './libraryFileOperation'

describe('native file selection', () => {
    it.each(['open', 'save'] as const)('keeps %s admission until the native result, including cancellation', async kind => {
        let finish!: (value: null) => void
        native[kind].mockImplementation(() => new Promise<null>(resolve => { finish = resolve }))
        const selected = kind === 'open' ? open({ multiple: false }) : save({ defaultPath: 'synthetic.bin' })
        expect(isLibraryFileOperationReserved()).toBe(true)
        window.dispatchEvent(new Event('focus'))
        document.dispatchEvent(new Event('visibilitychange'))
        await Promise.resolve()
        expect(isLibraryFileOperationReserved()).toBe(true)
        finish(null)
        expect(await selected).toBeNull()
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('releases a rejected native picker', async () => {
        native.open.mockRejectedValue(new Error('picker unavailable'))
        await expect(open()).rejects.toThrow('picker unavailable')
        expect(isLibraryFileOperationReserved()).toBe(false)
    })
})
