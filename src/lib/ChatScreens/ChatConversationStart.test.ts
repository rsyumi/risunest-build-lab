// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'
import { mount, unmount } from 'svelte'
import { createSelectedConversationLiveParserProjectionResolver, type LiveChatParserConversationStartRequest, type BoundedLiveChatParserProjection } from 'src/ts/selectedConversationLiveParserProjection'
import { PersistentConversationViewportSource } from 'src/ts/conversationViewportSource'
import type { ProcessScriptCaptureContext } from 'src/ts/process/scripts'
import type { character } from 'src/ts/storage/database.svelte'

vi.mock('./Chat.svelte', async () => ({
    default: (await import('./ChatMountProbe.test.svelte')).default,
}))
vi.mock('./CreatorQuote.svelte', async () => ({
    default: (await import('./ChatMountProbe.test.svelte')).default,
}))

import ChatConversationStart from './ChatConversationStart.svelte'
import { chatMountProbe, resetChatMountProbe } from './chatMountProbe.testSupport'

function metadataOnlyCharacter(): character {
    const conversation = { id: 'chat-id', fmIndex: -1 } as character['chats'][number]
    Object.defineProperty(conversation, 'message', {
        get() {
            throw new Error('metadata-only conversation body was accessed')
        },
    })
    return {
        type: 'character',
        name: 'Character',
        chaId: 'character-id',
        chatPage: 0,
        chats: [conversation],
        firstMessage: 'Greeting',
        alternateGreetings: [],
        creatorNotes: '',
        removedQuotes: false,
        customscript: [],
        additionalAssets: [],
        emotionImages: [],
        triggerscript: [],
    } as unknown as character
}

describe('ChatConversationStart', () => {
    let target: HTMLDivElement
    let mounted: ReturnType<typeof mount> | undefined

    beforeEach(() => {
        resetChatMountProbe()
        target = document.createElement('div')
        document.body.append(target)
    })

    afterEach(async () => {
        if (mounted) await unmount(mounted)
        document.body.replaceChildren()
    })

    test('uses the supplied total count for the empty warning without reading the shell body', () => {
        mounted = mount(ChatConversationStart, {
            target,
            props: {
                currentCharacter: metadataOnlyCharacter(),
                resolvedImage: '',
                showAiWarning: true,
                totalMessages: 0,
                onReroll: () => {},
                unReroll: () => {},
                onRemoveCreatorQuote: () => {},
            },
        })

        expect(target.querySelector('.italic')).not.toBeNull()
    })

    test('applies a new message count on refresh without remounting the greeting', async () => {
        const acquire = vi.fn(async (_request: { totalMessages: number }) => null)
        mounted = mount(ChatConversationStart, {
            target,
            props: {
                currentCharacter: metadataOnlyCharacter(),
                resolvedImage: '',
                showAiWarning: true,
                totalMessages: 0,
                onReroll: () => {},
                unReroll: () => {},
                onRemoveCreatorQuote: () => {},
                acquireConversationStartParserLease: acquire,
            },
        })
        await vi.waitFor(() => expect(chatMountProbe.mounts).toHaveLength(1))
        const greeting = target.querySelector('[data-chat-probe]')
        ;(
            mounted as { refreshConversationStartParser(totalMessages?: number): void }
        ).refreshConversationStartParser(1)
        await vi.waitFor(() => expect(acquire).toHaveBeenCalledTimes(2))
        await Promise.resolve()
        expect(acquire.mock.calls[1][0].totalMessages).toBe(1)
        expect(target.querySelector('.italic')).toBeNull()
        expect(target.querySelector('[data-chat-probe]')).toBe(greeting)
        expect(chatMountProbe.unmounts).toHaveLength(0)
    })

    test.each(['reload', 'stale history'])('keeps the greeting mounted while a %s prepares its replacement parser lease', async (trigger) => {
        const release = vi.fn()
        let finishReload!: (value: null) => void
        const acquire = vi
            .fn()
            .mockResolvedValueOnce({ release })
            .mockImplementationOnce(
                () =>
                    new Promise((resolve) => {
                        finishReload = resolve
                    }),
            )
        mounted = mount(ChatConversationStart, {
            target,
            props: {
                currentCharacter: metadataOnlyCharacter(),
                resolvedImage: '',
                showAiWarning: false,
                totalMessages: 0,
                onReroll: () => {},
                unReroll: () => {},
                onRemoveCreatorQuote: () => {},
                acquireConversationStartParserLease: acquire,
            },
        })
        await vi.waitFor(() => expect(chatMountProbe.mounts).toHaveLength(1))
        const greeting = target.querySelector('[data-chat-probe]')
        if (trigger === 'reload') {
            ;(
                mounted as { refreshConversationStartParser(): void }
            ).refreshConversationStartParser()
        } else acquire.mock.calls[0][0].onStale()
        await vi.waitFor(() => expect(acquire).toHaveBeenCalledTimes(2))
        // A replaced lease cannot restart preparation.
        acquire.mock.calls[0][0].onStale()
        expect(acquire).toHaveBeenCalledTimes(2)
        expect(release).toHaveBeenCalledOnce()
        expect(chatMountProbe.mounts[0].parserAbortSignal?.aborted).toBe(true)
        expect(target.querySelector('[data-chat-probe]')).toBe(greeting)
        expect(chatMountProbe.unmounts).toHaveLength(0)
        finishReload(null)
        await Promise.resolve()
        expect(target.querySelector('[data-chat-probe]')).toBe(greeting)
        expect(chatMountProbe.mounts).toHaveLength(1)
    })

    test('prepares the greeting once on its next pass after a silent revision advance', async () => {
        const currentCharacter = metadataOnlyCharacter()
        const messages = [{ role: 'user' as const, data: 'stored history' }]
        let revision = 1
        const readConversationWindow = async () => ({ revision, value: {
            characterId: 'character-id', conversationId: 'chat-id', startIndex: 0, endIndex: 1,
            totalMessages: 1, messages, hasMoreBefore: false, hasMoreAfter: false,
        } })
        const source = new PersistentConversationViewportSource({ reader: { readConversationWindow },
            characterId: 'character-id', conversationId: 'chat-id', revision, totalMessages: 1, rowBudget: 8 })
        const seedCharacter = { ...currentCharacter, chats: [{ id: 'chat-id', message: [] }] }
        const resolver = createSelectedConversationLiveParserProjectionResolver({
            runtime: {
                store: { readConversationWindow } as any,
                captureSelectedConversationTarget: () => ({ characterId: 'character-id', conversationId: 'chat-id', navigationGeneration: 0, storeRevision: revision }) as any,
                captureSelectedConversationAuthority: () => ({ storeRevision: revision, totalMessages: 1 }) as any,
                getActiveConversationViewportSource: () => source,
                acquireCompleteConversation: async () => { throw new Error('Unexpected promotion') },
            },
            captureCurrent: () => ({ character: currentCharacter, conversation: currentCharacter.chats[0] }),
            createBoundedContextSeed: () => ({ presetRegex: [], moduleRegexScripts: [], moduleAssets: [],
                parserContext: { database: { characters: [seedCharacter] }, character: seedCharacter,
                    selectedCharID: 0, chatVariables: {}, globalChatVariables: {}, modules: [], moduleLorebooks: [] },
            }) as unknown as ProcessScriptCaptureContext,
            createCompleteContext: () => { throw new Error('Unexpected promotion') },
            parserSource: () => '', unsafeDependencies: () => [], maxProjectionMessages: 4,
        })
        let projection: BoundedLiveChatParserProjection | undefined
        const acquire = vi.fn(async (request: LiveChatParserConversationStartRequest) => {
            const lease = await resolver.acquireConversationStart!(request)
            projection = lease?.projection
            return lease
        })
        mounted = mount(ChatConversationStart, { target, props: { currentCharacter, resolvedImage: '',
            showAiWarning: false, totalMessages: 1, historySnapshot: source.snapshot(),
            onReroll: () => {}, unReroll: () => {}, onRemoveCreatorQuote: () => {},
            acquireConversationStartParserLease: acquire } })
        await vi.waitFor(() => expect(chatMountProbe.mounts).toHaveLength(1))
        const greeting = target.querySelector('[data-chat-probe]')
        const retained = projection!
        const oldSignal = acquire.mock.calls[0][0].signal
        revision = 2
        source.advanceUnchangedRevision(revision)
        ;(mounted as { refreshConversationStartParser(total: number, snapshot: ReturnType<typeof source.snapshot>): void })
            .refreshConversationStartParser(1, source.snapshot())
        expect(acquire).toHaveBeenCalledOnce()
        await expect(retained.context.parserContext.prepareHistory!('{{lastmessage}}', -1, false)).rejects.toThrow()
        await vi.waitFor(() => expect(projection).not.toBe(retained))
        expect(acquire).toHaveBeenCalledTimes(2)
        expect(oldSignal?.aborted).toBe(true)
        expect((await projection!.context.parserContext.prepareHistory!('{{lastmessage}}', -1, false)).row(0)?.data).toBe('stored history')
        expect(target.querySelector('[data-chat-probe]')).toBe(greeting)
        expect(chatMountProbe.unmounts).toHaveLength(0)
        expect(target.querySelector('button')).toBeNull()
        source.dispose()
    })

    test('aborts the greeting Chat signal before releasing its complete lease', async () => {
        let acquiredSignal: AbortSignal | undefined
        const abortedAtRelease: boolean[] = []
        const release = vi.fn(() =>
            abortedAtRelease.push(acquiredSignal?.aborted ?? false),
        )
        mounted = mount(ChatConversationStart, {
            target,
            props: {
                currentCharacter: metadataOnlyCharacter(),
                resolvedImage: '',
                showAiWarning: false,
                totalMessages: 0,
                onReroll: () => {},
                unReroll: () => {},
                onRemoveCreatorQuote: () => {},
                acquireConversationStartParserLease: async ({ signal }) => {
                    acquiredSignal = signal
                    return { release }
                },
            },
        })
        await vi.waitFor(() => expect(chatMountProbe.mounts).toHaveLength(1))
        const chatSignal = chatMountProbe.mounts[0].parserAbortSignal
        expect(chatSignal).toBeDefined()
        expect(chatSignal).toBe(acquiredSignal)
        expect(chatSignal?.aborted).toBe(false)
        await unmount(mounted)
        mounted = undefined
        expect(chatSignal?.aborted).toBe(true)
        expect(release).toHaveBeenCalledOnce()
        expect(abortedAtRelease).toEqual([true])
    })

})
