import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { writable } from 'svelte/store'
import { languageEnglish } from 'src/lang/en'
import { DBState } from 'src/ts/stores.svelte'
import DefaultChatScreen from './DefaultChatScreen.svelte'

const mocks = vi.hoisted(() => ({ trigger: vi.fn(), generate: vi.fn(), process: vi.fn(), error: vi.fn(), postFile: vi.fn(),
    bounded: false, appended: [] as any[], acquireComplete: vi.fn(), flush: vi.fn(async () => {}),
    historyLimit: false, openWindow: vi.fn(), notify: vi.fn(async () => {}), scope: { finish: vi.fn(), release: vi.fn() }, createScope: vi.fn(),
    chatsProps: null as any, confirm: vi.fn(async () => false),
    target: { characterId: 'character', conversationId: 'chat', navigationGeneration: 1, storeRevision: 1, sessionToken: 'windowed' },
}))
vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState, selectedCharID: writable(0), PlaygroundStore: writable(0), hypaV3ModalOpen: writable(false),
        ScrollToMessageStore: writable(null), additionalChatMenu: writable([]), additionalFloatingActionButtons: writable([]),
        easyPanelStore: writable(false), chatPanelStore: writable(false), HideIconStore: writable(false) }
})
vi.mock('src/ts/process/index.svelte', () => ({ doingChat: writable(false), chatProcessStage: writable(0), sendChat: mocks.generate, getSelectedBoundedGenerationFallbackReason: () => mocks.bounded ? null : 'test-complete-conversation',
    getHistoryWindowMemoryMode: (historyLimit: boolean) => historyLimit && mocks.historyLimit ? 'none' : null, openSelectedHistoryWindow: mocks.openWindow, notifyGenerationCompletion: mocks.notify }))
vi.mock('src/ts/util', () => ({ sleep: async () => {}, getPersonaPrompt: () => '' }))
vi.mock('src/ts/alert', () => ({ alertError: mocks.error, alertConfirm: mocks.confirm }))
vi.mock('src/ts/translator/translator', () => ({}))
vi.mock('src/ts/process/scripts', () => ({ processScript: mocks.process, createPromptScriptOperationScope: mocks.createScope }))
vi.mock('src/ts/process/triggers', () => ({ runTrigger: mocks.trigger }))
vi.mock('src/ts/process/tts', () => ({}))
vi.mock('src/ts/process/command', () => ({}))
vi.mock('src/ts/process/files/multisend', () => ({ postChatFile: mocks.postFile }))
vi.mock('src/ts/globalApi.svelte', () => ({ aiLawApplies: false, chatFoldedState: writable(false), chatFoldedStateMessageIndex: writable(0) }))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    getActiveConversationSession: () => null,
    getPersistentDataRuntime: () => ({
        captureSelectedConversationTarget: () => mocks.bounded || mocks.historyLimit ? mocks.target : null, getActiveConversationSession: () => null,
        acknowledgeGenerationCompletion: async () => {},
        captureSelectedConversationAuthority: () => ({ totalMessages: 1500 }),
        acquireCompleteConversation: mocks.acquireComplete,
        flushPendingData: mocks.flush,
        captureWindowedConversationMutationController: () => ({
            applyRange: (_start: number, _count: number, messages: any[]) => { mocks.appended.push(...messages); return true },
            release: () => {},
        }),
        getActiveConversationViewportSource: () => null, getNavigationGeneration: () => 0,
        subscribeActiveConversationViewportSource: () => () => {},
    }),
}))
vi.mock('src/ts/chatScreenshotCapture', () => ({}))
vi.mock('src/ts/chatScreenshotSourceLease', () => ({}))
vi.mock('src/ts/chatScreenshotArchive', () => ({}))
vi.mock('src/ts/nativeScreenshotArchiveWriter', () => ({}))
vi.mock('src/ts/storage/androidSafBridge', () => ({}))
vi.mock('src/ts/process/modules', () => ({ getModuleRegexScripts: () => [] }))
vi.mock('src/ts/gui/colorscheme', () => ({ ColorSchemeTypeStore: writable('dark') }))
vi.mock('src/ts/plugins/plugins.svelte', () => ({ pluginV2: { editdisplay: new Set() } }))
vi.mock('src/ts/parser/parser.svelte', () => ({}))
vi.mock('./Suggestion.svelte', () => ({ default: () => {} }))
vi.mock('./Chats.svelte', () => ({ default: (_anchor: unknown, props: unknown) => {
    mocks.chatsProps = props
    return { jumpTo: async () => true }
} }))
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
    mocks.bounded = false
    mocks.historyLimit = false
    mocks.createScope.mockReturnValue(mocks.scope)
    mocks.appended = []
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

it('sends plain input through the bounded controller without complete-history acquisition', async () => {
    mocks.bounded = true
    type('Bounded submitted text')
    send()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledOnce())
    expect(mocks.appended).toEqual([expect.objectContaining({ role: 'user', data: 'Bounded submitted text' })])
    expect(mocks.acquireComplete).not.toHaveBeenCalled()
    expect(mocks.trigger).not.toHaveBeenCalled()
    expect(mocks.flush).toHaveBeenCalledWith('generation-input')
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('')
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

function windowOver(store: any[], start: number) {
    const chat = { id: 'chat', message: structuredClone(store.slice(start)) }
    const controller = {
        chat,
        absoluteStartIndex: start,
        isCurrent: () => true,
        applyRange: (localStart: number, deleteCount: number, messages: any[]) => {
            store.splice(start + localStart, deleteCount, ...structuredClone(messages))
            chat.message.splice(localStart, deleteCount, ...structuredClone(messages))
            return true
        },
        reconcileMetadata: () => true,
        release: () => {},
    }
    return { chat, controller, release: vi.fn() }
}
const stored = (count: number) => Array.from({ length: count }, (_, index) => ({
    role: index % 2 ? 'char' : 'user', data: `m${index}`, chatId: `id-${index}`,
}))

it('runs the input step over a history window when the loading limit is on', async () => {
    mocks.historyLimit = true
    const store = stored(4)
    const window = windowOver(store, 2)
    mocks.openWindow.mockResolvedValue(window)
    mocks.trigger.mockImplementation(async (_character, _mode, { chat }) => ({
        chat: { ...chat, message: chat.message.map((message: any, index: number) => index ? message : { ...message, data: 'triggered' }) },
    }))
    mocks.process.mockImplementation(async (_character, value) => `${value} processed`)
    type('Hello')
    send()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledOnce())
    expect(mocks.openWindow).toHaveBeenCalledWith({ register: true })
    expect(mocks.trigger).toHaveBeenCalledWith(DBState.db.characters[0], 'input', { chat: window.chat })
    expect(mocks.createScope).toHaveBeenCalledWith(DBState.db.characters[0], {
        historyWindow: { chat: window.chat, conversationId: 'chat' },
    })
    expect(mocks.process).toHaveBeenCalledWith(DBState.db.characters[0], 'Hello', 'editinput', {}, { promptOperationScope: mocks.scope })
    expect(mocks.scope.finish).toHaveBeenCalledOnce()
    expect(store.map((message) => message.data)).toEqual(['m0', 'm1', 'triggered', 'm3', 'Hello processed'])
    expect(store[4]).toEqual(expect.objectContaining({ role: 'user', chatId: expect.any(String) }))
    expect(window.release).toHaveBeenCalledOnce()
    expect(mocks.flush).toHaveBeenCalledWith('generation-input')
    expect(mocks.generate).toHaveBeenCalledWith(expect.objectContaining({ historyLimit: true }))
    expect(mocks.acquireComplete).not.toHaveBeenCalled()
    expect(DBState.db.characters[0].chats[0].message).toEqual([])
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('')
})

it('appends the says-nothing message over the window for an empty send', async () => {
    mocks.historyLimit = true
    DBState.db.useSayNothing = true
    const store = stored(4)
    mocks.openWindow.mockResolvedValue(windowOver(store, 3))
    send()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledOnce())
    expect(mocks.trigger).not.toHaveBeenCalled()
    expect(store.at(-1)).toEqual(expect.objectContaining({ role: 'user', data: '*says nothing*' }))
})

it('rerolls over a tail window when the loading limit is on', async () => {
    mocks.historyLimit = true
    const store: any[] = stored(40)
    mocks.openWindow.mockImplementation(async ({ tailStart }) => windowOver(store, tailStart(store.length)))
    mocks.generate.mockImplementation(async () => {
        store.push({ role: 'char', data: 'new', chatId: 'new' })
        return true
    })
    const before = structuredClone(store.slice(0, 39))
    type('').dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledOnce())
    await vi.waitFor(() => expect(store.at(-1)?.responseVariants?.candidates).toHaveLength(2))
    expect(mocks.generate).toHaveBeenCalledWith(expect.objectContaining({ historyLimit: true }))
    expect(store.slice(0, 39)).toEqual(before)
    expect(store.at(-1).data).toBe('new')
    for (const [options] of mocks.openWindow.mock.calls) expect(options.tailStart(store.length)).toBeGreaterThan(30)
    expect(mocks.acquireComplete).not.toHaveBeenCalled()
    expect(mocks.error).not.toHaveBeenCalled()
    expect(mocks.notify).toHaveBeenCalledWith('new')
})

it('tells the user when no history window opens for the input step', async () => {
    mocks.historyLimit = true
    mocks.openWindow.mockResolvedValue(null)
    const input = type('Hello')
    send()
    await vi.waitFor(() => expect(mocks.error).toHaveBeenCalledWith(languageEnglish.chatConversationActionFailed))
    expect(mocks.error).toHaveBeenCalledOnce()
    expect(mocks.generate).not.toHaveBeenCalled()
    expect(input.value).toBe('Hello')
})

it('tells the user when no tail window opens for a reroll', async () => {
    mocks.historyLimit = true
    mocks.openWindow.mockResolvedValue(null)
    type('').dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await vi.waitFor(() => expect(mocks.error).toHaveBeenCalledWith(languageEnglish.chatConversationActionFailed))
    expect(mocks.error).toHaveBeenCalledOnce()
    expect(mocks.generate).not.toHaveBeenCalled()
})

it.each([
    ['next', 'onNextReroll'],
    ['previous', 'unReroll'],
])('tells the user when no tail window opens for the %s candidate', async (_name, prop) => {
    mocks.historyLimit = true
    mocks.openWindow.mockResolvedValue(null)
    await mocks.chatsProps[prop]()
    expect(mocks.error).toHaveBeenCalledWith(languageEnglish.chatConversationActionFailed)
    expect(mocks.error).toHaveBeenCalledOnce()
    expect(mocks.confirm).not.toHaveBeenCalled()
})

it('tells the user when the tail cannot be reopened after a reroll generated a reply', async () => {
    mocks.historyLimit = true
    const store: any[] = stored(40)
    let opens = 0
    mocks.openWindow.mockImplementation(async ({ tailStart }) => ++opens > 1 ? null : windowOver(store, tailStart(store.length)))
    mocks.generate.mockImplementation(async () => {
        store.push({ role: 'char', data: 'new', chatId: 'new' })
        return true
    })
    type('').dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await vi.waitFor(() => expect(mocks.error).toHaveBeenCalledWith(languageEnglish.generationConversationChanged))
    expect(mocks.error).toHaveBeenCalledOnce()
    expect(mocks.notify).not.toHaveBeenCalled()
    // The recovery stays stored, so opening the conversation restores the original response.
    expect(store.at(-1)?.data).toBe('new')
})

it('rerolls with a user message last by reading only the newest messages', async () => {
    mocks.historyLimit = true
    const store: any[] = [...stored(40), { role: 'user', data: 'question', chatId: 'question' }]
    mocks.openWindow.mockImplementation(async ({ tailStart }) => windowOver(store, tailStart(store.length)))
    const before = structuredClone(store)
    type('').dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await vi.waitFor(() => expect(mocks.openWindow).toHaveBeenCalled())
    await tick()
    for (const [options] of mocks.openWindow.mock.calls) expect(options.tailStart(store.length)).toBeGreaterThan(30)
    expect(mocks.generate).not.toHaveBeenCalled()
    expect(mocks.error).not.toHaveBeenCalled()
    expect(store).toEqual(before)
})
