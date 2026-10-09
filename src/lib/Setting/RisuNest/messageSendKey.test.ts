import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { languageKorean } from 'src/lang/ko'
import { DBState } from 'src/ts/stores.svelte'
import { risuNestUiSettingsItems } from 'src/ts/setting/risuNestSettingsData'
import { accessibilitySettingsItems } from 'src/ts/setting/accessibilitySettingsData'
import { getDeviceSettings, reloadDeviceSettings, updateDeviceSettings } from 'src/ts/storage/deviceSettings'
import { sharedRootFields } from 'src/ts/storage/persistentRootFields'
import RisuNestSettingRows from './RisuNestSettingRows.svelte'

vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/parser/parser.svelte', () => ({ risuChatParser: (text: string) => text }))

let mounted: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (mounted) await unmount(mounted)
    mounted = undefined
    document.body.replaceChildren()
    localStorage.clear()
    reloadDeviceSettings()
    vi.restoreAllMocks()
})

it('renders and persists all three local choices in UI without changing shared settings', async () => {
    updateDeviceSettings({ messageSendKey: 'enter' })
    DBState.db.sendWithEnter = false
    DBState.db.risunestChatEditPopup = false
    const database = JSON.stringify(DBState.db)
    mounted = mount(RisuNestSettingRows, { target: document.body, props: { items: risuNestUiSettingsItems } })
    await tick()
    const group = document.querySelector('[role="group"][aria-label="Message send key"]')!
    const buttons = [...group.querySelectorAll<HTMLButtonElement>('button')]
    expect(buttons.map(button => button.textContent)).toEqual(['Enter', 'Ctrl/Shift+Enter', 'Send button'])
    expect(document.body.textContent).toContain(languageEnglish.risuNest.ui.messageSendKeyHelp)
    expect(buttons.map(button => button.getAttribute('aria-pressed'))).toEqual(['true', 'false', 'false'])
    for (const [index, mode] of ['enter', 'ctrl-shift-enter', 'button'].entries()) {
        buttons[index].click()
        await tick()
        expect(getDeviceSettings().messageSendKey).toBe(mode)
        expect(buttons[index].getAttribute('aria-pressed')).toBe('true')
    }
    expect(JSON.stringify(DBState.db)).toBe(database)
    expect(sharedRootFields.has('messageSendKey')).toBe(false)
    expect(sharedRootFields.has('risuNestDeviceSettings')).toBe(false)
    expect(accessibilitySettingsItems.some(item => item.bindKey === 'sendWithEnter')).toBe(false)
})

it('describes each send key choice', () => {
    expect(languageKorean.risuNest.ui).toMatchObject({
        messageSendKey: '메시지 보내기 키',
        messageSendKeyHelp: '채팅창에서 메시지를 보내는 단축키를 결정합니다. Enter를 선택하면 Shift+Enter로 줄바꿈이 가능하고, Enter가 아닌 다른 방식은 Enter로 줄바꿈을 할 수 있게 됩니다.',
        messageSendKeyButton: '전송 버튼',
    })
    expect(languageEnglish.risuNest.ui).toMatchObject({
        messageSendKey: 'Message send key',
        messageSendKeyHelp: 'With Enter, Enter sends a message and Shift+Enter inserts a newline. With Ctrl/Shift+Enter, Ctrl+Enter or Shift+Enter sends a message and Enter inserts a newline. With Send button, Enter inserts a newline and only the send button sends a message.',
        messageSendKeyButton: 'Send button',
    })
})

it('reflects restored device settings without rewriting them', async () => {
    updateDeviceSettings({ messageSendKey: 'enter' })
    mounted = mount(RisuNestSettingRows, { target: document.body, props: { items: risuNestUiSettingsItems } })
    await tick()
    localStorage.setItem('risuNestDeviceSettings', JSON.stringify({ ...getDeviceSettings(), messageSendKey: 'button' }))
    const write = vi.spyOn(localStorage, 'setItem')
    reloadDeviceSettings()
    await tick()
    expect(document.querySelector('[aria-label="Message send key"] button[aria-pressed="true"]')?.textContent).toBe('Send button')
    expect(write).not.toHaveBeenCalled()
    await unmount(mounted!)
    mounted = undefined
    updateDeviceSettings({ messageSendKey: 'ctrl-shift-enter' })
    await tick()
    expect(getDeviceSettings().messageSendKey).toBe('ctrl-shift-enter')
})
