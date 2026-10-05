import { describe, expect, test, vi } from 'vitest'
import { PersistentConversationViewportSource, type ConversationViewportRow } from './conversationViewportSource'
import type { ProcessScriptCaptureContext } from './process/scripts'
import {
    bindCompleteLiveParserContextAuthority,
    collectLiveChatParserUnsafeDependencies,
    createSelectedConversationLiveParserProjectionResolver,
    type SelectedConversationLiveParserProjectionDependencies,
} from './selectedConversationLiveParserProjection'
import type { Chat, character, Database, Message } from './storage/database.svelte'
import { createConversationSessionToken } from './storage/activeConversationSession'

const CHARACTER_ID = 'character-id'
const CONVERSATION_ID = 'conversation-id'

function message(index: number, data = `message-${index}`): Message {
    return {
        role: index % 2 === 0 ? 'char' : 'user',
        data,
        chatId: `message-${index}`,
    }
}

function conversation(messages: Message[]): Chat {
    return {
        id: CONVERSATION_ID,
        message: messages,
    } as Chat
}

function owner(chat: Chat): character {
    return {
        type: 'character',
        chaId: CHARACTER_ID,
        name: 'Character',
        chatPage: 0,
        chats: [chat],
        customscript: [],
        triggerscript: [],
        additionalAssets: [],
        emotionImages: [],
    } as unknown as character
}

function context(currentCharacter: character): ProcessScriptCaptureContext {
    return {
        presetRegex: [],
        moduleRegexScripts: [],
        moduleAssets: [],
        dynamicAssets: false,
        dynamicAssetsEditDisplay: false,
        parserContext: {
            database: { characters: [currentCharacter] } as Database,
            character: currentCharacter,
            userName: 'User',
            personaPrompt: '',
            modules: [],
            moduleLorebooks: [],
            selectedCharID: 0,
            chatVariables: {},
            globalChatVariables: {},
            currentTime: 1,
        },
    }
}

function row(index: number, value: Message): ConversationViewportRow {
    return {
        key: `row-${index}` as ConversationViewportRow['key'],
        absoluteIndex: index,
        message: value,
        sourceVersion: 0,
    }
}

function harness(options: {
    messages?: Message[]
    unsafeDependencies?: SelectedConversationLiveParserProjectionDependencies['unsafeDependencies']
    onAcquire?: () => void
    metadataOnly?: boolean
    advanceRevisionDuringAcquire?: boolean
    navigateDuringAcquire?: boolean
    maxProjectionMessages?: number
} = {}) {
    const messages = options.messages ?? Array.from({ length: 8 }, (_, index) => message(index))
    let currentChat = conversation([])
    let forbiddenHistoryReads = 0
    if (options.metadataOnly) {
        Object.defineProperty(currentChat, 'message', {
            get() {
                forbiddenHistoryReads += 1
                throw new Error('Metadata-only history must not be read')
            },
            enumerable: false,
        })
    }
    let currentCharacter = owner(currentChat)
    let windowed = true
    let releases = 0
    const boundedContextMessageCounts: number[] = []
    const completeContextMessageCounts: number[] = []
    const target = {
        characterId: CHARACTER_ID,
        conversationId: CONVERSATION_ID,
        navigationGeneration: 1,
        storeRevision: 4,
    } as any
    const dependencies: SelectedConversationLiveParserProjectionDependencies = {
        runtime: {
            store: {
                async readConversationWindow({ characterId, conversationId, startIndex, limit }) {
                    const selected = messages.slice(startIndex, startIndex + limit)
                    return {
                        revision: 4,
                        value: {
                            characterId,
                            conversationId,
                            startIndex,
                            endIndex: startIndex + selected.length,
                            totalMessages: messages.length,
                            messages: selected,
                            hasMoreBefore: startIndex > 0,
                            hasMoreAfter: startIndex + selected.length < messages.length,
                        },
                    }
                },
            } as any,
            captureSelectedConversationTarget: () => ({ ...target }),
            captureSelectedConversationAuthority: () => windowed
                ? {
                    kind: 'windowed',
                    characterId: CHARACTER_ID,
                    conversationId: CONVERSATION_ID,
                    storeRevision: 4,
                    totalMessages: messages.length,
                    sessionToken: 'session',
                    persistedSessionVersion: 0,
                    sessionVersion: 0,
                } as any
                : null,
            acquireCompleteConversation: vi.fn(async () => {
                options.onAcquire?.()
                if (options.advanceRevisionDuringAcquire) target.storeRevision += 1
                if (options.navigateDuringAcquire) target.navigationGeneration += 1
                windowed = false
                currentChat = conversation(messages)
                currentCharacter = owner(currentChat)
                return {
                    reason: 'live-render',
                    target: { ...target },
                    session: {
                        totalMessages: messages.length,
                        storeRevision: target.storeRevision,
                        isActive: true,
                        matchesConversation: (characterId: string, chat: Chat) => (
                            characterId === CHARACTER_ID && chat === currentChat
                        ),
                        locate: (absoluteIndex: number) => ({ absoluteIndex }),
                        readMessage: (locator: { absoluteIndex: number }) => messages[locator.absoluteIndex],
                    } as any,
                    release: () => releases += 1,
                }
            }),
        },
        captureCurrent: () => ({ character: currentCharacter, conversation: currentChat }),
        createBoundedContextSeed: ({ character }) => {
            boundedContextMessageCounts.push(character.chats[character.chatPage].message.length)
            return context(character as character)
        },
        createCompleteContext: ({ character }) => {
            completeContextMessageCounts.push(character.chats[character.chatPage].message.length)
            return context(character as character)
        },
        parserSource: () => ({ guiHTML: '' }),
        unsafeDependencies: options.unsafeDependencies ?? (() => []),
        maxProjectionMessages: options.maxProjectionMessages ?? 6,
    }
    return {
        resolver: createSelectedConversationLiveParserProjectionResolver(dependencies),
        dependencies,
        messages,
        acquire: dependencies.runtime.acquireCompleteConversation,
        releases: () => releases,
        boundedContextMessageCounts,
        completeContextMessageCounts,
        forbiddenHistoryReads: () => forbiddenHistoryReads,
    }
}

async function cachedHarness(options: Parameters<typeof harness>[0] = {}, startIndex = 2, limit = 6) {
    const result = harness(options)
    const source = new PersistentConversationViewportSource({
        reader: result.dependencies.runtime.store,
        characterId: CHARACTER_ID,
        conversationId: CONVERSATION_ID,
        revision: 4,
        totalMessages: result.messages.length,
        rowBudget: 64,
    })
    result.dependencies.runtime.getActiveConversationViewportSource = () => source
    await source.ensureRange({ startIndex, limit, reason: 'viewport' })
    const readWindow = result.dependencies.runtime.store.readConversationWindow.bind(result.dependencies.runtime.store)
    const reads = vi.spyOn(result.dependencies.runtime.store, 'readConversationWindow')
    const releases = vi.fn()
    const acquirePin = source.acquireRangePin.bind(source)
    vi.spyOn(source, 'acquireRangePin').mockImplementation((...args) => {
        const pin = acquirePin(...args)
        return { release: () => { releases(); pin.release() } }
    })
    return { ...result, source, reads, readWindow, pinReleases: releases }
}

describe('selected conversation live parser projection', () => {
    test('reuses leased persisted viewport rows without store reads and isolates parser mutations', async () => {
        const testHarness = await cachedHarness()
        const result = await testHarness.resolver.resolve({
            row: testHarness.source.snapshot().rowAt(7)!,
            totalMessages: 8,
        })
        expect(result.kind).toBe('bounded')
        if (result.kind !== 'bounded') throw new Error('Expected bounded projection')
        expect(result.messages).toEqual(testHarness.messages.slice(3))
        expect(testHarness.reads).not.toHaveBeenCalled()
        expect(testHarness.pinReleases).toHaveBeenCalledOnce()
        result.context.parserContext.character.chats[0].message[0].data = 'parser side effect'
        expect(testHarness.source.snapshot().rowAt(3)!.message.data).toBe('message-3')
    })

    test('reads only gaps around reusable rows and preserves literal dependencies outside the viewport', async () => {
        const messages = Array.from({ length: 8 }, (_, index) => message(index))
        messages[7].data = '{{previouschatlog::0}}'
        const testHarness = await cachedHarness({ messages, maxProjectionMessages: 8 }, 5, 3)
        const result = await testHarness.resolver.resolve({
            row: testHarness.source.snapshot().rowAt(7)!, totalMessages: 8,
        })
        expect(result).toMatchObject({ kind: 'bounded', historyOffset: 0 })
        if (result.kind !== 'bounded') throw new Error('Expected bounded projection')
        expect(result.messages).toEqual(messages)
        expect(testHarness.reads).toHaveBeenCalledExactlyOnceWith({
            characterId: CHARACTER_ID, conversationId: CONVERSATION_ID, startIndex: 0, limit: 5,
        })
        expect(testHarness.pinReleases).toHaveBeenCalledOnce()
    })

    test('does not reuse optimistic viewport rows whose session has pending persistence', async () => {
        const testHarness = await cachedHarness()
        const capture = testHarness.dependencies.runtime.captureSelectedConversationAuthority
        testHarness.dependencies.runtime.captureSelectedConversationAuthority = () => ({
            ...capture()!, sessionVersion: 1,
        })
        testHarness.source.applyOptimisticRange(3, 1, [message(3, 'pending edit')])
        const result = await testHarness.resolver.resolve({
            row: testHarness.source.snapshot().rowAt(7)!, totalMessages: 8,
        })
        expect(result.kind).toBe('bounded')
        if (result.kind !== 'bounded') throw new Error('Expected bounded projection')
        expect(result.messages[0].data).toBe('message-3')
        expect(testHarness.reads).toHaveBeenCalledTimes(2)
        expect(testHarness.pinReleases).not.toHaveBeenCalled()
    })

    test('does not reuse rows from a replaced viewport revision', async () => {
        const testHarness = await cachedHarness()
        const selectedRow = testHarness.source.snapshot().rowAt(7)!
        testHarness.source.advanceRevision(5, 8)
        const result = await testHarness.resolver.resolve({ row: selectedRow, totalMessages: 8 })
        expect(result.kind).toBe('bounded')
        expect(testHarness.reads).toHaveBeenCalledTimes(2)
        expect(testHarness.pinReleases).not.toHaveBeenCalled()
    })

    test('retains completed projections and captures a new revision when unchanged viewport rows advance', async () => {
        const testHarness = await cachedHarness()
        const runtime = testHarness.dependencies.runtime
        const captureTarget = runtime.captureSelectedConversationTarget
        const captureAuthority = runtime.captureSelectedConversationAuthority
        const snapshot = testHarness.source.snapshot.bind(testHarness.source)
        let revision = 4
        runtime.captureSelectedConversationTarget = () => ({ ...captureTarget()!, storeRevision: revision })
        runtime.captureSelectedConversationAuthority = () => ({ ...captureAuthority()!, storeRevision: revision })
        // Model the storage-only seam, which preserves the source, rows, keys and render version.
        vi.spyOn(testHarness.source, 'snapshot').mockImplementation(() => ({ ...snapshot(), storeRevision: revision }))
        const selectedRow = testHarness.source.snapshot().rowAt(7)!
        const retained = await testHarness.resolver.resolve({ row: selectedRow, totalMessages: 8 })
        revision = 5
        const fresh = await testHarness.resolver.resolve({ row: selectedRow, totalMessages: 8 })

        expect(retained).toMatchObject({ kind: 'bounded', revision: 4 })
        expect(fresh).toMatchObject({ kind: 'bounded', revision: 5 })
        if (retained.kind !== 'bounded' || fresh.kind !== 'bounded') throw new Error('Expected bounded projections')
        expect(fresh.messages).toEqual(retained.messages)
        expect(testHarness.reads).not.toHaveBeenCalled()
        expect(testHarness.pinReleases).toHaveBeenCalledTimes(2)
    })

    test('fences an in-flight gap read after an unchanged revision advance and renews at the new revision', async () => {
        const testHarness = await cachedHarness({}, 7, 1)
        const runtime = testHarness.dependencies.runtime
        const captureTarget = runtime.captureSelectedConversationTarget
        const captureAuthority = runtime.captureSelectedConversationAuthority
        const snapshot = testHarness.source.snapshot.bind(testHarness.source)
        let revision = 4
        runtime.captureSelectedConversationTarget = () => ({ ...captureTarget()!, storeRevision: revision })
        runtime.captureSelectedConversationAuthority = () => ({ ...captureAuthority()!, storeRevision: revision })
        vi.spyOn(testHarness.source, 'snapshot').mockImplementation(() => ({ ...snapshot(), storeRevision: revision }))
        testHarness.reads.mockImplementation(async (query) => {
            const result = await testHarness.readWindow(query)
            revision = 5
            return result && { ...result, revision }
        })
        const selectedRow = testHarness.source.snapshot().rowAt(7)!

        await expect(testHarness.resolver.resolve({ row: selectedRow, totalMessages: 8 }))
            .rejects.toMatchObject({ name: 'ChatParserHistoryProjectionStaleError' })
        const fresh = await testHarness.resolver.resolve({ row: selectedRow, totalMessages: 8 })
        expect(fresh).toMatchObject({ kind: 'bounded', revision: 5 })
        if (fresh.kind !== 'bounded') throw new Error('Expected bounded projection')
        expect(fresh.messages).toEqual(testHarness.messages.slice(3))
        expect(testHarness.pinReleases).toHaveBeenCalledTimes(2)
    })

    test.each(['navigation', 'mutation', 'abort', 'source', 'revision', 'session'])(
        'rejects cached projections after %s during a missing-range read and releases the pin',
        async (change) => {
            const testHarness = await cachedHarness({}, 7, 1)
            const controller = new AbortController()
            let current = true
            testHarness.reads.mockImplementation(async (query) => {
                const result = await testHarness.readWindow(query)
                if (change === 'navigation') current = false
                if (change === 'abort') controller.abort()
                if (change === 'source') testHarness.dependencies.runtime.getActiveConversationViewportSource = () => null
                if (change === 'revision') testHarness.source.advanceRevision(5, 8)
                if (change === 'session') {
                    const capture = testHarness.dependencies.runtime.captureSelectedConversationAuthority
                    testHarness.dependencies.runtime.captureSelectedConversationAuthority = () => ({
                        ...capture()!, sessionToken: createConversationSessionToken(),
                    })
                }
                if (change === 'mutation') {
                    const capture = testHarness.dependencies.runtime.captureSelectedConversationAuthority
                    testHarness.dependencies.runtime.captureSelectedConversationAuthority = () => ({
                        ...capture()!, sessionVersion: 1,
                    })
                }
                return result
            })
            await expect(testHarness.resolver.resolve({
                row: testHarness.source.snapshot().rowAt(7)!, totalMessages: 8,
                signal: controller.signal, isCurrent: () => current,
            })).rejects.toMatchObject({ name: change === 'abort' ? 'AbortError' : 'ChatParserHistoryProjectionStaleError' })
            expect(testHarness.pinReleases).toHaveBeenCalledOnce()
        },
    )

    test.each(['lua', 'plugin-v2', 'display-trigger'] as const)(
        'still supplies complete history to %s and releases the viewport pin before promotion',
        async (dependency) => {
            const testHarness = await cachedHarness({ unsafeDependencies: () => [dependency] })
            const result = await testHarness.resolver.resolve({
                row: testHarness.source.snapshot().rowAt(7)!, totalMessages: 8,
            })
            expect(result.kind).toBe('complete')
            expect(testHarness.completeContextMessageCounts).toEqual([8])
            expect(testHarness.pinReleases).toHaveBeenCalledOnce()
            expect(testHarness.releases()).toBe(0)
            if (result.kind === 'complete') result.release()
            expect(testHarness.releases()).toBe(1)
        },
    )
    test('keeps a plain greeting windowed without reading metadata-only history', async () => {
        const testHarness = harness({ metadataOnly: true })
        const lease = await testHarness.resolver.acquireConversationStart!({
            greeting: 'Synthetic greeting',
            totalMessages: 8,
        })
        expect(lease).toBeNull()
        expect(testHarness.acquire).not.toHaveBeenCalled()
        expect(testHarness.forbiddenHistoryReads()).toBe(0)
    })

    test('holds complete authority for a Lua greeting even when there are no message rows', async () => {
        const testHarness = harness({
            messages: [],
            metadataOnly: true,
            unsafeDependencies: () => ['lua'],
        })
        const lease = await testHarness.resolver.acquireConversationStart!({
            greeting: 'Synthetic greeting',
            totalMessages: 0,
        })
        expect(testHarness.acquire).toHaveBeenCalledTimes(1)
        expect(testHarness.forbiddenHistoryReads()).toBe(0)
        expect(testHarness.releases()).toBe(0)
        expect(lease).not.toBeNull()
        lease!.release()
        lease!.release()
        expect(testHarness.releases()).toBe(1)
    })

    test('accepts the same empty conversation retargeted after promotion flushes pending data', async () => {
        const testHarness = harness({
            messages: [],
            unsafeDependencies: () => ['lua'],
            advanceRevisionDuringAcquire: true,
        })
        const lease = await testHarness.resolver.acquireConversationStart!({
            greeting: 'Synthetic greeting',
            totalMessages: 0,
        })
        expect(lease).not.toBeNull()
        expect(testHarness.releases()).toBe(0)
        lease!.release()
        expect(testHarness.releases()).toBe(1)
    })

    test('rejects a greeting retargeted by navigation even if character and chat IDs match', async () => {
        const testHarness = harness({
            messages: [],
            unsafeDependencies: () => ['lua'],
            advanceRevisionDuringAcquire: true,
            navigateDuringAcquire: true,
        })
        await expect(
            testHarness.resolver.acquireConversationStart!({
                greeting: 'Synthetic greeting',
                totalMessages: 0,
            }),
        ).rejects.toThrow('Chat parser history projection became stale')
        expect(testHarness.releases()).toBe(1)
    })

    test.each(['{{history}}', '{{previouschatlog::0}}'])(
        'provides complete authority for greeting history expressions: %s',
        async (greeting) => {
            const testHarness = harness({ metadataOnly: true })
            const lease = await testHarness.resolver.acquireConversationStart!({
                greeting,
                totalMessages: 8,
            })
            expect(testHarness.acquire).toHaveBeenCalledTimes(1)
            expect(testHarness.forbiddenHistoryReads()).toBe(0)
            lease!.release()
            expect(testHarness.releases()).toBe(1)
        },
    )

    test('releases a greeting lease when the component is cancelled during promotion', async () => {
        const controller = new AbortController()
        const testHarness = harness({
            unsafeDependencies: () => ['lua'],
            onAcquire: () => controller.abort(),
        })
        await expect(
            testHarness.resolver.acquireConversationStart!({
                greeting: 'Synthetic greeting',
                totalMessages: 8,
                signal: controller.signal,
            }),
        ).rejects.toMatchObject({ name: 'AbortError' })
        expect(testHarness.releases()).toBe(1)
    })

    test('releases a greeting lease when navigation supersedes its request', async () => {
        let current = true
        const testHarness = harness({
            unsafeDependencies: () => ['lua'],
            onAcquire: () => {
                current = false
            },
        })
        await expect(
            testHarness.resolver.acquireConversationStart!({
                greeting: 'Synthetic greeting',
                totalMessages: 8,
                isCurrent: () => current,
            }),
        ).rejects.toThrow('Chat parser history projection became stale')
        expect(testHarness.releases()).toBe(1)
    })

    test('rejects a greeting whose message count changed while acquiring history', async () => {
        const testHarness = harness({ unsafeDependencies: () => ['lua'] })
        await expect(
            testHarness.resolver.acquireConversationStart!({
                greeting: 'Synthetic greeting',
                totalMessages: 9,
            }),
        ).rejects.toThrow('Chat parser history projection became stale')
        expect(testHarness.releases()).toBe(1)
    })

    test('binds the production complete context to promoted shared authority without mutating the bounded seed', () => {
        const boundedCharacter = owner(conversation([]))
        const boundedSeed = context(boundedCharacter)
        const completeConversation = conversation(Array.from({ length: 4 }, (_, index) => message(index)))
        const completeCharacter = owner(completeConversation)

        const complete = bindCompleteLiveParserContextAuthority(boundedSeed, {
            character: completeCharacter,
            conversation: completeConversation,
        })

        expect(boundedSeed.parserContext.character).toBe(boundedCharacter)
        expect(boundedCharacter.chats[0].message).toEqual([])
        expect(complete.parserContext.character).toBe(completeCharacter)
        expect(complete.parserContext.database.characters[0]).toBe(completeCharacter)
        expect(complete.parserContext.character.chats[0]).toBe(completeConversation)
        expect(complete.parserContext.database.characters[0].chats[0]).toBe(completeConversation)
        expect(completeConversation.message).toHaveLength(4)
        expect(completeConversation.message.every((_, index) => index in completeConversation.message)).toBe(true)
    })

    test('classifies live-only Lua, display trigger, plugin, and inject dependencies', () => {
        expect(collectLiveChatParserUnsafeDependencies({
            triggers: [
                { type: 'manual', effect: [{ type: 'triggerlua' }] },
                { type: 'display', effect: [] },
            ] as any,
            pluginV2EditDisplay: true,
            regexScripts: [{
                comment: '',
                type: 'editdisplay',
                in: 'match',
                out: '@@inject replacement',
            }],
        })).toEqual(['lua', 'display-trigger', 'plugin-v2', 'inject'])
    })

    test('returns a bounded context with absolute and projected parser indices', async () => {
        const testHarness = harness()

        const result = await testHarness.resolver.resolve({
            row: row(7, message(7)),
            totalMessages: 8,
        })

        expect(result.kind).toBe('bounded')
        if (result.kind !== 'bounded') throw new Error('Expected bounded projection')
        expect(result.chatID).toBe(7)
        expect(result.projectedChatID).toBe(4)
        expect(result.context?.parserContext.historyOffset).toBe(3)
        expect(
            result.context?.parserContext.character.chats[0].message.map((entry) => entry.data),
        ).toEqual(['message-3', 'message-4', 'message-5', 'message-6', 'message-7'])
        expect(testHarness.acquire).not.toHaveBeenCalled()
        expect(testHarness.boundedContextMessageCounts).toEqual([0])
    })

    test('promotes unsafe display parsing and exposes an idempotent complete lease', async () => {
        const testHarness = harness({ unsafeDependencies: () => ['lua'] })

        const promoted = await testHarness.resolver.resolve({
            row: row(7, message(7)),
            totalMessages: 8,
        })

        expect(promoted.kind).toBe('complete')
        if (promoted.kind !== 'complete') throw new Error('Expected complete projection')
        expect(testHarness.acquire).toHaveBeenCalledTimes(1)
        expect(testHarness.boundedContextMessageCounts).toEqual([0])
        expect(testHarness.completeContextMessageCounts).toEqual([8])
        promoted.release()
        promoted.release()
        expect(testHarness.releases()).toBe(1)

        const mounted = await testHarness.resolver.resolve({
            row: row(7, message(7)),
            totalMessages: 8,
        })
        expect(mounted.kind).toBe('complete')
        if (mounted.kind !== 'complete') throw new Error('Expected complete projection')
        expect(testHarness.acquire).toHaveBeenCalledTimes(2)
        mounted.release()
        expect(testHarness.releases()).toBe(2)
    })

    test('releases a promoted lease when the request becomes stale', async () => {
        let current = true
        const testHarness = harness({
            unsafeDependencies: () => ['display-trigger'],
            onAcquire: () => {
                current = false
            },
        })

        await expect(testHarness.resolver.resolve({
            row: row(7, message(7)),
            totalMessages: 8,
            isCurrent: () => current,
        })).rejects.toMatchObject({ name: 'ChatParserHistoryProjectionStaleError' })
        expect(testHarness.releases()).toBe(1)
    })
})
