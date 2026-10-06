// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import { chatScreenState } from 'src/ts/ui/chatScreenState.svelte'
import { mount, tick, unmount } from 'svelte'
import type { character, Message } from 'src/ts/storage/database.svelte'
import { ActiveConversationSession } from 'src/ts/storage/activeConversationSession'
import { SynchronousSessionConversationViewportSource } from 'src/ts/conversationViewportSource'
import type { ChatViewEvent, ChatViewRow } from 'src/ts/plugins/chatViewEvents'

const pinned = vi.hoisted(() => ({ resolve: vi.fn() }))

vi.mock('src/ts/alert', () => ({ alertNormal: vi.fn() }))
vi.mock('src/ts/plugins/pinnedConversationPosition', () => ({ resolvePinnedConversationPosition: pinned.resolve }))
vi.mock('src/ts/characters', () => ({ getCharImage: async (source: string | undefined) => `image:${source ?? ''}` }))
vi.mock('src/ts/globalApi.svelte', () => ({ chatFoldedStateMessageIndex: { index: -1 } }))
vi.mock('src/ts/ui/yieldToUi', () => ({ yieldToMainThread: () => Promise.resolve() }))
vi.mock('src/ts/stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    const DBState = $state({
        db: {
            streamingDisplayOptimizationMode: 'balanced',
            streamingThoughtMode: undefined as string | undefined,
            autoScrollToNewMessage: false,
            alwaysScrollToNewMessage: false,
            characters: [] as unknown[],
        },
    })
    const selIdState = $state({ selId: 0 })
    return {
        DBState,
        selIdState,
        selectedCharID: writable(0),
        ReloadChatPointer: writable({}),
        ReloadGUIPointer: writable(0),
        createSimpleCharacter: (char: character) => ({
            type: 'simple',
            chaId: char.chaId,
            virtualscript: char.virtualscript,
            customscript: char.customscript,
            additionalAssets: char.additionalAssets,
            emotionImages: char.emotionImages,
            triggerscript: char.triggerscript,
        }),
    }
})
vi.mock('./Chat.svelte', async () => ({ default: (await import('./ChatMountProbe.test.svelte')).default }))
vi.mock('./CreatorQuote.svelte', async () => ({ default: (await import('./ChatMountProbe.test.svelte')).default }))

import { DBState, selIdState } from 'src/ts/stores.svelte'
import { chatViewEvents } from 'src/ts/plugins/chatViewHost.svelte'
import ChatsHarness from './ChatsHarness.test.svelte'
import { chatMountProbe, resetChatMountProbe } from './chatMountProbe.testSupport'

interface HarnessInstance {
    setMessages(messages: Message[]): void
    updateMessage(index: number, data: string): void
    setStreaming(isStreaming: boolean): void
    switchCharacter(character: character, messages: Message[]): void
    jumpTo(index: number): Promise<boolean>
}

class TestResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
}

function makeMessage(index: number, overrides: Partial<Message> = {}): Message {
    return { role: index % 2 === 0 ? 'char' : 'user', data: `message-${index}`, chatId: `message-id-${index}`, ...overrides }
}

function makeCharacter(messages: Message[], isStreaming = false, chaId = 'character-id', chatId = 'chat-room-id'): character {
    return {
        type: 'character',
        name: 'Character',
        image: 'character.png',
        chaId,
        chatPage: 0,
        chats: [{ id: chatId, message: messages, isStreaming, activeStreamingDisplayOptimizationMode: 'balanced' }],
        firstMessage: 'first greeting',
        alternateGreetings: [],
        creatorNotes: '',
        removedQuotes: false,
        customscript: [],
        additionalAssets: [],
        emotionImages: [],
        triggerscript: [],
    } as unknown as character
}

const selectable = (char: character) => ({ chaId: char.chaId, chatPage: 0, chats: [{ id: char.chats[0].id }] }) as unknown as character
const viewRow = (index: number): ChatViewRow => ({ index, messageId: `message-id-${index}`, role: index % 2 === 0 ? 'char' : 'user' })
const byIndex = (rows: readonly ChatViewRow[]) => [...rows].sort((left, right) => left.index - right.index)
const range = (start: number, end: number) => Array.from({ length: end - start }, (_, offset) => start + offset)

describe('chat view events from the chat row lifecycle', () => {
    let mounted: ReturnType<typeof mount> | undefined
    let target: HTMLDivElement
    let frames: Map<number, FrameRequestCallback>
    let events: ChatViewEvent[]
    let model: Map<number, ChatViewRow>

    const harness = () => mounted as HarnessInstance
    const runFrames = () => {
        const batch = [...frames.values()]
        frames.clear()
        for (const frame of batch) frame(performance.now())
    }
    const listen = async () => {
        const registration = chatViewEvents.forOwner('plugin').register((event) => {
            events.push(event)
            if (event.type === 'conversation') {
                model.clear()
                return
            }
            for (const row of event.unmounted) model.delete(row.index)
            for (const row of event.mounted) model.set(row.index, row)
        })
        // The selection is reported once its position has been resolved.
        await vi.waitFor(() => expect(pinned.resolve).toHaveBeenCalled())
        await new Promise((resolve) => setTimeout(resolve, 0))
        return registration
    }
    const rowEvents = () => events.filter((event): event is Extract<ChatViewEvent, { type: 'rows' }> => event.type === 'rows')
    const domIndices = () => [...target.querySelectorAll<HTMLElement>('[data-chat-index]')]
        .filter((element) => element.querySelector('[data-chat-probe]'))
        .map((element) => Number(element.dataset.chatIndex))
        .sort((left, right) => left - right)
    const modelIndices = () => [...model.keys()].sort((left, right) => left - right)
    const settled = async () => {
        await vi.waitFor(() => expect(target.querySelectorAll('[data-chat-mount-pending]')).toHaveLength(0))
        for (let pass = 0; pass < 3; pass++) {
            await tick()
            await new Promise((resolve) => setTimeout(resolve, 0))
        }
    }
    const mountChats = async (messages: Message[], char = makeCharacter(messages), props: Record<string, unknown> = {}) => {
        DBState.db.characters = [selectable(char)]
        selIdState.selId = 0
        mounted = mount(ChatsHarness, { target, props: { initialMessages: messages, initialCharacter: char, ...props } })
        await vi.waitFor(() => expect(domIndices().length).toBeGreaterThan(0))
        await settled()
    }

    beforeEach(() => {
        chatScreenState.clear()
        resetChatMountProbe()
        frames = new Map()
        let nextFrame = 1
        vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
            const id = nextFrame++
            frames.set(id, callback)
            return id
        })
        vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id))
        vi.stubGlobal('ResizeObserver', TestResizeObserver)
        DBState.db.streamingThoughtMode = undefined
        pinned.resolve.mockReset()
        // Without archived characters the position the index APIs read is the working-set position.
        pinned.resolve.mockImplementation(async (characterId: string) => ({
            characterIndex: (DBState.db.characters as character[]).findIndex((char) => char.chaId === characterId),
            chatIndex: 0,
        }))
        events = []
        model = new Map()
        target = document.createElement('div')
        document.body.appendChild(target)
    })

    afterEach(async () => {
        chatViewEvents.forOwner('plugin').dispose()
        if (mounted) await unmount(mounted)
        mounted = undefined
        document.body.replaceChildren()
        vi.unstubAllGlobals()
    })

    test('reports the conversation and every mounted row in one frame after registration', async () => {
        const messages = range(0, 8).map((index) => makeMessage(index))
        await mountChats(messages)

        expect((await listen()).id).toEqual(expect.any(String))
        expect(pinned.resolve).toHaveBeenCalledExactlyOnceWith('character-id', 'chat-room-id')
        expect(events).toEqual([])
        runFrames()
        expect(events).toHaveLength(2)
        expect(events[0]).toEqual({
            type: 'conversation', characterId: 'character-id', conversationId: 'chat-room-id', characterIndex: 0, chatIndex: 0,
        })
        const [rows] = rowEvents()
        expect(rows).toMatchObject({ characterId: 'character-id', conversationId: 'chat-room-id', unmounted: [], rerendered: [] })
        expect(byIndex(rows.mounted)).toEqual(range(0, 8).map(viewRow))
        runFrames()
        expect(events).toHaveLength(2)
    })

    test('reports appended and removed rows and the rows refreshed around them', async () => {
        const messages = range(0, 8).map((index) => makeMessage(index))
        await mountChats(messages)
        await listen()
        runFrames()
        events = []

        harness().setMessages([...messages, makeMessage(8)])
        await vi.waitFor(() => expect(domIndices()).toEqual(range(0, 9)))
        await settled()
        runFrames()
        expect(rowEvents()).toHaveLength(1)
        expect(rowEvents()[0].mounted).toEqual([viewRow(8)])
        expect(rowEvents()[0].unmounted).toEqual([])
        expect(modelIndices()).toEqual(domIndices())

        events = []
        harness().setMessages(messages.slice(0, 7))
        await vi.waitFor(() => expect(domIndices()).toEqual(range(0, 7)))
        await settled()
        runFrames()
        expect(rowEvents()).toHaveLength(1)
        expect(byIndex(rowEvents()[0].unmounted)).toEqual([viewRow(7), viewRow(8)])
        expect(rowEvents()[0].mounted).toEqual([])
        expect(modelIndices()).toEqual(range(0, 7))
    })

    test('follows the bounded viewport through scrolling and direct jumps', async () => {
        const messages = range(0, 200).map((index) => makeMessage(index))
        await mountChats(messages)
        await listen()
        runFrames()
        expect(modelIndices()).toEqual(range(136, 200))
        expect(modelIndices()).toEqual(domIndices())

        const scrollParent = target.querySelector<HTMLElement>('.scroll-parent')!
        scrollParent.getBoundingClientRect = () => ({ top: 0, bottom: 500, height: 500 } as DOMRect)
        const [gap] = target.querySelectorAll<HTMLElement>('[data-chat-gap]')
        gap.getBoundingClientRect = () => ({ top: 0, bottom: 100, height: 100 } as DOMRect)
        scrollParent.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }))
        scrollParent.scrollTop = -100
        scrollParent.dispatchEvent(new Event('scroll'))
        await vi.waitFor(() => {
            runFrames()
            expect(domIndices()).toContain(135)
            expect(domIndices()).toHaveLength(64)
        })
        await settled()
        runFrames()
        expect(rowEvents().some((event) => event.unmounted.some((row) => row.index >= 136))).toBe(true)
        expect(modelIndices()).toEqual(domIndices())

        for (const index of [0, 120, 199]) {
            let jumped: boolean | undefined
            void harness().jumpTo(index).then((value) => { jumped = value })
            await vi.waitFor(() => {
                runFrames()
                expect(jumped).toBe(true)
            })
            await settled()
            runFrames()
            expect(modelIndices()).toEqual(domIndices())
            expect(model.get(index)).toEqual(viewRow(index))
        }
    })

    test('starts over when the selected conversation changes', async () => {
        const messages = range(0, 4).map((index) => makeMessage(index))
        const first = makeCharacter(messages)
        await mountChats(messages, first)
        await listen()
        runFrames()
        events = []

        const nextMessages = range(0, 3).map((index) => makeMessage(index, { chatId: `next-${index}` }))
        const next = makeCharacter(nextMessages, false, 'next-character-id', 'next-chat-id')
        DBState.db.characters = [selectable(first), selectable(next)]
        selIdState.selId = 1
        harness().switchCharacter(next, nextMessages)
        await vi.waitFor(() => expect(target.querySelector('[data-message="message-3"]')).toBeNull())
        await settled()
        runFrames()
        expect(events[0]).toEqual({
            type: 'conversation', characterId: 'next-character-id', conversationId: 'next-chat-id', characterIndex: 1, chatIndex: 0,
        })
        expect(rowEvents().every((event) => event.characterId === 'next-character-id' && event.conversationId === 'next-chat-id')).toBe(true)
        expect(rowEvents().flatMap((event) => event.unmounted)).toEqual([])
        expect(byIndex([...model.values()])).toEqual(range(0, 3).map((index) => ({ ...viewRow(index), messageId: `next-${index}` })))

        events = []
        selIdState.selId = -1
        await tick()
        runFrames()
        expect(events).toEqual([{ type: 'conversation', characterId: null, conversationId: null, characterIndex: -1, chatIndex: -1 }])
    })

    test('reports a row the host renders again after an edit', async () => {
        const messages = range(0, 12).map((index) => makeMessage(index))
        const char = makeCharacter(messages)
        const conversation = char.chats[0]
        const session = new ActiveConversationSession({
            characterId: char.chaId,
            conversationId: conversation.id!,
            conversation,
            storeRevision: 1,
            measureMessage: () => 1,
        })
        const source = new SynchronousSessionConversationViewportSource({
            session,
            captureCurrent: () => ({ character: char, conversation }),
        })
        await mountChats(messages, char, { initialViewportSource: source })
        await listen()
        runFrames()
        events = []

        session.edit(session.locate(11), { ...messages[11], data: 'new last message' })
        await vi.waitFor(() => expect(chatMountProbe.displayUpdates.some((update) => update.message === 'new last message')).toBe(true))
        await settled()
        runFrames()
        expect(rowEvents()).toEqual([{
            type: 'rows', characterId: 'character-id', conversationId: 'chat-room-id', mounted: [], unmounted: [], rerendered: [viewRow(11)],
        }])
    })

    test.each([
        ['off', 'off'],
        ['off', 'recent'],
        ['balanced', 'recent'],
        ['strong', 'recent'],
    ] as const)('reports a streamed reply once when it is final (%s display, %s thoughts)', async (mode, thoughts) => {
        DBState.db.streamingThoughtMode = thoughts
        const messages = range(0, 8).map((index) => makeMessage(index, index === 7 ? { role: 'char' } : {}))
        const char = makeCharacter(messages, true)
        char.chats[0].activeStreamingDisplayOptimizationMode = mode
        await mountChats(messages, char)
        await listen()
        runFrames()
        expect(model.get(7)).toEqual({ index: 7, messageId: 'message-id-7', role: 'char' })
        events = []

        for (const chunk of ['first chunk', 'second chunk', 'third chunk']) {
            harness().updateMessage(7, chunk)
            await vi.waitFor(() => expect([
                ...chatMountProbe.streamingUpdates.map((update) => update.rawStreamingText),
                ...chatMountProbe.displayUpdates.map((update) => update.message),
                ...chatMountProbe.mounts.map((entry) => entry.message),
            ]).toContain(chunk))
            await settled()
            runFrames()
        }
        expect(rowEvents()).toEqual([])

        harness().setStreaming(false)
        for (let pass = 0; pass < 4; pass++) {
            await settled()
            runFrames()
        }
        expect(rowEvents()).toEqual([{
            type: 'rows', characterId: 'character-id', conversationId: 'chat-room-id',
            mounted: [], unmounted: [], rerendered: [{ index: 7, messageId: 'message-id-7', role: 'char' }],
        }])
    })
})
