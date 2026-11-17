import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { DBState, selectedCharID } from 'src/ts/stores.svelte'
import { normalizeDatabaseDefaults, type Database, type character } from 'src/ts/storage/database.svelte'
import { getDeviceSettings, reloadDeviceSettings, updateDeviceSettings } from 'src/ts/storage/deviceSettings'
import CustomSidebar from './CustomSidebar.svelte'
import Toggles from './Toggles.svelte'
import CustomSidebarConfig from '../Others/CustomSidebarConfig.svelte'

vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule())
vi.mock('./ModelBind.svelte', () => ({ default: () => {} }))
vi.mock('./PersonaBind.svelte', () => ({ default: () => {} }))

let mounted: ReturnType<typeof mount> | undefined
let configMounted: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (configMounted) await unmount(configMounted)
    configMounted = undefined
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

it.each([
    { noContainer: true, count: 1 },
    { noContainer: true, count: 5 },
    { noContainer: false, count: 5 },
])('keeps custom items above toggle binding and toggle inputs below it (%j)', async ({ noContainer, count }) => {
    const database = normalizeDatabaseDefaults({} as Database)
    database.characters = [{ chaId: 'sidebar-character', chatPage: 0, chats: [{ id: 'sidebar-chat' }] } as character]
    database.customPromptTemplateToggle = Array.from({ length: count }, (_, index) => `example${index}=Example ${index}`).join('\n')
    database.hypaV3 = true
    DBState.db = database
    selectedCharID.set(0)
    const sidebar = document.createElement('div')
    const config = document.createElement('div')
    document.body.append(sidebar, config)
    mounted = mount(Toggles, { target: sidebar, props: { noContainer, chara: DBState.db.characters[0] as character } })
    configMounted = mount(CustomSidebarConfig, { target: config })
    await tick()

    const clickConfig = async (label: string) => {
        const button = [...config.querySelectorAll('button')].find(button => button.textContent?.trim() === label)
        expect(button).toBeDefined()
        button!.click()
        await tick()
    }
    const expectBindingOrder = () => {
        const bind = sidebar.querySelector<HTMLButtonElement>(`button[title="${languageEnglish.bindToggles}"]`)!
        expect(bind).not.toBeNull()
        const binding = bind.parentElement!.parentElement!
        const toggleInputs = [...sidebar.querySelectorAll('input[role="switch"]')]
        expect(toggleInputs.length).toBeGreaterThanOrEqual(count)
        for (const input of toggleInputs) {
            expect(binding.compareDocumentPosition(input) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0)
        }
        const customButtons = [...sidebar.querySelectorAll('button')].filter(button => !binding.contains(button))
        for (const button of customButtons) {
            expect(button.compareDocumentPosition(binding) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0)
        }
    }

    await clickConfig('Add Item')
    await clickConfig(languageEnglish.presets)
    expect(sidebar.textContent).toContain(database.botPresets[0].name)
    expectBindingOrder()
    await clickConfig('Add Item')
    await clickConfig(languageEnglish.model)
    expectBindingOrder()
    await clickConfig('Add Item')
    await clickConfig(languageEnglish.persona)
    expectBindingOrder()
    while (DBState.db.customSidebarItems.length > 0) {
        await clickConfig('Delete')
        expectBindingOrder()
    }
})
