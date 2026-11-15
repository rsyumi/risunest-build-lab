import { beforeEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({
    database: {} as Record<string, unknown>,
    scored: [] as [string, number][],
}))

vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => state.database }))
vi.mock('./hypamemory', () => ({
    HypaProcesser: class {
        oaikey = ''
        async addText() {}
        async similaritySearchScored() {
            return state.scored
        }
    },
}))
vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule())
vi.mock('../modules', async () => (await import('../tests/sendChatTestHarness')).modulesModule())
vi.mock('src/ts/alert', () => ({ alertError: vi.fn() }))
vi.mock('../request/request', () => ({ requestChatData: vi.fn() }))
vi.mock('src/ts/globalApi.svelte', () => ({ globalFetch: vi.fn() }))
vi.mock('../transformers', () => ({ runSummarizer: vi.fn() }))

import type { Chat, character } from 'src/ts/storage/database.svelte'
import type { ChatTokenizer } from 'src/ts/tokenizer'
import type { OpenAIChat } from '../index.svelte'
import { hanuraiMemory } from './hanuraiMemory'
import { hypaMemoryV2 } from './hypav2'

// Records each text the tokenizer is asked to count ahead and each chat it counts.
function recordingTokenizer(events: string[]) {
    return {
        prefetch: async (texts: string[]) => {
            events.push(`prefetch ${texts.join(' | ')}`)
        },
        tokenizeChat: async (chat: OpenAIChat) => {
            events.push(chat.content)
            return chat.content.length
        },
    } as unknown as ChatTokenizer
}

beforeEach(() => {
    vi.spyOn(console, 'log').mockImplementation(() => undefined)
})

describe('memory walks', () => {
    it('counts the HanuraiMemory results ahead before it walks them', async () => {
        state.database = { hanuraiSplit: false, hanuraiTokens: 0 }
        state.scored = [['search_document: memory alpha', 2], ['search_document: memory beta', 1]]
        const chats: OpenAIChat[] = ['first', 'second', 'third', 'fourth', 'fifth']
            .map((content) => ({ role: 'user', content }))
        const events: string[] = []

        await hanuraiMemory(chats, { currentTokens: 0, maxContextTokens: 1000, tokenizer: recordingTokenizer(events) })

        expect(events).toEqual(['prefetch  memory alpha |  memory beta', ' memory alpha', ' memory beta'])
    })

    it('counts the HypaV2 summaries and retrieved chunks ahead before it walks them', async () => {
        state.database = {
            maxResponse: 0, hypaAllocatedTokens: 1000, hypaChunkSize: 100, hypaModel: 'MiniLM', supaMemoryKey: '',
        }
        state.scored = [['search_document: chunk detail', 1], ['search_document: other detail', 0.5]]
        const chats: OpenAIChat[] = [
            { role: 'system', content: '[Start a new chat]', memo: 'NewChat' },
            { role: 'user', content: 'covered', memo: 'a' },
            { role: 'user', content: 'tail', memo: 'b' },
        ]
        const room = {
            hypaV2Data: {
                lastMainChunkID: 2,
                mainChunks: [
                    { id: 1, text: 'main summary', chatMemos: ['NewChat'], lastChatMemo: 'NewChat' },
                    { id: 2, text: 'later summary', chatMemos: ['a'], lastChatMemo: 'a' },
                ],
                chunks: [{ mainChunkID: 1, text: 'chunk detail' }, { mainChunkID: 2, text: 'other detail' }],
            },
        } as unknown as Chat
        const events: string[] = []

        await hypaMemoryV2(chats, 10, 10_000, room, {} as character, recordingTokenizer(events))

        expect(events.slice(2, -1)).toEqual([
            'prefetch main summary | later summary', 'main summary', 'later summary',
            'prefetch chunk detail | other detail', 'chunk detail', 'other detail',
        ])
    })
})
