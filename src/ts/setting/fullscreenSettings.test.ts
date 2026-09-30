import { beforeEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({ desktop: false, fullscreen: false, fullScreen: true,
    isFullscreen: vi.fn(), setFullscreen: vi.fn() }))
vi.mock('../platform', () => ({ isTauri: true, get isTauriDesktop() { return state.desktop }, isTauriMobile: false, isIOS: () => false }))
vi.mock('@tauri-apps/api/webviewWindow', () => ({ getCurrentWebviewWindow: () => state }))
vi.mock('../storage/database.svelte', () => ({ getDatabase: () => state }))
vi.mock('../characters', () => ({ createBlankChar: vi.fn(), getCharImage: vi.fn() }))
vi.mock('../stores.svelte', () => ({ DBState: { db: {} }, selectedCharID: { subscribe: vi.fn() }, CustomGUISettingMenuStore: { set: vi.fn() } }))
vi.mock('src/lib/UI/PopupList.svelte', () => ({ default: {} }))
vi.mock('../gui/animation', () => ({ updateAnimationSpeed: vi.fn() }))
vi.mock('../gui/guisize', () => ({ guiSizeText: vi.fn(), updateGuisize: vi.fn() }))
vi.mock('../gui/colorscheme', () => ({ updateTextThemeAndCSS: vi.fn() }))
import { changeFullscreen } from '../util'
import { displayOtherSettingsItems } from './displaySettingsData.svelte'

beforeEach(() => {
    vi.clearAllMocks()
    state.desktop = false
    state.fullScreen = true
    state.isFullscreen.mockResolvedValue(false)
    state.setFullscreen.mockResolvedValue(undefined)
})

describe('fullscreen platform admission', () => {
    it('hides the unsupported control and ignores a synced true value outside native desktop', async () => {
        const control = displayOtherSettingsItems.find(item => item.id === 'display.fullScreen')!
        expect(control.condition?.({} as never)).toBe(false)
        await changeFullscreen()
        expect(state.isFullscreen).not.toHaveBeenCalled()
        expect(state.setFullscreen).not.toHaveBeenCalled()
    })
    it('retains the desktop control and applies the stored value', async () => {
        state.desktop = true
        const control = displayOtherSettingsItems.find(item => item.id === 'display.fullScreen')!
        expect(control.condition?.({} as never)).toBe(true)
        await changeFullscreen()
        expect(state.setFullscreen).toHaveBeenCalledExactlyOnceWith(true)
    })
    it('does not hide a desktop window API failure from the caller', async () => {
        state.desktop = true
        state.setFullscreen.mockRejectedValueOnce(new Error('window unavailable'))
        await expect(changeFullscreen()).rejects.toThrow('window unavailable')
    })
})
