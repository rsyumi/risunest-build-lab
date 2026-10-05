import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, unmount } from 'svelte'
import type { Chat, Message } from 'src/ts/storage/database.svelte'
import { attachHistoryWindow } from 'src/ts/process/historyWindowIndex'
import ChatBindingLifecycle from './ChatBindingLifecycle.svelte'
import { DBState } from 'src/ts/stores.svelte'

const mocks = vi.hoisted(() => ({
    windowed: true,
    acquireComplete: vi.fn(),
    openWindow: vi.fn(),
    flush: vi.fn(async () => {}),
    error: vi.fn(),
}))
vi.mock('src/ts/stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    const DBState = $state({ db: {} as any })
    return { DBState, selectedCharID: writable(0) }
})
vi.mock('src/ts/storage/database.svelte', () => ({
    flushEffectiveToggleEdits: () => {},
    deriveEffectiveToggleVariables: () => {},
}))
vi.mock('src/ts/storage/conversationResidency', () => ({ isConversationSummaryStub: () => false }))
vi.mock('src/ts/process/index.svelte', () => ({
    getHistoryWindowMemoryMode: (historyLimit: boolean) => historyLimit && mocks.windowed ? 'none' : null,
    openSelectedHistoryWindow: mocks.openWindow,
}))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    acquireCompleteConversation: mocks.acquireComplete,
    captureSelectedConversationTarget: () => ({ characterId: 'character', conversationId: 'chat', navigationGeneration: 1 }),
    flushPendingData: mocks.flush,
}))
vi.mock('src/ts/alert', () => ({ alertError: mocks.error }))

const message = (data: string, role: Message['role'] = 'char'): Message => ({
    data,
    role,
    saying: 'bot',
    chatId: `message:${data}`,
})

// 99 stored messages, then the partial output of a reroll the app did not finish.
function interruptedConversation() {
    const stored = Array.from({ length: 99 }, (_, index) => message(`m${index}`, index % 2 ? 'char' : 'user'))
    stored.push(message('partial'))
    const shell: Chat = {
        id: 'chat',
        message: [],
        name: 'Synthetic',
        note: '',
        localLore: [],
        rerollRecovery: {
            attemptId: 'attempt',
            phase: 'generating',
            startIndex: 99,
            anchorId: 'message:m98',
            original: [message('original')],
            responseCount: 1,
            outputs: { 'message:partial': message('partial') },
        },
    }
    DBState.db = { characters: [{ chaId: 'character', chatPage: 0, chats: [shell] }] } as never
    return { stored, shell: () => DBState.db.characters[0].chats[0] as Chat }
}

let instance: ReturnType<typeof mount> | undefined

beforeEach(() => {
    mocks.windowed = true
    mocks.acquireComplete.mockReset()
    mocks.openWindow.mockReset()
    mocks.flush.mockClear()
    mocks.error.mockReset()
})

afterEach(() => {
    if (instance) unmount(instance)
    instance = undefined
})

it('recovers an interrupted reroll over the tail without loading the conversation', async () => {
    const { stored, shell } = interruptedConversation()
    const starts: number[] = []
    let released!: () => void
    const done = new Promise<void>((resolve) => { released = resolve })
    mocks.acquireComplete.mockImplementation(async () => ({ session: null, release: released }))
    mocks.openWindow.mockImplementation(async ({ tailStart }: { tailStart: (total: number) => number }) => {
        const start = tailStart(stored.length)
        starts.push(start)
        const { message: _message, ...metadata } = structuredClone($state.snapshot(shell()))
        const chat = { ...metadata, message: structuredClone(stored.slice(start)) } as Chat
        attachHistoryWindow(chat, start)
        return {
            chat,
            controller: {
                chat,
                absoluteStartIndex: start,
                isCurrent: () => true,
                applyRange(localStart: number, deleteCount: number, replacement: readonly Message[]) {
                    stored.splice(start + localStart, deleteCount, ...structuredClone([...replacement]))
                    chat.message.splice(localStart, deleteCount, ...structuredClone([...replacement]))
                    if (chat.rerollRecovery) shell().rerollRecovery = structuredClone(chat.rerollRecovery)
                    else delete shell().rerollRecovery
                    return true
                },
                release: () => {},
            },
            release: released,
        }
    })

    instance = mount(ChatBindingLifecycle, { target: document.body })
    await done

    expect(mocks.acquireComplete).not.toHaveBeenCalled()
    expect(starts).toEqual([98])
    expect(stored.map((entry) => entry.data)).toEqual([
        ...Array.from({ length: 99 }, (_, index) => `m${index}`),
        'original',
    ])
    expect(shell().rerollRecovery).toBeUndefined()
    expect(mocks.flush).toHaveBeenCalledWith('recover-reroll')
    expect(mocks.error).not.toHaveBeenCalled()
})

it('recovers over the complete conversation when the loading limit does not apply', async () => {
    mocks.windowed = false
    const { stored, shell } = interruptedConversation()
    let released!: () => void
    const done = new Promise<void>((resolve) => { released = resolve })
    mocks.acquireComplete.mockImplementation(async () => {
        shell().message = structuredClone(stored)
        return { session: null, release: released }
    })

    instance = mount(ChatBindingLifecycle, { target: document.body })
    await done

    expect(mocks.openWindow).not.toHaveBeenCalled()
    expect(mocks.acquireComplete).toHaveBeenCalledWith('recover-reroll', expect.objectContaining({ conversationId: 'chat' }))
    expect(shell().message.map((entry) => entry.data).slice(-2)).toEqual(['m98', 'original'])
    expect(shell().rerollRecovery).toBeUndefined()
    expect(mocks.flush).toHaveBeenCalledWith('recover-reroll')
})
