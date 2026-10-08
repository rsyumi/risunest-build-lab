import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { DBState } from 'src/ts/stores.svelte'
import { getDeviceSettings, reloadDeviceSettings, updateDeviceSettings } from 'src/ts/storage/deviceSettings'
import CustomSidebar from './CustomSidebar.svelte'

vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/parser/parser.svelte', () => ({ risuChatParser: (text: string) => text }))
vi.mock('./ModelBind.svelte', () => ({ default: () => {} }))
vi.mock('./PersonaBind.svelte', () => ({ default: () => {} }))

let mounted: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (mounted) await unmount(mounted)
    mounted = undefined
    document.body.replaceChildren()
    localStorage.clear()
    reloadDeviceSettings()
})

function pin(...ids: string[]) {
    DBState.db.customSidebarItems = ids.map((id) => ({ id, type: 'setting', subType: id, label: id }))
}

it('skips a pinned setting that no longer exists', async () => {
    pin('acc.sendWithEnter', 'acc.fixedChatTextarea')
    mounted = mount(CustomSidebar, { target: document.body })
    await tick()
    expect(document.body.textContent).toContain(languageEnglish.fixedChatTextarea)
})

it('reads and writes the pinned message send key through the device settings', async () => {
    updateDeviceSettings({ messageSendKey: 'ctrl-shift-enter' })
    pin('risunest.ui.messageSendKey')
    const database = JSON.stringify(DBState.db)
    mounted = mount(CustomSidebar, { target: document.body })
    await tick()
    const buttons = [...document.querySelectorAll<HTMLButtonElement>('[data-segment-btn]')]
    expect(buttons.map((button) => button.textContent?.trim())).toEqual(['Enter', 'Ctrl/Shift+Enter', 'Send button'])
    expect(buttons.map((button) => button.classList.contains('segmented-btn-active'))).toEqual([false, true, false])
    buttons[2].click()
    await tick()
    expect(getDeviceSettings().messageSendKey).toBe('button')
    expect(JSON.stringify(DBState.db)).toBe(database)
    expect(Object.hasOwn(DBState.db, 'undefined')).toBe(false)
})
