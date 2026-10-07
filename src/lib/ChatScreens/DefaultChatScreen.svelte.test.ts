import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get, writable } from 'svelte/store'
import { languageEnglish } from 'src/lang/en'
import { DBState } from 'src/ts/stores.svelte'
import DefaultChatScreen from './DefaultChatScreen.svelte'
import { chatScreenState } from '../../ts/ui/chatScreenState.svelte'
import { doingChat } from 'src/ts/process/index.svelte'
import { noteGenerationMessage, noteGenerationStarted, subscribeGenerationEnd, type GenerationEndRecord } from 'src/ts/process/generationEnd'

const mocks = vi.hoisted(() => ({ trigger: vi.fn(), generate: vi.fn(), process: vi.fn(), error: vi.fn(), postFile: vi.fn(),
    bounded: false, appended: [] as any[], acquireComplete: vi.fn(), flush: vi.fn(async () => {}),
    historyLimit: false, targetCaptures: 0, openWindow: vi.fn(), notify: vi.fn(async () => {}), scope: { finish: vi.fn(), release: vi.fn() }, createScope: vi.fn(),
    translate: vi.fn(), chatsProps: null as any, confirm: vi.fn(async () => false),
    viewport: { jumpTo: async () => true, jumpToTop: vi.fn(async () => true), jumpToBottom: vi.fn(async () => true), navigateMessage: vi.fn(async () => true) },
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
vi.mock('src/ts/translator/translator', () => ({ translate: mocks.translate, isExpTranslator: () => false }))
vi.mock('src/ts/process/scripts', () => ({ processScript: mocks.process, createPromptScriptOperationScope: mocks.createScope }))
vi.mock('src/ts/process/triggers', () => ({ runTrigger: mocks.trigger }))
vi.mock('src/ts/process/tts', () => ({}))
vi.mock('src/ts/process/command', () => ({}))
vi.mock('src/ts/process/files/multisend', () => ({ postChatFile: mocks.postFile }))
vi.mock('src/ts/globalApi.svelte', () => ({ aiLawApplies: false, chatFoldedState: writable(false), chatFoldedStateMessageIndex: writable(0) }))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    getActiveConversationSession: () => null,
    getPersistentDataRuntime: () => ({
        captureSelectedConversationTarget: () => mocks.bounded || mocks.historyLimit || mocks.targetCaptures-- > 0 ? mocks.target : null, getActiveConversationSession: () => null,
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
    return mocks.viewport
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
    chatScreenState.clear()
    mocks.translate.mockResolvedValue('')
    mocks.bounded = false
    mocks.historyLimit = false
    mocks.targetCaptures = 0
    mocks.target = { characterId: 'character', conversationId: 'chat', navigationGeneration: 1, storeRevision: 1, sessionToken: 'windowed' }
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
    while (stopListening.length) stopListening.pop()!()
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

it('shows the scroll buttons for a while after the chat scrolls and runs each move', async () => {
    const nav = document.querySelector<HTMLElement>('[data-chat-scroll-nav]')!
    const button = (label: string) => nav.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)!
    expect(nav.classList.contains('invisible')).toBe(true)
    vi.useFakeTimers()
    try {
        mocks.chatsProps.onScrollMove()
        await tick()
        expect(nav.classList.contains('invisible')).toBe(false)
        await vi.advanceTimersByTimeAsync(1_000)
        button('Scroll Up').click()
        await vi.advanceTimersByTimeAsync(1_499)
        expect(nav.classList.contains('invisible')).toBe(false)
        await vi.advanceTimersByTimeAsync(1)
        expect(nav.classList.contains('invisible')).toBe(true)
    } finally {
        vi.useRealTimers()
    }
    button('Scroll Down').click()
    button('Scroll to Top').click()
    button('Scroll to Bottom').click()
    expect(mocks.viewport.navigateMessage.mock.calls).toEqual([['previous', 0], ['next', 0]])
    expect(mocks.viewport.jumpToTop).toHaveBeenCalledOnce()
    expect(mocks.viewport.jumpToBottom).toHaveBeenCalledOnce()
})

it('keeps the scroll buttons hidden while the chat menu is open', async () => {
    const nav = document.querySelector<HTMLElement>('[data-chat-scroll-nav]')!
    document.querySelector<HTMLButtonElement>('.button-icon-send')!.nextElementSibling!.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    mocks.chatsProps.onScrollMove()
    await tick()
    expect(nav.classList.contains('invisible')).toBe(true)
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


it('restores paired composer fields after the chat screen is destroyed and remounted', async () => {
    DBState.db.useAutoTranslateInput = true
    await tick()
    type('Unsent original')
    const translated = document.querySelector<HTMLTextAreaElement>('#messageInputTranslate')!
    translated.value = 'Unsent translation'
    translated.dispatchEvent(new Event('input', { bubbles: true }))
    await tick()
    await unmount(instance!)
    instance = mount(DefaultChatScreen, { target: document.body })
    await tick()
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('Unsent original')
    expect(document.querySelector<HTMLTextAreaElement>('#messageInputTranslate')!.value).toBe('Unsent translation')
})

it('keeps each composer with its conversation and discards a removed conversation', async () => {
    const character = DBState.db.characters[0]
    character.chats.push({ id: 'other', message: [] } as any)
    type('First draft')
    character.chatPage = 1
    await tick()
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('')
    type('Second draft')
    character.chatPage = 0
    await tick()
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('First draft')
    character.chats.splice(1, 1)
    await tick()
    character.chats.push({ id: 'other', message: [] } as any)
    character.chatPage = 1
    await tick()
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('')
})

it('a send completing after remount preserves new typing in the restored composer', async () => {
    type('Submitted before Settings')
    send()
    await vi.waitFor(() => expect(mocks.trigger).toHaveBeenCalledOnce())
    await unmount(instance!)
    instance = mount(DefaultChatScreen, { target: document.body })
    await tick()
    type('Typed after Settings')
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledOnce())
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('Typed after Settings')
})

it('a late translation cannot replace newer text or write into another conversation', async () => {
    DBState.db.useAutoTranslateInput = true
    DBState.db.characters[0].chats.push({ id: 'other', message: [] } as any)
    let finishTranslation: (value: string) => void = () => {}
    mocks.translate.mockImplementationOnce(() => new Promise(resolve => { finishTranslation = resolve }))
    await tick()
    type('Original for translation')
    await vi.waitFor(() => expect(mocks.translate).toHaveBeenCalledWith('Original for translation', false))
    type('New original')
    DBState.db.characters[0].chatPage = 1
    await tick()
    type('Other conversation draft')
    finishTranslation('Late result')
    await tick()
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('Other conversation draft')
    expect(document.querySelector<HTMLTextAreaElement>('#messageInputTranslate')!.value).toBe('')
    DBState.db.characters[0].chatPage = 0
    await tick()
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('New original')
    expect(document.querySelector<HTMLTextAreaElement>('#messageInputTranslate')!.value).toBe('')
})


it('clears the same submitted composer after it was restored by a remount', async () => {
    type('Submitted before grid')
    send()
    await vi.waitFor(() => expect(mocks.trigger).toHaveBeenCalledOnce())
    await unmount(instance!)
    instance = mount(DefaultChatScreen, { target: document.body })
    await tick()
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('Submitted before grid')
    finishTrigger()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledOnce())
    expect(document.querySelector<HTMLTextAreaElement>('textarea.input-text')!.value).toBe('')
})

it('does not send or reroll from composing keyboard events', async () => {
    const input = type('Composition draft')
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true }))
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, isComposing: true, bubbles: true }))
    await tick()
    expect(mocks.trigger).not.toHaveBeenCalled()
    expect(mocks.generate).not.toHaveBeenCalled()
    expect(input.value).toBe('Composition draft')
})

const stopListening: Array<() => void> = []
function listenGenerationEnd() {
    const records: Array<GenerationEndRecord & { busy: boolean }> = []
    stopListening.push(subscribeGenerationEnd((record) => records.push({ ...record, busy: get(doingChat) })))
    return records
}
// Stands in for sendChat: it holds the busy flag, which a thrown generation leaves set for the caller to clear.
function generation(outcome: boolean | Error, during?: () => void | Promise<void>) {
    return async () => {
        doingChat.set(true)
        noteGenerationStarted(mocks.target)
        noteGenerationMessage(mocks.target, 'gen-1')
        await during?.()
        if (outcome instanceof Error) throw outcome
        doingChat.set(false)
        return outcome
    }
}
const endOf = (status: string, reroll = false) => ({
    characterId: 'character', conversationId: 'chat', status, reroll, messageIds: ['gen-1'], busy: false,
})

it('reports a completed send once the chat is no longer busy', async () => {
    mocks.bounded = true
    const records = listenGenerationEnd()
    mocks.generate.mockImplementation(generation(true))
    type('Hello')
    send()
    await vi.waitFor(() => expect(records).toEqual([endOf('completed')]))
})

it('reports a failed send after the error, once the chat is no longer busy', async () => {
    mocks.bounded = true
    const records = listenGenerationEnd()
    const failure = new Error('Synthetic provider failure')
    mocks.generate.mockImplementation(generation(failure))
    type('Hello')
    send()
    await vi.waitFor(() => expect(records).toEqual([endOf('failed')]))
    expect(mocks.error).toHaveBeenCalledWith(failure)
})

it('reports a send that ended without completing as failed', async () => {
    mocks.bounded = true
    const records = listenGenerationEnd()
    mocks.generate.mockImplementation(generation(false))
    type('Hello')
    send()
    await vi.waitFor(() => expect(records).toEqual([endOf('failed')]))
})

it('reports a send the user stopped as aborted', async () => {
    mocks.bounded = true
    const records = listenGenerationEnd()
    mocks.generate.mockImplementation(async (options: { signal: AbortSignal }) => generation(false, async () => {
        await tick()
        document.querySelector<HTMLButtonElement>('button[aria-labelledby="cancel"]')!.click()
        expect(options.signal.aborted).toBe(true)
    })())
    type('Hello')
    send()
    await vi.waitFor(() => expect(records).toEqual([endOf('aborted')]))
})

it('reports a send dropped by a conversation change as aborted', async () => {
    mocks.bounded = true
    const records = listenGenerationEnd()
    mocks.generate.mockImplementation(generation(false, () => {
        mocks.target = { ...mocks.target, navigationGeneration: 2 }
    }))
    type('Hello')
    send()
    await vi.waitFor(() => expect(records).toEqual([endOf('aborted')]))
})

it('reports nothing for a send that never entered generation', async () => {
    mocks.bounded = true
    const records = listenGenerationEnd()
    mocks.generate.mockResolvedValue(false)
    type('Hello')
    send()
    await vi.waitFor(() => expect(mocks.generate).toHaveBeenCalledOnce())
    await tick()
    expect(records).toEqual([])
})

it('reports a completed reroll after its candidate is stored and announced', async () => {
    mocks.historyLimit = true
    const records = listenGenerationEnd()
    const store: any[] = stored(40)
    mocks.openWindow.mockImplementation(async ({ tailStart }) => windowOver(store, tailStart(store.length)))
    mocks.generate.mockImplementation(generation(true, () => {
        store.push({ role: 'char', data: 'new', chatId: 'gen-1' })
    }))
    mocks.notify.mockImplementation(async () => {
        expect(records).toEqual([])
    })
    type('').dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await vi.waitFor(() => expect(records).toEqual([endOf('completed', true)]))
    expect(store.at(-1).responseVariants.candidates).toHaveLength(2)
    expect(mocks.notify).toHaveBeenCalledWith('new')
})

it('reports a completed reroll of the resident conversation after its candidate is stored and announced', async () => {
    // Only the reroll's own capture sees a target, so the candidate is generated on the resident conversation.
    mocks.targetCaptures = 1
    const records = listenGenerationEnd()
    const chat = DBState.db.characters[0].chats[0]
    chat.message = [{ role: 'user', data: 'question', chatId: 'question' }, { role: 'char', data: 'old', chatId: 'old' }]
    mocks.generate.mockImplementation(generation(true, () => {
        chat.message.push({ role: 'char', data: 'new', chatId: 'gen-1' })
    }))
    mocks.notify.mockImplementation(async () => {
        expect(records).toEqual([])
    })
    type('').dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await vi.waitFor(() => expect(records).toEqual([endOf('completed', true)]))
    expect(chat.message.at(-1)?.responseVariants?.candidates).toHaveLength(2)
    expect(mocks.notify).toHaveBeenCalledWith('new')
})

it('reports a reroll whose tail could not be reopened as failed', async () => {
    mocks.historyLimit = true
    const records = listenGenerationEnd()
    const store: any[] = stored(40)
    let opens = 0
    mocks.openWindow.mockImplementation(async ({ tailStart }) => ++opens > 1 ? null : windowOver(store, tailStart(store.length)))
    mocks.generate.mockImplementation(generation(true, () => {
        store.push({ role: 'char', data: 'new', chatId: 'gen-1' })
    }))
    type('').dispatchEvent(new KeyboardEvent('keydown', { key: 'm', ctrlKey: true, bubbles: true }))
    await vi.waitFor(() => expect(records).toEqual([endOf('failed', true)]))
    expect(mocks.error).toHaveBeenCalledWith(languageEnglish.generationConversationChanged)
})
