import { describe, expect, it } from 'vitest'

import type { Chat, Message } from '../storage/database.svelte'
import type { PersistentRevisionLease } from '../storage/persistentDataStore'
import type { WindowedConversationPersistenceAuthority } from '../storage/saveCoordinator'
import {
    admitHistoryWindow,
    createStoreHistoryWindowReader,
    prepareHistoryWindow,
    readStoreMessageMetadata,
    type HistoryWindowAdmissionInput,
} from './historyWindowGeneration'

const admission: HistoryWindowAdmissionInput = {
    requested: true,
    enabled: true,
    maxContext: 8000,
    group: false,
    supaMemory: false,
    supaModelType: 'none',
    hanuraiEnable: false,
    hypav2: false,
    hypaV3: false,
}

describe('admitHistoryWindow', () => {
    it('windows chat-screen sends without long-term memory or with HypaV3', () => {
        expect(admitHistoryWindow(admission)).toBe('none')
        expect(admitHistoryWindow({ ...admission, hypaV3: true })).toBe('none')
        expect(admitHistoryWindow({ ...admission, supaMemory: true })).toBe('none')
        expect(admitHistoryWindow({ ...admission, supaMemory: true, hypaV3: true })).toBe('hypaV3')
    })

    it('keeps today\'s path for the inactive cases', () => {
        expect(admitHistoryWindow({ ...admission, requested: false })).toBeNull()
        expect(admitHistoryWindow({ ...admission, enabled: false })).toBeNull()
        expect(admitHistoryWindow({ ...admission, group: true })).toBeNull()
        expect(admitHistoryWindow({ ...admission, maxContext: 0 })).toBeNull()
        expect(admitHistoryWindow({ ...admission, supaMemory: true, hanuraiEnable: true, hypaV3: true })).toBeNull()
        expect(admitHistoryWindow({ ...admission, supaMemory: true, hypav2: true, hypaV3: true })).toBeNull()
        expect(admitHistoryWindow({ ...admission, supaMemory: true, supaModelType: 'distilbart' })).toBeNull()
    })
})

function messages(count: number): Message[] {
    return Array.from({ length: count }, (_, index): Message => ({
        role: 'user',
        data: `m${index}`,
        chatId: `id-${index}`,
    }))
}

const authority = (totalMessages: number): WindowedConversationPersistenceAuthority => ({
    kind: 'windowed',
    characterId: 'character-a',
    conversationId: 'conversation-a',
    storeRevision: 7,
    sessionToken: 'token' as never,
    sessionVersion: 0,
    persistedSessionVersion: 0,
    totalMessages,
}) as WindowedConversationPersistenceAuthority

function leaseOver(all: Message[], options: { metadata?: boolean } = {}) {
    const bodyReads: Array<[number, number]> = []
    const metadataQueries: unknown[] = []
    let metadataPages = 0
    const lease = {
        revision: 7,
        async readConversationWindow({ startIndex, limit }: { startIndex: number, limit: number }) {
            bodyReads.push([startIndex, limit])
            const slice = all.slice(startIndex, startIndex + limit)
            return {
                revision: 7,
                value: {
                    characterId: 'character-a',
                    conversationId: 'conversation-a',
                    messages: slice,
                    startIndex,
                    endIndex: startIndex + slice.length,
                    totalMessages: all.length,
                },
            }
        },
        ...(options.metadata === false ? {} : {
            async readConversationMessageMetadataWindow(query: { startIndex: number, limit: number }) {
                const { startIndex, limit } = query
                metadataQueries.push(query)
                metadataPages += 1
                const slice = all.slice(startIndex, startIndex + limit)
                return {
                    revision: 7,
                    value: {
                        characterId: 'character-a',
                        conversationId: 'conversation-a',
                        messages: slice.map((message) => ({
                            chatId: message.chatId,
                            role: message.role,
                            disabled: message.disabled,
                            parserInert: true,
                        })),
                        startIndex,
                        endIndex: startIndex + slice.length,
                        totalMessages: all.length,
                    },
                }
            },
        }),
    } as unknown as PersistentRevisionLease
    return { lease, bodyReads, metadataQueries, metadataPages: () => metadataPages }
}

const conversation = (hypaV3Data?: Chat['hypaV3Data']): Omit<Chat, 'message'> => ({
    id: 'conversation-a',
    name: '',
    note: '',
    localLore: [],
    hypaV3Data,
})

describe('prepareHistoryWindow', () => {
    it('reads only the newest pages from the store when memory is off', async () => {
        const all = messages(2000)
        const { lease, bodyReads, metadataPages } = leaseOver(all)

        const prepared = await prepareHistoryWindow({
            reader: createStoreHistoryWindowReader(lease, authority(all.length)),
            memory: 'none',
            conversation: conversation(),
            readMetadata: () => readStoreMessageMetadata(lease, authority(all.length), () => {}),
            preserveOrphanedMemory: false,
            queryChatCount: 4,
            tokenBudget: 30,
            countTokens: async () => 10,
            assertCurrent: () => {},
        })

        expect(prepared.selection.start).toBe(1997)
        expect(prepared.hypaPlan).toBeNull()
        expect(metadataPages()).toBe(0)
        expect(bodyReads).toEqual([[1936, 64]])
    })

    it('extends a HypaV3 window to the summary boundary and keeps the whole memo set', async () => {
        const all = messages(2000)
        const { lease } = leaseOver(all)
        const summaries = [{ text: 's', chatMemos: ['id-0', 'id-1499'], isImportant: false }]

        const prepared = await prepareHistoryWindow({
            reader: createStoreHistoryWindowReader(lease, authority(all.length)),
            memory: 'hypaV3',
            conversation: conversation({ summaries } as Chat['hypaV3Data']),
            readMetadata: () => readStoreMessageMetadata(lease, authority(all.length), () => {}),
            preserveOrphanedMemory: false,
            queryChatCount: 4,
            tokenBudget: 30,
            countTokens: async () => 10,
            assertCurrent: () => {},
        })

        expect(prepared.selection.start).toBe(1499)
        expect(prepared.hypaPlan?.effectiveMessageMemos).toHaveLength(2000)
    })

    it('reads HypaV3 message metadata in pages of the store range limit', async () => {
        const all = messages(9000)
        const { lease, metadataPages } = leaseOver(all)

        const rows = await readStoreMessageMetadata(lease, authority(all.length), () => {})

        expect(rows).toHaveLength(9000)
        expect(rows?.at(-1)?.chatId).toBe('id-8999')
        expect(metadataPages()).toBe(3)
    })

    it('asks the store to skip parser work, since HypaV3 reads only ids and disabled flags', async () => {
        const all = messages(5000)
        const { lease, metadataQueries } = leaseOver(all)

        await readStoreMessageMetadata(lease, authority(all.length), () => {})

        expect(metadataQueries).toEqual([
            expect.objectContaining({ startIndex: 0, limit: 4096, skipParserWork: true }),
            expect.objectContaining({ startIndex: 4096, limit: 904, skipParserWork: true }),
        ])
    })

    it('applies the queryChatCount floor for HypaV3', async () => {
        const all = messages(100)
        const { lease } = leaseOver(all)
        const summaries = [{ text: 's', chatMemos: ['id-99'], isImportant: false }]

        const prepared = await prepareHistoryWindow({
            reader: createStoreHistoryWindowReader(lease, authority(all.length)),
            memory: 'hypaV3',
            conversation: conversation({ summaries } as Chat['hypaV3Data']),
            readMetadata: () => readStoreMessageMetadata(lease, authority(all.length), () => {}),
            preserveOrphanedMemory: false,
            queryChatCount: 6,
            tokenBudget: 10,
            countTokens: async () => 10,
            assertCurrent: () => {},
        })

        expect(prepared.selection.start).toBe(94)
    })

    it('loads the whole conversation for HypaV3 when metadata cannot be read', async () => {
        const all = messages(300)
        const { lease } = leaseOver(all, { metadata: false })
        const summaries = [{ text: 's', chatMemos: ['id-250'], isImportant: false }]

        const prepared = await prepareHistoryWindow({
            reader: createStoreHistoryWindowReader(lease, authority(all.length)),
            memory: 'hypaV3',
            conversation: conversation({ summaries } as Chat['hypaV3Data']),
            readMetadata: () => readStoreMessageMetadata(lease, authority(all.length), () => {}),
            preserveOrphanedMemory: false,
            queryChatCount: 4,
            tokenBudget: 10,
            countTokens: async () => 10,
            assertCurrent: () => {},
        })

        expect(prepared.selection.start).toBe(0)
        expect(prepared.hypaPlan?.effectiveMessageMemos).toHaveLength(300)
    })

    it('rejects a store page from another revision', async () => {
        const all = messages(10)
        const { lease } = leaseOver(all)
        const reader = createStoreHistoryWindowReader(lease, { ...authority(all.length), storeRevision: 8 })

        await expect(reader.read(5, 5)).rejects.toThrow('incomplete')
    })
})
