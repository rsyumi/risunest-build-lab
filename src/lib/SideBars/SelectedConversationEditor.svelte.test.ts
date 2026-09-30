import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { createRawSnippet, mount, tick, unmount } from 'svelte'
import { writable } from 'svelte/store'
import { languageEnglish } from '../../lang/en'
import { DBState } from 'src/ts/stores.svelte'
import { createMetadataOnlySelectedConversation } from 'src/ts/storage/selectedConversationLifecycle'
import SelectedConversationEditor from './SelectedConversationEditor.svelte'
import { doingChat } from 'src/ts/process/generationState'

const mocks = vi.hoisted(() => ({
    acquire: vi.fn(),
    capture: vi.fn(),
    session: vi.fn(),
    changed: () => {},
    unsubscribe: vi.fn(),
}))
vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState, selectedCharID: writable(0) }
})
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: () => ({
        captureSelectedConversationTarget: mocks.capture,
        acquireCompleteConversation: mocks.acquire,
        getActiveConversationSession: mocks.session,
        subscribeActiveConversationViewportSource: (listener: () => void) => {
            mocks.changed = listener
            return mocks.unsubscribe
        },
    }),
}))

let editor: ReturnType<typeof mount> | undefined
const mounted = vi.fn()
function show(active = true, close?: () => void) {
    editor = mount(SelectedConversationEditor, {
        target: document.body,
        props: {
            active,
            close,
            children: createRawSnippet(() => ({
                render: () => '<input aria-label="name" />',
                setup: mounted,
            })),
        },
    })
}
function deferred() {
    let resolve!: (value: { release: () => void }) => void
    const promise = new Promise<{ release: () => void }>((done) => {
        resolve = done
    })
    return { promise, resolve }
}

beforeEach(() => {
    doingChat.set(false)
    vi.clearAllMocks()
    DBState.db = {
        characters: [
            {
                chaId: 'char-a',
                chatPage: 0,
                chats: [{ id: 'chat-a', message: [] }],
            },
        ],
    } as any
    mocks.capture.mockReturnValue({
        characterId: 'char-a',
        conversationId: 'chat-a',
    })
    mocks.acquire.mockReset()
    mocks.session.mockReturnValue(null)
})
afterEach(async () => {
    doingChat.set(false)
    if (editor) await unmount(editor)
    editor = undefined
    document.body.replaceChildren()
})

it('does not mount bound inputs or mount-time defaults before promotion completes', async () => {
    const pending = deferred()
    const release = vi.fn()
    mocks.acquire.mockReturnValue(pending.promise)
    show()
    await tick()
    expect(mounted).not.toHaveBeenCalled()
    expect(document.querySelector('input')).toBeNull()
    pending.resolve({ release })
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
    expect(release).not.toHaveBeenCalled()
    await unmount(editor!)
    editor = undefined
    expect(release).toHaveBeenCalledOnce()
    expect(mocks.unsubscribe).toHaveBeenCalledOnce()
})

it('does not load history for a hidden desktop sidebar', async () => {
    show(false)
    await tick()
    expect(mocks.acquire).not.toHaveBeenCalled()
    expect(mounted).not.toHaveBeenCalled()
})

it('waits for a windowed response to finish before promoting a newly opened editor', async () => {
    DBState.db.characters[0].chats[0] = createMetadataOnlySelectedConversation({
        id: 'chat-a', name: '', note: '', localLore: [],
    })
    doingChat.set(true)
    const close = vi.fn()
    mocks.acquire.mockResolvedValue({ release: vi.fn() })
    show(true, close)
    await tick()
    expect(mocks.acquire).not.toHaveBeenCalled()
    expect(mounted).not.toHaveBeenCalled()
    expect(document.body.textContent).toContain(languageEnglish.navigationBlockedWhileGenerating)
    expect(document.querySelector('button')?.textContent).toContain(languageEnglish.cancel)
    doingChat.set(false)
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
    expect(mocks.acquire).toHaveBeenCalledOnce()
})

it('keeps a complete editor mounted when generation starts and finishes', async () => {
    const session = {}
    const release = vi.fn()
    mocks.session.mockReturnValue(session)
    mocks.acquire.mockResolvedValue({ session, release })
    show()
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
    const input = document.querySelector('input')!
    doingChat.set(true)
    await tick()
    doingChat.set(false)
    await tick()
    expect(document.querySelector('input')).toBe(input)
    expect(mocks.acquire).toHaveBeenCalledOnce()
    expect(release).not.toHaveBeenCalled()
})

it('keeps the mounted editor and its lease across saves of the same session', async () => {
    const session = {}
    const release = vi.fn()
    mocks.session.mockReturnValue(session)
    mocks.acquire.mockResolvedValue({ session, release })
    show()
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
    const input = document.querySelector('input')!
    input.focus()
    mocks.changed()
    await tick()
    await tick()
    expect(mocks.acquire).toHaveBeenCalledOnce()
    expect(release).not.toHaveBeenCalled()
    expect(mounted).toHaveBeenCalledOnce()
    expect(document.activeElement).toBe(input)
})

it('keeps the mounted editor inert while another chat of the same character loads', async () => {
    const first = { session: {}, release: vi.fn() }
    const second = deferred()
    mocks.session.mockReturnValue(first.session)
    mocks.acquire
        .mockResolvedValueOnce(first)
        .mockReturnValueOnce(second.promise)
    DBState.db.characters[0].chats.push({ id: 'chat-b', message: [] } as any)
    show()
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
    const input = document.querySelector('input')!
    expect(input.closest('[inert]')).toBeNull()

    DBState.db.characters[0].chatPage = 1
    mocks.capture.mockReturnValue({
        characterId: 'char-a',
        conversationId: 'chat-b',
    })
    mocks.session.mockReturnValue({})
    mocks.changed()
    await tick()
    expect(mocks.acquire).toHaveBeenCalledTimes(2)
    expect(first.release).toHaveBeenCalledOnce()
    expect(document.querySelector('input')).toBe(input)
    expect(input.closest('[inert]')).not.toBeNull()
    expect(document.querySelector('[role="alert"]')).toBeNull()

    second.resolve({ release: vi.fn() })
    await vi.waitFor(() => expect(input.closest('[inert]')).toBeNull())
    expect(document.querySelector('input')).toBe(input)
    expect(mounted).toHaveBeenCalledOnce()
})

it('keeps the editor and its lease when the selection is republished with the same ids', async () => {
    const session = {}
    const release = vi.fn()
    mocks.session.mockReturnValue(session)
    mocks.acquire.mockResolvedValue({ session, release })
    show()
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
    const input = document.querySelector('input')!
    input.focus()
    DBState.db.characters[0] = {
        chaId: 'char-a',
        chatPage: 0,
        chats: [{ id: 'chat-a', message: [] }],
    } as any
    await tick()
    await tick()
    expect(mocks.acquire).toHaveBeenCalledOnce()
    expect(release).not.toHaveBeenCalled()
    expect(mounted).toHaveBeenCalledOnce()
    expect(document.activeElement).toBe(input)
})

it('remounts the editor when the selected character changes', async () => {
    const second = deferred()
    mocks.session.mockReturnValue({})
    mocks.acquire
        .mockResolvedValueOnce({ session: {}, release: vi.fn() })
        .mockReturnValueOnce(second.promise)
    show()
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
    DBState.db.characters[0] = {
        chaId: 'char-b',
        chatPage: 0,
        chats: [{ id: 'chat-c', message: [] }],
    } as any
    mocks.capture.mockReturnValue({
        characterId: 'char-b',
        conversationId: 'chat-c',
    })
    mocks.changed()
    await tick()
    expect(document.querySelector('input')).toBeNull()
    second.resolve({ release: vi.fn() })
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledTimes(2))
})

it('releases promotion that finishes after the panel closes', async () => {
    const pending = deferred()
    const release = vi.fn()
    mocks.acquire.mockReturnValue(pending.promise)
    show()
    await tick()
    await unmount(editor!)
    editor = undefined
    pending.resolve({ release })
    await vi.waitFor(() => expect(release).toHaveBeenCalledOnce())
    expect(mounted).not.toHaveBeenCalled()
})

it('keeps the popup close action available during promotion', async () => {
    mocks.acquire.mockReturnValue(deferred().promise)
    const close = vi.fn()
    show(true, close)
    await tick()
    document.querySelector<HTMLButtonElement>('button')!.click()
    expect(close).toHaveBeenCalledOnce()
    expect(mounted).not.toHaveBeenCalled()
})

it('discards stale selection and only mounts the newly acquired editor', async () => {
    const first = deferred()
    const second = deferred()
    const firstRelease = vi.fn()
    const secondRelease = vi.fn()
    mocks.acquire
        .mockReturnValueOnce(first.promise)
        .mockReturnValueOnce(second.promise)
    show()
    await tick()
    DBState.db.characters[0].chats[0].id = 'chat-b'
    mocks.capture.mockReturnValue({
        characterId: 'char-a',
        conversationId: 'chat-b',
    })
    mocks.changed()
    await tick()
    first.resolve({ release: firstRelease })
    await vi.waitFor(() => expect(firstRelease).toHaveBeenCalledOnce())
    expect(mounted).not.toHaveBeenCalled()
    second.resolve({ release: secondRelease })
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
})

it('keeps failed promotion uneditable and allows retry', async () => {
    mocks.acquire.mockRejectedValueOnce(new Error('synthetic read failure'))
    show()
    await vi.waitFor(() =>
        expect(document.querySelector('[role="alert"]')).not.toBeNull(),
    )
    expect(mounted).not.toHaveBeenCalled()
    mocks.acquire.mockResolvedValueOnce({ release: vi.fn() })
    document.querySelector<HTMLButtonElement>('button')!.click()
    await vi.waitFor(() => expect(mounted).toHaveBeenCalledOnce())
})

it('allows a complete nonpersistent playground but blocks an unowned partial shell', async () => {
    mocks.capture.mockReturnValue(null)
    show()
    await tick()
    expect(mounted).toHaveBeenCalledOnce()
    await unmount(editor!)
    editor = undefined
    mounted.mockClear()
    DBState.db.characters[0].chats[0] = createMetadataOnlySelectedConversation({
        id: 'chat-a',
        name: '',
        note: '',
        localLore: [],
    })
    show()
    await tick()
    expect(mounted).not.toHaveBeenCalled()
    expect(document.querySelector('[role="alert"]')).not.toBeNull()
    expect(mocks.acquire).not.toHaveBeenCalled()
})
