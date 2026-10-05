import { describe, expect, it, vi } from 'vitest'
import {
    createChatViewEvents,
    createPinnedChatViewConversation,
    type ChatViewConversation,
    type ChatViewEvent,
    type ChatViewRowReport,
} from './chatViewEvents'

function setup() {
    let conversation: ChatViewConversation | null = { characterId: 'char-a', conversationId: 'conv-a', characterIndex: 0, chatIndex: 0 }
    const frames: Array<() => void> = []
    let notify: (() => void) | null = null
    const stopWatching = vi.fn(() => { notify = null })
    let nextId = 0
    const events = createChatViewEvents({
        readConversation: () => conversation,
        watchConversation: (onChange) => {
            notify = onChange
            return stopWatching
        },
        requestFrame: (callback) => { frames.push(callback) },
        createId: () => `listener-${++nextId}`,
    })
    const received: ChatViewEvent[] = []
    return {
        events,
        frames,
        received,
        stopWatching,
        reporter: events.createReporter(),
        listen(owner = 'plugin') {
            return events.forOwner(owner).register((event) => { received.push(event) })
        },
        drain() {
            for (const frame of frames.splice(0)) frame()
            return received.splice(0)
        },
        select(next: ChatViewConversation | null) {
            conversation = next
            notify?.()
        },
    }
}

const report = (index: number, overrides: Partial<ChatViewRowReport> = {}): ChatViewRowReport => ({
    characterId: 'char-a',
    conversationId: 'conv-a',
    index,
    message: { role: index % 2 ? 'user' : 'char', chatId: `m-${index}` },
    streaming: false,
    ...overrides,
})
const row = (index: number, messageId: string | null = `m-${index}`) => ({ index, messageId, role: index % 2 ? 'user' : 'char' })
const rows = (mounted: unknown[] = [], unmounted: unknown[] = [], rerendered: unknown[] = []) =>
    ({ type: 'rows', characterId: 'char-a', conversationId: 'conv-a', mounted, unmounted, rerendered })
const conversationA = { type: 'conversation', characterId: 'char-a', conversationId: 'conv-a', characterIndex: 0, chatIndex: 0 }

describe('chat view events', () => {
    it('reports the conversation and every mounted row on registration, once per frame', () => {
        const { reporter, listen, drain, frames, received } = setup()
        reporter.rendered('k1', report(1))
        reporter.rendered('k2', report(2, { message: { role: 'char' } }))
        reporter.rendered('greeting', report(-1, { message: { role: 'system' } }))
        expect(frames).toHaveLength(0)

        expect(listen()).toEqual({ id: 'listener-1' })
        expect(received).toEqual([])
        expect(frames).toHaveLength(1)
        expect(drain()).toEqual([conversationA, rows([row(1), row(2, null)])])
        expect(drain()).toEqual([])
    })

    it('coalesces a frame into its net changes', () => {
        const { reporter, listen, drain, frames } = setup()
        listen()
        reporter.rendered('k1', report(1))
        reporter.rendered('k2', report(2))
        reporter.rendered('k3', report(3))
        drain()

        reporter.removed('k1')
        reporter.rendered('k4', report(4))
        reporter.removed('k4')
        reporter.rendered('k2', report(2))
        reporter.rendered('k2', report(2))
        reporter.removed('k3')
        reporter.rendered('k3', report(3))
        reporter.updated('k5', report(5))
        expect(frames).toHaveLength(1)
        expect(drain()).toEqual([rows([], [row(1)], [row(2), row(3)])])

        reporter.updated('k2', report(2))
        expect(drain()).toEqual([])
    })

    it('reports a row that rendered its content again by itself, unless its reply is streaming', () => {
        const { reporter, listen, drain } = setup()
        reporter.contentRendered('k1')
        listen()
        reporter.rendered('k1', report(1))
        reporter.rendered('tail', report(2, { streaming: true }))
        drain()

        reporter.contentRendered('k1')
        reporter.contentRendered('tail')
        reporter.contentRendered('unknown')
        expect(drain()).toEqual([rows([], [], [row(1)])])
    })

    it('reports a row that changed its index as unmounted and mounted again', () => {
        const { reporter, listen, drain } = setup()
        listen()
        reporter.rendered('k1', report(1))
        reporter.rendered('k2', report(2))
        drain()

        reporter.removed('k1')
        reporter.updated('k2', report(1, { message: { role: 'char', chatId: 'm-2' } }))
        expect(drain()).toEqual([rows(
            [{ index: 1, messageId: 'm-2', role: 'char' }],
            [row(1), row(2)],
        )])
    })

    it('follows a row that keeps its DOM under a new key without reporting it', () => {
        const { reporter, listen, drain } = setup()
        listen()
        reporter.rendered('old', report(1))
        drain()

        reporter.moved('old', 'new')
        reporter.updated('new', report(1))
        expect(drain()).toEqual([])
        reporter.rendered('new', report(1))
        expect(drain()).toEqual([rows([], [], [row(1)])])
        reporter.removed('new')
        expect(drain()).toEqual([rows([], [row(1)])])
    })

    it.each(['updated', 'rendered'] as const)('reports a streamed reply once when it is final (%s)', (completion) => {
        const { reporter, listen, drain } = setup()
        listen()
        reporter.rendered('k1', report(0))
        reporter.rendered('tail', report(2, { streaming: true }))
        expect(drain()).toEqual([conversationA, rows([row(0), row(2)])])

        for (let token = 0; token < 3; token++) {
            reporter.rendered('tail', report(2, { streaming: true }))
            reporter.updated('tail', report(2, { streaming: true }))
            expect(drain()).toEqual([])
        }
        reporter[completion]('tail', report(2))
        expect(drain()).toEqual([rows([], [], [row(2)])])
        reporter.updated('tail', report(2))
        expect(drain()).toEqual([])
    })

    it('starts over on a conversation switch and reports only the selected conversation', () => {
        const { reporter, listen, drain, select } = setup()
        listen()
        reporter.rendered('a1', report(1))
        drain()

        select({ characterId: 'char-b', conversationId: 'conv-b', characterIndex: 3, chatIndex: 1 })
        reporter.removed('a1')
        reporter.rendered('b1', report(1, { characterId: 'char-b', conversationId: 'conv-b' }))
        reporter.rendered('stale', report(5))
        expect(drain()).toEqual([
            { type: 'conversation', characterId: 'char-b', conversationId: 'conv-b', characterIndex: 3, chatIndex: 1 },
            { type: 'rows', characterId: 'char-b', conversationId: 'conv-b', mounted: [row(1)], unmounted: [], rerendered: [] },
        ])

        select({ characterId: 'char-b', conversationId: null, characterIndex: 3, chatIndex: -1 })
        expect(drain()).toEqual([
            { type: 'conversation', characterId: 'char-b', conversationId: null, characterIndex: 3, chatIndex: -1 },
        ])
        reporter.rendered('b2', report(2, { characterId: 'char-b', conversationId: undefined }))
        expect(drain()).toEqual([])
    })

    it('reports nothing while the selected conversation is still being resolved', () => {
        const { reporter, listen, drain, select, frames } = setup()
        listen()
        reporter.rendered('a1', report(1))
        drain()

        select(null)
        reporter.removed('a1')
        reporter.rendered('b1', report(1, { characterId: 'char-b', conversationId: 'conv-b' }))
        expect(drain()).toEqual([])
        expect(frames).toHaveLength(0)
        select({ characterId: 'char-b', conversationId: 'conv-b', characterIndex: 0, chatIndex: 1 })
        expect(drain()).toEqual([
            { type: 'conversation', characterId: 'char-b', conversationId: 'conv-b', characterIndex: 0, chatIndex: 1 },
            { type: 'rows', characterId: 'char-b', conversationId: 'conv-b', mounted: [row(1)], unmounted: [], rerendered: [] },
        ])
    })

    it('keeps each listener and reporter apart', async () => {
        const { events, reporter, listen, drain, frames, stopWatching } = setup()
        const first = listen('plugin-a')
        reporter.rendered('k1', report(1))
        drain()

        const other = events.createReporter()
        other.rendered('k1', report(7))
        const late: ChatViewEvent[] = []
        events.forOwner('plugin-b').register((event) => { late.push(event) })
        expect(drain()).toEqual([rows([row(7)])])
        for (const frame of frames.splice(0)) frame()
        expect(late).toEqual([conversationA, rows([row(1), row(7)])])

        events.forOwner('plugin-b').unregister(first.id)
        other.dispose()
        other.rendered('k2', report(2))
        expect(drain()).toEqual([rows([], [row(7)])])

        events.forOwner('plugin-a').dispose()
        reporter.rendered('k3', report(3))
        expect(drain()).toEqual([])
        expect(late.at(-1)).toEqual(rows([row(3)]))
        expect(stopWatching).not.toHaveBeenCalled()
        events.forOwner('plugin-b').dispose()
        expect(stopWatching).toHaveBeenCalledTimes(1)
    })

    it('logs a failing callback and still delivers to the others', async () => {
        const { events, reporter, drain, received } = setup()
        const error = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        try {
            events.forOwner('throws').register(() => { throw new Error('sync') })
            events.forOwner('rejects').register(async () => { throw new Error('async') })
            events.forOwner('plugin').register((event) => { received.push(event) })
            reporter.rendered('k1', report(1))
            expect(drain()).toEqual([conversationA, rows([row(1)])])
            await Promise.resolve()
            await Promise.resolve()
            expect(error).toHaveBeenCalledTimes(4)
        } finally {
            error.mockRestore()
        }
    })
})

describe('pinned chat view conversation', () => {
    type Position = { characterIndex: number; chatIndex: number }
    const drainContinuations = () => new Promise((resolve) => setTimeout(resolve, 0))

    function setup() {
        let selection: ChatViewConversation = { characterId: 'char-b', conversationId: 'conv-b', characterIndex: 1, chatIndex: 0 }
        let notify: (() => void) | null = null
        const pending: Array<{ args: [string, string | null]; resolve(position: Position): void; reject(error: unknown): void }> = []
        const stop = vi.fn(() => { notify = null })
        const source = createPinnedChatViewConversation({
            readSelection: () => selection,
            watchSelection: (onChange) => {
                notify = onChange
                return stop
            },
            resolvePosition: (characterId, conversationId) => new Promise<Position>((resolve, reject) => {
                pending.push({ args: [characterId, conversationId], resolve, reject })
            }),
        })
        const changed = vi.fn()
        return {
            source, pending, stop, changed,
            watch: () => source.watchConversation(changed),
            select(next: ChatViewConversation) {
                selection = next
                notify?.()
            },
        }
    }

    it('reports the selection at its resolved position and drops a superseded resolution', async () => {
        const { source, pending, changed, watch, select } = setup()
        watch()
        expect(source.readConversation()).toBeNull()
        expect(pending.map((entry) => entry.args)).toEqual([['char-b', 'conv-b']])

        select({ characterId: 'char-c', conversationId: 'conv-c', characterIndex: 2, chatIndex: 3 })
        pending[0].resolve({ characterIndex: 0, chatIndex: 0 })
        await drainContinuations()
        expect(source.readConversation()).toBeNull()
        expect(changed).not.toHaveBeenCalled()

        pending[1].resolve({ characterIndex: 1, chatIndex: 2 })
        await drainContinuations()
        expect(changed).toHaveBeenCalledOnce()
        expect(source.readConversation()).toEqual({ characterId: 'char-c', conversationId: 'conv-c', characterIndex: 1, chatIndex: 2 })

        select({ characterId: 'char-c', conversationId: 'conv-c', characterIndex: 2, chatIndex: 3 })
        expect(pending).toHaveLength(2)
    })

    it('reports an empty selection at once and an unresolved one at -1', async () => {
        const { source, pending, changed, watch, select } = setup()
        const error = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        try {
            watch()
            select({ characterId: null, conversationId: null, characterIndex: -1, chatIndex: -1 })
            expect(changed).toHaveBeenCalledOnce()
            expect(source.readConversation()).toEqual({ characterId: null, conversationId: null, characterIndex: -1, chatIndex: -1 })

            select({ characterId: 'char-d', conversationId: null, characterIndex: 4, chatIndex: -1 })
            pending[1].reject(new Error('store closed'))
            await drainContinuations()
            expect(source.readConversation()).toEqual({ characterId: 'char-d', conversationId: null, characterIndex: -1, chatIndex: -1 })
            expect(error).toHaveBeenCalledOnce()
        } finally {
            error.mockRestore()
        }
    })

    it('resolves again after watching restarts', async () => {
        const { source, pending, stop, watch } = setup()
        const stopWatching = watch()
        pending[0].resolve({ characterIndex: 0, chatIndex: 0 })
        await drainContinuations()
        stopWatching()
        expect(stop).toHaveBeenCalledOnce()
        expect(source.readConversation()).toBeNull()

        watch()
        expect(pending).toHaveLength(2)
    })
})
