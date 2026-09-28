import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { writable } from 'svelte/store'
import { languageEnglish } from 'src/lang/en'
import { DBState } from 'src/ts/stores.svelte'
import DefaultChatScreen from './DefaultChatScreen.svelte'

const mocks = vi.hoisted(() => ({ trigger: vi.fn(), generate: vi.fn(), process: vi.fn(), error: vi.fn(), postFile: vi.fn() }))
vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState, selectedCharID: writable(0), PlaygroundStore: writable(0), hypaV3ModalOpen: writable(false),
        ScrollToMessageStore: writable(null), additionalChatMenu: writable([]), additionalFloatingActionButtons: writable([]),
        easyPanelStore: writable(false), chatPanelStore: writable(false), HideIconStore: writable(false) }
})
vi.mock('src/ts/process/index.svelte', () => ({ doingChat: writable(false), chatProcessStage: writable(0), sendChat: mocks.generate }))
vi.mock('src/ts/util', () => ({ sleep: async () => {}, getPersonaPrompt: () => '' }))
vi.mock('src/ts/alert', () => ({ alertError: mocks.error }))
vi.mock('src/ts/translator/translator', () => ({}))
vi.mock('src/ts/process/scripts', () => ({ processScript: mocks.process }))
vi.mock('src/ts/process/triggers', () => ({ runTrigger: mocks.trigger }))
vi.mock('src/ts/process/tts', () => ({}))
vi.mock('src/ts/process/command', () => ({}))
vi.mock('src/ts/process/files/multisend', () => ({ postChatFile: mocks.postFile }))
vi.mock('src/ts/globalApi.svelte', () => ({ aiLawApplies: false, chatFoldedState: writable(false), chatFoldedStateMessageIndex: writable(0) }))
vi.mock('src/ts/sync/multiuser', () => ({ ConnectionOpenStore: writable(false) }))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    getActiveConversationSession: () => null,
    getPersistentDataRuntime: () => ({
        captureSelectedConversationTarget: () => null, getActiveConversationSession: () => null,
        getActiveConversationViewportSource: () => null, getNavigationGeneration: () => 0,
        subscribeActiveConversationViewportSource: () => () => {},
    }),
}))
vi.mock('src/ts/chatScreenshotCapture', () => ({}))
vi.mock('src/ts/chatScreenshotSourceLease', () => ({}))
vi.mock('src/ts/chatScreenshotArchive', () => ({}))
vi.mock('src/ts/nativeScreenshotArchiveWriter', () => ({}))
vi.mock('src/ts/storage/androidSafBridge', () => ({}))
vi.mock('src/ts/process/modules', () => ({}))
vi.mock('src/ts/gui/colorscheme', () => ({ ColorSchemeTypeStore: writable('dark') }))
vi.mock('src/ts/plugins/plugins.svelte', () => ({ pluginV2: { editdisplay: new Set() } }))
vi.mock('src/ts/parser/parser.svelte', () => ({}))
vi.mock('./Suggestion.svelte', () => ({ default: () => {} }))
vi.mock('./Chats.svelte', () => ({ default: () => ({ jumpTo: async () => true }) }))
vi.mock('./AssetInput.svelte', () => ({ default: () => {} }))
vi.mock('./InlayFilePreview.svelte', () => ({ default: () => {} }))
vi.mock('./ChatScreenshotCaptureSurface.svelte', () => ({ default: () => {} }))
vi.mock('./ChatScreenshotDialog.svelte', () => ({ default: () => {} }))
vi.mock('../UI/MainMenu.svelte', () => ({ default: () => {} }))
vi.mock('../Others/PluginDefinedIcon.svelte', () => ({ default: () => {} }))

let instance: ReturnType<typeof mount> | undefined
let finishTrigger: () => void
function type(value: string) {
    const input = document.querySelector<HTMLTextAreaElement>('textarea.input-text')!
    input.value = value
    input.dispatchEvent(new Event('input', { bubbles: true }))
    return input
}
function send() {
    document.querySelector('textarea.input-text')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
}
beforeEach(async () => {
    vi.clearAllMocks()
    mocks.trigger.mockImplementation(() => new Promise(resolve => { finishTrigger = () => resolve(null) }))
    mocks.process.mockImplementation(async (_character, value) => value)
    mocks.generate.mockResolvedValue(true)
    DBState.db = {
        characters: [{ type: 'character', chaId: 'character', name: 'Synthetic', chatPage: 0, chats: [{ id: 'chat', message: [] }] }],
        sendWithEnter: true, username: 'User', userIcon: '', personas: [{ name: 'User', icon: '' }], selectedPersona: 0,
    } as any
    instance = mount(DefaultChatScreen, { target: document.body })
    await tick()
})
afterEach(async () => {
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

it('admits only one send while input processing is pending', async () => {
    type('Submitted')
    send()
    send()
    await tick()
    expect(mocks.trigger).toHaveBeenCalledTimes(1)
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledTimes(1))
    expect(DBState.db.characters[0].chats[0].message.map(message => message.data)).toEqual(['Submitted'])
})

it('sends the submitted text and preserves the next draft typed during input processing', async () => {
    type('Submitted')
    send()
    await vi.waitFor(() => expect(mocks.trigger).toHaveBeenCalledTimes(1))
    const input = type('Next draft')
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledTimes(1))
    expect(DBState.db.characters[0].chats[0].message.map(message => message.data)).toEqual(['Submitted'])
    expect(input.value).toBe('Next draft')
    expect(document.querySelector('textarea.input-text')).toBe(input)
})

it('retains failed input and releases the send guard for a retry', async () => {
    const failure = new Error('Synthetic input failure')
    mocks.trigger.mockRejectedValueOnce(failure)
    const input = type('Retry me')
    send()
    await vi.waitFor(() => expect(mocks.error).toHaveBeenCalledWith(failure))
    expect(input.value).toBe('Retry me')
    expect(mocks.generate).not.toHaveBeenCalled()
    send()
    await vi.waitFor(() => expect(mocks.trigger).toHaveBeenCalledTimes(2))
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledTimes(1))
    expect(input.value).toBe('')
    expect(DBState.db.characters[0].chats[0].message.map(message => message.data)).toEqual(['Retry me'])
})

it('holds the reroll shortcut until the submitted input has finished', async () => {
    const input = type('Submitted')
    send()
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await tick()
    expect(mocks.generate).not.toHaveBeenCalled()
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledTimes(1))
    expect(mocks.error).not.toHaveBeenCalled()
})

it('retains attachments on input failure and preserves files added for the next send', async () => {
    async function attach(id: string) {
        mocks.postFile.mockResolvedValueOnce([{ type: 'asset', data: id }])
        const fileItem = () => [...document.querySelectorAll('span')].find(span => span.textContent === languageEnglish.postFile)
        if (!fileItem()) {
            const menu = document.querySelector('.button-icon-send')!.nextElementSibling as HTMLButtonElement
            menu.click()
            await tick()
        }
        fileItem()!.parentElement!.click()
        await tick()
    }
    await attach('file-a')
    mocks.trigger.mockRejectedValueOnce(new Error('Synthetic input failure'))
    type('Submitted')
    send()
    await vi.waitFor(() => expect(mocks.error).toHaveBeenCalledTimes(1))
    expect(document.querySelectorAll('button[class*="-right-1"]')).toHaveLength(1)
    send()
    await vi.waitFor(() => expect(mocks.trigger).toHaveBeenCalledTimes(2))
    await attach('file-b')
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledTimes(1))
    expect(DBState.db.characters[0].chats[0].message[0].data).toBe('Submitted{{inlayed::file-a}}')
    expect(document.querySelectorAll('button[class*="-right-1"]')).toHaveLength(1)
    type('Following')
    send()
    await vi.waitFor(() => expect(mocks.trigger).toHaveBeenCalledTimes(3))
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledTimes(2))
    expect(DBState.db.characters[0].chats[0].message[1].data).toBe('Following{{inlayed::file-b}}')
    expect(document.querySelectorAll('button[class*="-right-1"]')).toHaveLength(0)
})
