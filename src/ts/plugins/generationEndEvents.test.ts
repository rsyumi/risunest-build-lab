import 'fake-indexeddb/auto'
import { describe, expect, it, vi } from 'vitest'
import type { GenerationEndRecord } from '../process/generationEnd'
import { projectResponseVariant } from '../responseVariants'
import type { Database, Message } from '../storage/database.svelte'
import { IndexedDbPersistentDataStore } from '../storage/indexedDbPersistentDataStore'
import {
    createGenerationEndEvents,
    createGenerationEndLocator,
    findGenerationEndMessage,
    type GenerationEndEvent,
    type GenerationEndLocation,
} from './generationEndEvents'

vi.mock('../storage/database.svelte', () => ({
    getDatabase: () => {
        throw new Error('No live database in generation end tests')
    },
    presetTemplate: {},
}))
vi.mock('../globalApi.svelte', () => ({ forageStorage: {} }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))

const record = (overrides: Partial<GenerationEndRecord> = {}): GenerationEndRecord => ({
    characterId: 'char-b',
    conversationId: 'chat-b2',
    status: 'completed',
    reroll: false,
    messageIds: ['gen-1'],
    ...overrides,
})

const message = (role: 'user' | 'char', chatId: string, extra: Partial<Message> = {}): Message =>
    ({ role, data: chatId, chatId, ...extra }) as Message

function rerolledTail(selectedId: string): Message[] {
    const variants = {
        groupId: 'group',
        selectedId,
        candidates: [
            { id: 'old', messages: [message('char', 'previous')] },
            { id: 'new', messages: [message('char', 'gen-1')] },
        ],
    }
    return [message('user', 'question'), ...projectResponseVariant(variants, selectedId)]
}

describe('findGenerationEndMessage', () => {
    it('finds the newest message the generation wrote', () => {
        const messages = [message('char', 'gen-1'), message('char', 'continued'), message('user', 'next'), message('char', 'error')]

        expect(findGenerationEndMessage(messages, record({ messageIds: ['gen-1', 'continued'] }))).toBe(1)
        expect(findGenerationEndMessage(messages, record({ status: 'failed', messageIds: ['gen-1', 'error'] }))).toBe(3)
        expect(findGenerationEndMessage(messages, record({ status: 'aborted', messageIds: ['missing'] }))).toBe(-1)
    })

    it('finds the projected carrier of a completed reroll and nothing for one that did not complete', () => {
        const projected = rerolledTail('new')
        expect(projected.at(-1)?.chatId).toBe('group')

        expect(findGenerationEndMessage(projected, record({ reroll: true }))).toBe(1)
        expect(findGenerationEndMessage(rerolledTail('old'), record({ reroll: true }))).toBe(-1)
        expect(findGenerationEndMessage([message('user', 'q'), message('char', 'gen-1')], record({ reroll: true, status: 'failed' }))).toBe(-1)
        expect(findGenerationEndMessage([...projected, message('char', 'comment', { isComment: true })], record({ reroll: true }))).toBe(1)
    })
})

async function storeWith(messages: Message[]) {
    const chat = (id: string, list: Message[] = [message('user', `${id}-first`)]) =>
        ({ id, name: id, note: '', localLore: [], message: list, scriptstate: {} })
    const database = {
        username: 'Fixture',
        characters: [
            { type: 'character', chaId: 'char-a', name: 'Alpha', chatPage: 0, chats: [chat('chat-a')] },
            { type: 'character', chaId: 'char-b', name: 'Beta', chatPage: 1, chats: [chat('chat-b1'), chat('chat-b2', messages)] },
        ],
    } as unknown as Database
    const store = new IndexedDbPersistentDataStore(`generation-end-${crypto.randomUUID()}`, indexedDB, IDBKeyRange)
    await store.open()
    await store.replaceFromDatabase(database)
    const flushPendingData = vi.fn(async () => {})
    return { locate: createGenerationEndLocator(() => ({ store, flushPendingData })), flushPendingData }
}

describe('createGenerationEndLocator', () => {
    it('reports the positions and absolute message index from the committed store after a flush', async () => {
        const messages = Array.from({ length: 100 }, (_, index) => message(index % 2 ? 'char' : 'user', `m-${index}`))
        messages[97] = message('char', 'gen-1')
        const { locate, flushPendingData } = await storeWith(messages)

        await expect(locate(record())).resolves.toEqual({ characterIndex: 1, chatIndex: 1, messageIndex: 97, messageId: 'gen-1' })
        expect(flushPendingData).toHaveBeenCalledWith('plugin-generation-end')
        await expect(locate(record({ messageIds: ['m-10'] }))).resolves.toEqual({ characterIndex: 1, chatIndex: 1, messageIndex: -1, messageId: null })
    })

    it('reports the projected reroll carrier by its group ID', async () => {
        const { locate } = await storeWith(rerolledTail('new'))

        await expect(locate(record({ reroll: true }))).resolves.toEqual({ characterIndex: 1, chatIndex: 1, messageIndex: 1, messageId: 'group' })
    })

    it('reports -1 for a conversation or character that is gone', async () => {
        const { locate } = await storeWith([message('char', 'gen-1')])

        await expect(locate(record({ conversationId: 'missing' }))).resolves.toEqual({ characterIndex: 1, chatIndex: -1, messageIndex: -1, messageId: null })
        await expect(locate(record({ characterId: 'missing' }))).resolves.toEqual({ characterIndex: -1, chatIndex: -1, messageIndex: -1, messageId: null })
    })
})

function setupEvents(locate: (record: GenerationEndRecord) => Promise<GenerationEndLocation>) {
    let emit: ((record: GenerationEndRecord) => void) | null = null
    const unsubscribe = vi.fn(() => { emit = null })
    const subscribe = vi.fn((listener: (record: GenerationEndRecord) => void) => {
        emit = listener
        return unsubscribe
    })
    let ids = 0
    const events = createGenerationEndEvents({ subscribe, locate: vi.fn(locate), createId: () => `end-${++ids}` })
    return { events, subscribe, unsubscribe, emit: (value: GenerationEndRecord) => emit?.(value) }
}

const located: GenerationEndLocation = { characterIndex: 1, chatIndex: 1, messageIndex: 5, messageId: 'gen-1' }

describe('createGenerationEndEvents', () => {
    it('subscribes while listeners exist and delivers a separate event to each', async () => {
        const { events, subscribe, unsubscribe, emit } = setupEvents(async () => located)
        const first = events.forOwner('first')
        const second = events.forOwner('second')
        const received: GenerationEndEvent[] = []
        const other: GenerationEndEvent[] = []
        expect(subscribe).not.toHaveBeenCalled()

        const { id } = first.register((event: GenerationEndEvent) => {
            received.push(event)
            event.messageIndex = 99
        })
        second.register((event: GenerationEndEvent) => other.push(event))
        expect(subscribe).toHaveBeenCalledOnce()
        emit(record({ status: 'failed' }))

        const expected = { status: 'failed', characterId: 'char-b', conversationId: 'chat-b2', ...located }
        await vi.waitFor(() => expect(other).toEqual([expected]))
        expect(received).toHaveLength(1)

        second.unregister(id)
        first.unregister(id)
        second.dispose()
        expect(unsubscribe).toHaveBeenCalledOnce()
    })

    it('delivers in order and still delivers when locating fails or a listener throws', async () => {
        const error = vi.spyOn(console, 'error').mockImplementation(() => {})
        let releaseFirst!: () => void
        const firstLocated = new Promise<void>((resolve) => { releaseFirst = resolve })
        const { events, emit } = setupEvents(async (value) => {
            if (value.messageIds[0] === 'slow') {
                await firstLocated
                throw new Error('store unavailable')
            }
            return located
        })
        const received: GenerationEndEvent[] = []
        const access = events.forOwner('plugin')
        access.register(() => { throw new Error('listener failure') })
        access.register(async (event: GenerationEndEvent) => { received.push(event) })

        emit(record({ status: 'aborted', messageIds: ['slow'] }))
        emit(record())
        releaseFirst()

        await vi.waitFor(() => expect(received).toHaveLength(2))
        expect(received[0]).toEqual({
            status: 'aborted', characterId: 'char-b', conversationId: 'chat-b2',
            characterIndex: -1, chatIndex: -1, messageIndex: -1, messageId: null,
        })
        expect(received[1].status).toBe('completed')
        expect(error).toHaveBeenCalledWith(expect.objectContaining({ message: 'store unavailable' }))
        error.mockRestore()
    })

    it('stops delivering to an owner after dispose', async () => {
        const { events, subscribe, emit } = setupEvents(async () => located)
        const received: GenerationEndEvent[] = []
        const access = events.forOwner('plugin')
        access.register((event: GenerationEndEvent) => received.push(event))
        access.dispose()

        emit(record())
        await Promise.resolve()

        expect(received).toEqual([])
        access.register((event: GenerationEndEvent) => received.push(event))
        expect(subscribe).toHaveBeenCalledTimes(2)
    })
})
