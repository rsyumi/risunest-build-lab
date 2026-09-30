import { describe, expect, it, vi } from 'vitest'
import type { PersistentDataStore, PersistentRevisionLease } from '../storage/persistentDataStore'
import { prepareSummaryAwareGeneration } from './summaryAwareGenerationPreparation'

function fixture(prefixCount = 300, tailCount = 2) {
    const messages = Array.from({ length: prefixCount + tailCount }, (_, index) => ({
        chatId: `m${index}`,
        role: index % 2 ? 'char' : 'user',
        data: `body-${index}`,
    }))
    const conversation = {
        id: 'conversation',
        name: 'Conversation',
        hypaV3Data: { summaries: [{ chatMemos: messages.slice(0, prefixCount).map((m) => m.chatId) }] },
    } as any
    return { messages, conversation }
}

function storeFor(messages: any[], revision = 7) {
    const bodyReads: number[] = []
    let released = 0
    const lease = {
        revision,
        async readConversationMessageMetadataWindow(input: any) {
            const end = Math.min(messages.length, input.startIndex + input.limit)
            return {
                revision,
                value: {
                    characterId: input.characterId,
                    conversationId: input.conversationId,
                    messages: messages.slice(input.startIndex, end).map((message) => ({
                        chatId: message.chatId,
                        role: message.role,
                        disabled: message.disabled,
                        parserInert: true,
                    })),
                    startIndex: input.startIndex,
                    endIndex: end,
                    totalMessages: messages.length,
                    hasMoreBefore: input.startIndex > 0,
                    hasMoreAfter: end < messages.length,
                },
            }
        },
        async readConversationWindow(input: any) {
            bodyReads.push(input.startIndex)
            const end = Math.min(messages.length, input.startIndex + input.limit)
            return {
                revision,
                value: {
                    characterId: input.characterId,
                    conversationId: input.conversationId,
                    messages: messages.slice(input.startIndex, end),
                    startIndex: input.startIndex,
                    endIndex: end,
                    totalMessages: messages.length,
                    hasMoreBefore: input.startIndex > 0,
                    hasMoreAfter: end < messages.length,
                },
            }
        },
        async release() { released += 1 },
    } as unknown as PersistentRevisionLease
    const store = {
        readConversationMessageMetadataWindow: vi.fn(),
        acquireRevision: vi.fn(async () => lease),
    } as unknown as PersistentDataStore
    return { store, lease, bodyReads, released: () => released }
}

describe('summary-aware bounded generation preparation', () => {
    it('falls back before acquiring a revision when selected-session edits are pending', async () => {
        const { messages, conversation } = fixture(2)
        const mocked = storeFor(messages)
        const result = await prepareSummaryAwareGeneration({
            store: mocked.store,
            authority: {
                kind: 'windowed', characterId: 'character', conversationId: 'conversation',
                sessionToken: 'session' as any, storeRevision: 7,
                persistedSessionVersion: 1, sessionVersion: 2, totalMessages: messages.length,
            },
            conversation,
            preserveOrphanedMemory: false,
            isCurrent: () => true,
        })

        expect(result).toEqual({
            route: 'complete',
            reason: 'pending-selected-session-edits',
        })
        expect(mocked.store.acquireRevision).not.toHaveBeenCalled()
        expect(mocked.released()).toBe(0)
    })

    it('grows metadata work but transfers only the unsummarized suffix', async () => {
        const { messages, conversation } = fixture()
        const mocked = storeFor(messages)
        const result = await prepareSummaryAwareGeneration({
            store: mocked.store,
            authority: {
                kind: 'windowed', characterId: 'character', conversationId: 'conversation',
                sessionToken: 'session' as any, storeRevision: 7,
                persistedSessionVersion: 0, sessionVersion: 0, totalMessages: messages.length,
            },
            conversation,
            preserveOrphanedMemory: false,
            isCurrent: () => true,
            now: () => 1,
        })
        expect(result.route).toBe('summary-aware')
        if (result.route !== 'summary-aware') return
        expect(result.preparation.chat.message.map((message) => message.chatId)).toEqual([
            'm300', 'm301',
        ])
        expect(mocked.bodyReads).toEqual([300])
        expect(result.preparation.metrics).toMatchObject({
            metadataRows: 302,
            metadataPages: 3,
            bodyRows: 2,
            bodyPages: 1,
        })
        await result.preparation.release()
        await result.preparation.release()
        expect(mocked.released()).toBe(1)
    })

    it('releases the pinned revision when the target changes between pages', async () => {
        const { messages, conversation } = fixture(130)
        const mocked = storeFor(messages)
        let checks = 0
        await expect(prepareSummaryAwareGeneration({
            store: mocked.store,
            authority: {
                kind: 'windowed', characterId: 'character', conversationId: 'conversation',
                sessionToken: 'session' as any, storeRevision: 7,
                persistedSessionVersion: 0, sessionVersion: 0, totalMessages: messages.length,
            },
            conversation,
            preserveOrphanedMemory: false,
            isCurrent: () => ++checks < 5,
        })).rejects.toThrow('target became stale')
        expect(mocked.released()).toBe(1)
        expect(mocked.bodyReads).toEqual([])
    })

    it('releases the pinned revision when the authority changes between body reads', async () => {
        const { messages, conversation } = fixture(2, 65)
        const mocked = storeFor(messages)
        let checks = 0
        await expect(prepareSummaryAwareGeneration({
            store: mocked.store,
            authority: {
                kind: 'windowed', characterId: 'character', conversationId: 'conversation',
                sessionToken: 'session' as any, storeRevision: 7,
                persistedSessionVersion: 0, sessionVersion: 0, totalMessages: messages.length,
            },
            conversation,
            preserveOrphanedMemory: false,
            isCurrent: () => ++checks <= 5,
        })).rejects.toThrow('target became stale')
        expect(mocked.released()).toBe(1)
        expect(mocked.bodyReads).toEqual([2])
    })
})


it('reads a mixed 300-row tail in five pages and preserves an oversized row', async () => {
    const { messages, conversation } = fixture(1, 300)
    messages[90].data = 'x'.repeat(2_000_000)
    const mocked = storeFor(messages)
    const result = await prepareSummaryAwareGeneration({
        store: mocked.store,
        authority: { kind: 'windowed', characterId: 'character', conversationId: 'conversation',
            sessionToken: 'session' as any, storeRevision: 7, persistedSessionVersion: 0, sessionVersion: 0, totalMessages: messages.length },
        conversation, preserveOrphanedMemory: false, isCurrent: () => true,
    })
    expect(result.route).toBe('summary-aware')
    if (result.route !== 'summary-aware') throw new Error('Expected bounded route')
    expect(mocked.bodyReads).toEqual([1, 65, 129, 193, 257])
    expect(result.preparation.chat.message).toEqual(messages.slice(1))
    await result.preparation.release()
})

it('rejects a revision change between body pages and releases its lease', async () => {
    const { messages, conversation } = fixture(1, 100)
    const mocked = storeFor(messages)
    const read = mocked.lease.readConversationWindow.bind(mocked.lease)
    vi.spyOn(mocked.lease, 'readConversationWindow').mockImplementation(async (input) => {
        const page = await read(input)
        return input.startIndex > 1 ? { ...page!, revision: 8 } : page
    })
    await expect(prepareSummaryAwareGeneration({
        store: mocked.store,
        authority: { kind: 'windowed', characterId: 'character', conversationId: 'conversation',
            sessionToken: 'session' as any, storeRevision: 7, persistedSessionVersion: 0, sessionVersion: 0, totalMessages: messages.length },
        conversation, preserveOrphanedMemory: false, isCurrent: () => true,
    })).rejects.toThrow('body page is incomplete')
    expect(mocked.bodyReads).toEqual([1, 65])
    expect(mocked.released()).toBe(1)
})
