import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import gemma4Reference from '../../tests/fixtures/tokenizer/gemma4.json'

type NativeRequest = { tokenizer_id: string; artifact_fingerprint: string; mode: 'count' | 'ids'; texts: string[] }

const harness = vi.hoisted(() => ({
    invoke: vi.fn(),
    runtime: 'web' as 'web' | 'tauri',
    providerOptions: new Map<string, unknown>(),
    jsModels: [] as string[],
    useTokenizerCaching: false,
    aiModel: 'gpt-4',
    modelTokenizer: 1,
    modelInternalID: 'synthetic-count-model',
    customTokenizer: 'tik',
    googleClaudeTokenizing: false,
    transformersTokenizer: vi.fn(),
}))

vi.mock('./platform', async () => (await import('./platform.testSupport')).platformModule(() => ({
    runtime: harness.runtime,
    os: 'windows',
})))
vi.mock('@tauri-apps/api/core', () => ({ invoke: harness.invoke }))
// Records each JavaScript tiktoken model a tokenizer builds.
vi.mock('@dqbd/tiktoken', async (importOriginal) => {
    const actual = await importOriginal<typeof import('@dqbd/tiktoken')>()
    return {
        ...actual,
        Tiktoken: new Proxy(actual.Tiktoken, {
            construct(target, args, newTarget) {
                harness.jsModels.push('tiktoken')
                return Reflect.construct(target, args, newTarget)
            },
        }),
    }
})

vi.mock('./storage/database.svelte', () => ({
    getCurrentCharacter: vi.fn(),
    getDatabase: () => ({
        aiModel: harness.aiModel,
        currentPluginProvider: '',
        customTokenizer: harness.customTokenizer,
        googleClaudeTokenizing: harness.googleClaudeTokenizing,
        useTokenizerCaching: harness.useTokenizerCaching,
    }),
}))

vi.mock('./process/files/inlays', () => ({ supportsInlayImage: () => false }))
vi.mock('./parser/parser.svelte', async () => (await import('./parser/parser.testSupport')).parserStubModule())
vi.mock('./process/transformers', () => ({
    tokenizeTransformers: harness.transformersTokenizer,
}))
vi.mock('./globalApi.svelte', () => ({ globalFetch: vi.fn() }))
vi.mock('./model/modellist', async () => ({
    getModelInfo: () => ({ tokenizer: harness.modelTokenizer, internalID: harness.modelInternalID }),
    LLMTokenizer: (await import('./model/types')).LLMTokenizer,
}))
vi.mock('./plugins/plugins.svelte', () => ({
    pluginV2: { providerOptions: harness.providerOptions },
}))
vi.mock('./alert', () => ({ alertError: vi.fn(), alertNormal: vi.fn() }))

import { ChatTokenizer, countTogether, encode, encodeWithTokenizer, strongBan, tokenCounter, tokenize } from './tokenizer'
import { pluginV2 } from './plugins/plugins.svelte'
import type { OpenAIChat } from './process/index.svelte'
import type { PromptItem } from './process/prompt'
import type { Chat, character } from './storage/database.svelte'
import { nativeTokenIds as nativeIds, nativeTokenizeAnswer } from './tokenizer.testSupport'

function nativeAnswer(_command: string, args: { request: NativeRequest }) {
    return nativeTokenizeAnswer(args.request)
}

// A native answer with the counts the JavaScript tokenizer gives.
function answerWith(counts: Map<string, number>) {
    return (_command: string, args: { request: NativeRequest }) => ({
        mode: 'count',
        artifact_fingerprint: args.request.artifact_fingerprint,
        counts: args.request.texts.map(text => counts.get(text)),
    })
}

// The JavaScript counts of `texts`, taken on the Web build.
async function javaScriptCounts(texts: (string | undefined)[]) {
    const counts = new Map<string, number>()
    for (const text of texts) {
        if (text !== undefined) counts.set(text, (await encode(text)).length)
    }
    return counts
}

// Each invoke's request, in order.
function invoked() {
    return harness.invoke.mock.calls.map(([, args]) => (args as { request: NativeRequest }).request)
}

// The tokenizer module a Tauri build loads, fresh, so the session latch and the caches start empty.
async function tauriTokenizer() {
    harness.runtime = 'tauri'
    vi.resetModules()
    return await import('./tokenizer')
}

function stubTokenizerAssets(fetched: string[], fromPublic: boolean) {
    vi.stubGlobal('fetch', vi.fn(async (input: string) => {
        fetched.push(input)
        if (!fromPublic) {
            throw new Error('synthetic asset failure')
        }
        if (!input.startsWith('/token/') || input.includes('..')) {
            throw new Error(`Unexpected tokenizer asset: ${input}`)
        }
        return new Response(await readFile(resolve('public', input.slice(1))))
    }))
}

type Settings = { modelTokenizer?: number; aiModel?: string; customTokenizer?: string; pluginTokenizer?: string }

function useSettings(settings: Settings) {
    if (settings.modelTokenizer !== undefined) harness.modelTokenizer = settings.modelTokenizer
    if (settings.aiModel !== undefined) harness.aiModel = settings.aiModel
    if (settings.customTokenizer !== undefined) harness.customTokenizer = settings.customTokenizer
    if (settings.pluginTokenizer !== undefined) harness.providerOptions.set('', { tokenizer: settings.pluginTokenizer })
}

afterEach(() => {
    harness.runtime = 'web'
    harness.aiModel = 'gpt-4'
    harness.modelTokenizer = 1
    harness.customTokenizer = 'tik'
    harness.useTokenizerCaching = false
    harness.googleClaudeTokenizing = false
    harness.providerOptions.clear()
    vi.unstubAllGlobals()
})

describe('chat tokenizer native count batching', () => {
    beforeEach(() => {
        pluginV2.providerOptions.clear()
        harness.invoke.mockReset()
        harness.useTokenizerCaching = false
        harness.aiModel = 'gpt-4'
        harness.modelTokenizer = 1
        harness.modelInternalID = 'synthetic-count-model'
        harness.googleClaudeTokenizing = false
        harness.transformersTokenizer.mockReset()
    })

    it('preserves arbitrary plugin tokenizer calls even with generation reuse enabled', async () => {
        harness.aiModel = 'custom'
        const tokenizerFunc = vi.fn().mockResolvedValueOnce([1, 2]).mockResolvedValueOnce([1, 2, 3])
        pluginV2.providerOptions.set('', { tokenizer: 'custom', tokenizerFunc })
        const tokenizer = new ChatTokenizer(2, 'noName', true)
        const chat = { role: 'user' as const, content: 'synthetic' }
        await expect(tokenizer.tokenizeChat(chat)).resolves.toBe(4)
        await expect(tokenizer.tokenizeChat(chat)).resolves.toBe(5)
        expect(tokenizerFunc).toHaveBeenCalledTimes(2)
    })

    it.each([false, true])('preserves exact generation counts across edits and tokenizer changes (global cache: %s)', async (globalCache) => {
        harness.useTokenizerCaching = globalCache
        const tokenizer = new ChatTokenizer(2, 'name', true)
        const chat: OpenAIChat = { role: 'user', content: '첫 번째 답변 🙂', name: '사용자' }
        const check = async (tokenizerId: string) => {
            const expected = (await encodeWithTokenizer(chat.content, tokenizerId)).length + 2
                + (chat.name ? (await encodeWithTokenizer(chat.name, tokenizerId)).length + 1 : 0)
            expect(await tokenizer.tokenizeChat(chat)).toBe(expected)
            expect(await tokenizer.tokenizeChat(chat)).toBe(expected)
        }
        await check('tik')
        chat.content = 'A changed answer with more words, punctuation and Unicode: 한글 🙂.'
        await check('tik')
        chat.name = 'A longer speaker name'
        await check('tik')
        delete chat.name
        await check('tik')
        harness.modelTokenizer = 2
        await check('o200k_base')
        harness.aiModel = 'custom'
        pluginV2.providerOptions.set('', { tokenizer: 'cl100k_base' })
        await check('tik')
        pluginV2.providerOptions.set('', { tokenizer: 'o200k_base' })
        await check('o200k_base')
    })

    it('recomputes multimodal and requested thought adjustments around cached text counts', async () => {
        const tokenizer = new ChatTokenizer(2, 'noName', true)
        const chat: OpenAIChat = { role: 'assistant', content: 'synthetic response' }
        const text = (await encodeWithTokenizer(chat.content, 'tik')).length + 2
        expect(await tokenizer.tokenizeChat(chat)).toBe(text)
        chat.multimodals = [{ type: 'image', base64: 'synthetic' }]
        expect(await tokenizer.tokenizeChat(chat)).toBe(text + 2)
        chat.multimodals.push({type: 'image', base64: 'synthetic-second'})
        expect(await tokenizer.tokenizeChat(chat)).toBe(text + 4)
        delete chat.multimodals
        chat.thoughts = ['first thought', '두 번째 생각']
        const thoughts = (await Promise.all(chat.thoughts.map(async thought =>
            (await encodeWithTokenizer(thought, 'tik')).length + 1))).reduce((total, count) => total + count, 0)
        expect(await tokenizer.tokenizeChat(chat, {countThoughts: true})).toBe(text + thoughts)
        expect(await tokenizer.tokenizeChat(chat)).toBe(text)
    })

    it('preserves remote tokenizer routing changes when generation reuse is enabled', async () => {
        harness.aiModel = 'google-test'
        harness.modelTokenizer = 10
        harness.googleClaudeTokenizing = true
        const fetchCount = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({totalTokens: 7})))
            .mockResolvedValueOnce(new Response(JSON.stringify({totalTokens: 11})))
        vi.stubGlobal('fetch', fetchCount)
        const tokenizer = new ChatTokenizer(2, 'noName', true)
        const chat: OpenAIChat = {role: 'user', content: 'synthetic remote count'}
        expect(await tokenizer.tokenizeChat(chat)).toBe(9)
        harness.modelInternalID = 'synthetic-count-model-other'
        expect(await tokenizer.tokenizeChat(chat)).toBe(13)
    })

    it('keeps count reuse opt-in and never reuses a failed count', async () => {
        harness.aiModel = 'hf:::synthetic/model'
        harness.modelTokenizer = 12
        harness.transformersTokenizer.mockResolvedValue([1])
        const chat = { role: 'user' as const, content: 'synthetic' }
        const ordinary = new ChatTokenizer(0, 'noName')
        await ordinary.tokenizeChat(chat)
        await ordinary.tokenizeChat(chat)
        expect(harness.transformersTokenizer).toHaveBeenCalledTimes(2)
        const cached = new ChatTokenizer(0, 'noName', true)
        harness.transformersTokenizer.mockRejectedValueOnce(new Error('synthetic tokenizer failure'))
        await expect(cached.tokenizeChat(chat)).rejects.toThrow('synthetic tokenizer failure')
        await expect(cached.tokenizeChat(chat)).resolves.toBe(1)
        expect(harness.transformersTokenizer).toHaveBeenCalledTimes(4)
    })

    it.each([
        'Xenova/opt-350m',
        'Xenova/tiny-random-mistral',
        'Xenova/gpt2-large-conversational',
        'synthetic/custom-model',
    ])(
        'tokenizes the WebLLM model %s without the removed GGUF server',
        async (model) => {
            harness.aiModel = `hf:::${model}`
            harness.modelTokenizer = 12
            harness.transformersTokenizer.mockResolvedValue([101, 42, 102])

            await expect(encode('synthetic prompt')).resolves.toEqual([
                101, 42, 102,
            ])
            expect(
                harness.transformersTokenizer,
            ).toHaveBeenCalledExactlyOnceWith('synthetic prompt', model)
        },
    )

    it('rejects a non-Hugging-Face local tokenizer instead of loading it as a model', async () => {
        harness.aiModel = 'local_C:/synthetic/model.gguf'
        harness.modelTokenizer = 12

        await expect(encode('synthetic prompt')).rejects.toThrow(
            'Local tokenization requires a Hugging Face model (hf:::).',
        )
        expect(harness.transformersTokenizer).not.toHaveBeenCalled()
    })

    it('counts a chat list with one native invocation', async () => {
        const chats = Array.from({ length: 100 }, (_, index) => ({
            role: 'user' as const,
            content: `content-${index}`,
            name: `name-${index}`,
        }))
        const tauri = await tauriTokenizer()
        harness.invoke.mockImplementation(nativeAnswer)

        const result = await new tauri.ChatTokenizer(3, 'name').tokenizeChats(chats)

        expect(result).toBe(chats.reduce((total, chat) => total + chat.content.length + 3 + chat.name.length + 1, 0))
        expect(invoked()).toEqual([{
            tokenizer_id: 'cl100k_base',
            artifact_fingerprint: expect.any(String),
            mode: 'count',
            texts: chats.flatMap(chat => [chat.content, chat.name]),
        }])
    })

    it('answers cached texts without an invoke', async () => {
        harness.useTokenizerCaching = true
        const tauri = await tauriTokenizer()
        harness.invoke.mockImplementation(nativeAnswer)

        expect(Array.from(await tauri.encode('cached ids'))).toEqual(nativeIds('cached ids'))
        expect(await tauri.tokenize('cached ids')).toBe(10)
        expect(await tauri.tokenize('cached count')).toBe(12)
        expect(await tauri.tokenize('cached count')).toBe(12)
        expect(await new tauri.ChatTokenizer(0, 'noName').tokenizeChats([
            { role: 'user', content: 'cached count' },
            { role: 'user', content: 'cached ids' },
        ])).toBe(22)
        // A cached count does not answer for IDs.
        expect(Array.from(await tauri.encode('cached count'))).toEqual(nativeIds('cached count'))

        expect(invoked().map(request => [request.mode, request.texts])).toEqual([
            ['ids', ['cached ids']],
            ['count', ['cached count']],
            ['ids', ['cached count']],
        ])
    })

    it('fills chatCounts from the batch path', async () => {
        const tauri = await tauriTokenizer()
        harness.invoke.mockImplementation(nativeAnswer)
        const tokenizer = new tauri.ChatTokenizer(2, 'name', true)
        const chats: OpenAIChat[] = [
            { role: 'user', content: 'first', name: 'user' },
            { role: 'assistant', content: 'second answer' },
        ]

        const batch = tokenizer.countBatch()
        for (const chat of chats) {
            expect(await batch.add(chat)).toBe(0)
        }
        expect(await batch.settle()).toEqual([5 + 2 + 4 + 1, 13 + 2])
        expect(await tokenizer.tokenizeChat(chats[0])).toBe(12)
        expect(await tokenizer.tokenizeChats(chats)).toBe(27)

        expect(invoked().map(request => request.texts)).toEqual([['first', 'user', 'second answer']])
    })

    it.each(['name', 'noName'] as const)(
        'counts mixed chat adjustments natively as JavaScript does in %s mode',
        async (useName) => {
            const chats = Array.from({ length: 100 }, (_, index) => ({
                role: index % 2 === 0 ? 'user' as const : 'assistant' as const,
                content: `mixed-content-${index}`,
                name: index % 3 === 0 ? `speaker-${index}` : undefined,
                thoughts: index === 0 ? ['hidden thought', 'another hidden thought'] : undefined,
                multimodals: index === 1
                    ? [
                        { type: 'image' as const, base64: 'image-a' },
                        { type: 'audio' as const, base64: 'audio-b' },
                    ]
                    : undefined,
            }))
            const expected = await new ChatTokenizer(3, useName).tokenizeChats(chats)
            const counts = await javaScriptCounts(chats.flatMap(chat => [chat.content, chat.name]))
            const tauri = await tauriTokenizer()
            harness.invoke.mockImplementation(answerWith(counts))

            await expect(new tauri.ChatTokenizer(3, useName).tokenizeChats(chats)).resolves.toBe(expected)

            const texts = invoked().flatMap(request => request.texts)
            expect(invoked()).toHaveLength(1)
            expect(texts.some(text => text.includes('hidden thought'))).toBe(false)
            expect(texts.some(text => text.startsWith('speaker-'))).toBe(useName === 'name')
        },
    )

    it.each([
        ['an IPC failure', new Error('synthetic IPC failure')],
        ['a refused text', { code: 'tokenizer_regex_limit', message: 'synthetic refusal', index: 3 }],
        ['a refused special token', { code: 'disallowed_special_token', message: 'synthetic refusal', index: 0 }],
    ])('keeps the JavaScript count after %s', async (_name, failure) => {
        const chats = Array.from({ length: 100 }, (_, index) => ({
            role: 'user' as const,
            content: index === 0 ? '<|ENDOFTEXT|>' : `fallback-content-${index}`,
            name: index % 4 === 0 ? `fallback-name-${index}` : undefined,
        }))
        const expected = await new ChatTokenizer(3, 'name').tokenizeChats(chats)
        const counts = await javaScriptCounts(chats.flatMap(chat => [chat.content, chat.name]))
        const tauri = await tauriTokenizer()
        harness.invoke.mockRejectedValueOnce(failure).mockImplementation(answerWith(counts))

        await expect(new tauri.ChatTokenizer(3, 'name').tokenizeChats(chats)).resolves.toBe(expected)
    })
})

describe('native batching for sequential callers', () => {
    beforeEach(() => {
        localStorage.clear()
        harness.invoke.mockReset()
        harness.invoke.mockImplementation(nativeAnswer)
    })

    const texts = () => invoked().map(request => request.texts)

    it('counts each chat of an inline batch as it is added', async () => {
        const tauri = await tauriTokenizer()
        const batch = new tauri.ChatTokenizer(1, 'noName').countBatch({ inline: true })

        expect(await batch.add({ role: 'user', content: 'alpha' })).toBe(6)
        expect(texts()).toEqual([['alpha']])
        expect(await batch.add({ role: 'user', content: 'beta' })).toBe(5)
        expect(await batch.settle()).toEqual([0, 0])
        expect(texts()).toEqual([['alpha'], ['beta']])
    })

    it('counts what a batch deferred when it is flushed, and the rest when it settles', async () => {
        const tauri = await tauriTokenizer()
        const batch = tauri.tokenCountBatch()

        expect(await batch.add('alpha')).toBe(0)
        expect(await batch.add('beta')).toBe(0)
        await batch.flush()
        expect(texts()).toEqual([['alpha', 'beta']])
        expect(await batch.add('gamma')).toBe(0)
        expect(await batch.settle()).toEqual([5, 4, 5])
        expect(texts()).toEqual([['alpha', 'beta'], ['gamma']])
    })

    it('answers a prefetched walk from one request', async () => {
        const tauri = await tauriTokenizer()
        const counter = tauri.tokenCounter()

        await counter.prefetch(['first', 'second', 'first'])
        expect(await counter.count('second')).toBe(6)
        expect(await counter.count('first')).toBe(5)
        expect(await counter.count('unlisted')).toBe(8)

        expect(invoked().map(request => [request.tokenizer_id, request.mode, request.texts])).toEqual([
            ['cl100k_base', 'count', ['first', 'second']],
            ['cl100k_base', 'count', ['unlisted']],
        ])
    })

    it('counts a prefetched text again after the tokenizer changes', async () => {
        const tauri = await tauriTokenizer()
        const counter = tauri.tokenCounter()

        await counter.prefetch(['first'])
        harness.modelTokenizer = 2
        expect(await counter.count('first')).toBe(5)
        await counter.prefetch(['first'])
        expect(await counter.count('first')).toBe(5)

        expect(invoked().map(request => [request.tokenizer_id, request.texts])).toEqual([
            ['cl100k_base', ['first']],
            ['o200k_base', ['first']],
            ['o200k_base', ['first']],
        ])
    })

    it('counts a walk with the tokenizer it names, and a text it did not prefetch without its IDs', async () => {
        const tauri = await tauriTokenizer()
        const counter = tauri.tokenCounter('o200k_base')

        await counter.prefetch(['first', 'second'])
        expect(await counter.count('first')).toBe(5)
        expect(await counter.count('third')).toBe(5)

        expect(invoked().map(request => [request.tokenizer_id, request.mode, request.texts])).toEqual([
            ['o200k_base', 'count', ['first', 'second']],
            ['o200k_base', 'count', ['third']],
        ])
    })

    it('leaves a walk counted in JavaScript to count only the texts it reaches', async () => {
        harness.aiModel = 'custom'
        const tokenizerFunc = vi.fn(async (text: string) => Array.from(text, () => 1))
        pluginV2.providerOptions.set('', { tokenizer: 'custom', tokenizerFunc })
        const counter = tokenCounter()

        await counter.prefetch(['first', 'second'])
        expect(await counter.count('second')).toBe(6)

        expect(tokenizerFunc.mock.calls).toEqual([['second']])
    })

    it('counts a copy or a multimodal chat from the texts a reusing tokenizer counted', async () => {
        const tauri = await tauriTokenizer()
        const tokenizer = new tauri.ChatTokenizer(1, 'noName', true)

        expect(await tokenizer.tokenizeChat({ role: 'user', content: 'shared' })).toBe(7)
        expect(await tokenizer.tokenizeChat({ role: 'assistant', content: 'shared' })).toBe(7)
        expect(await tokenizer.tokenizeChat({
            role: 'user', content: 'shared', multimodals: [{ type: 'image', base64: 'synthetic' }],
        })).toBe(8)

        expect(texts()).toEqual([['shared']])
    })

    it('counts the chats of a prefetched walk without another request', async () => {
        const tauri = await tauriTokenizer()
        const tokenizer = new tauri.ChatTokenizer(1, 'noName', true)

        await tokenizer.prefetch(['first summary', 'second summary'])
        expect(await tokenizer.tokenizeChat({ role: 'system', content: 'second summary' })).toBe(15)
        expect(await tokenizer.tokenizeChat({ role: 'system', content: 'first summary' })).toBe(14)
        await new tauri.ChatTokenizer(1, 'noName').prefetch(['unused'])

        expect(texts()).toEqual([['first summary', 'second summary']])
    })

    it('encodes the strings a loop encodes in one IDs request', async () => {
        const tauri = await tauriTokenizer()

        const ids = await tauri.prefetchTokenIds(['ban', 'bias', 'ban'])

        expect([...ids.keys()]).toEqual(['ban', 'bias'])
        expect(Array.from(ids.get('bias')!)).toEqual(nativeIds('bias'))
        expect(invoked().map(request => [request.mode, request.texts])).toEqual([['ids', ['ban', 'bias']]])
    })

    it('takes cached IDs and caches the IDs it encodes', async () => {
        harness.useTokenizerCaching = true
        const tauri = await tauriTokenizer()

        await tauri.encode('cached')
        await tauri.tokenize('counted')
        const ids = await tauri.prefetchTokenIds(['cached', 'counted', 'fresh'])
        expect(Array.from(ids.get('cached')!)).toEqual(nativeIds('cached'))
        expect(Array.from(ids.get('counted')!)).toEqual(nativeIds('counted'))
        expect(Array.from(await tauri.encode('fresh'))).toEqual(nativeIds('fresh'))

        expect(invoked().map(request => [request.mode, request.texts])).toEqual([
            ['ids', ['cached']],
            ['count', ['counted']],
            ['ids', ['counted', 'fresh']],
        ])
    })

    it('starts counts together where they run natively', async () => {
        const tauri = await tauriTokenizer()

        expect(await tauri.countTogether(() => tauri.tokenize('alpha'), () => tauri.tokenize('beta'))).toEqual([5, 4])

        expect(texts()).toEqual([['alpha', 'beta']])
    })

    it('runs counts in turn where they run in JavaScript', async () => {
        harness.aiModel = 'custom'
        const events: string[] = []
        pluginV2.providerOptions.set('', {
            tokenizer: 'custom',
            tokenizerFunc: async (text: string) => {
                events.push(`start ${text}`)
                await new Promise(resolve => setTimeout(resolve, 0))
                events.push(`end ${text}`)
                return [1]
            },
        })

        expect(await countTogether(() => tokenize('first'), () => tokenize('second'))).toEqual([1, 1])

        expect(events).toEqual(['start first', 'end first', 'start second', 'end second'])
    })

    it('counts a preset in one request', async () => {
        await tauriTokenizer()
        const { tokenizePreset } = await import('./process/prompt')
        const prompts = [
            { type: 'plain', type2: 'normal', role: 'system', text: 'plain text' },
            { type: 'chat', rangeStart: 0, rangeEnd: 'end' },
            { type: 'persona', innerFormat: 'persona {{slot}}' },
            { type: 'jailbreak', type2: 'normal', role: 'system', text: 'jailbreak' },
        ] as PromptItem[]

        expect(await tokenizePreset(prompts, true)).toBe(10 + 8 + 9)

        expect(texts()).toEqual([['plain text', 'persona ', 'jailbreak']])
    })

    it('counts a character in one request', async () => {
        const tauri = await tauriTokenizer()
        const char = {
            name: 'Nova',
            desc: 'desc {{char}}',
            personality: 'calm',
            scenario: 'at sea',
            globalLore: [{ content: '@@depth 0\nlore <char>\n\nmore' }],
        } as unknown as character

        expect(await tauri.getCharToken(char)).toEqual({ persistant: 9 + 4 + 6, dynamic: 14 })

        expect(texts()).toEqual([['desc Nova', 'calm', 'at sea', 'lore Nova\nmore']])
    })

    it('counts a chat log in one request', async () => {
        const tauri = await tauriTokenizer()
        const chat = { message: [{ role: 'user', data: 'hello' }, { role: 'char', data: 'reply!' }] } as Chat

        expect(await tauri.getChatToken(chat)).toBe(11)

        expect(texts()).toEqual([['hello', 'reply!']])
    })
})

describe('strong ban native batch routing', () => {
    beforeEach(() => {
        localStorage.clear()
        harness.invoke.mockReset()
    })

    it('uses one native IDs batch and applies every ordered result', async () => {
        const tauri = await tauriTokenizer()
        harness.invoke.mockImplementation(async (_command: string, args: { request: NativeRequest }) => ({
            mode: 'ids',
            artifact_fingerprint: args.request.artifact_fingerprint,
            ids: args.request.texts.map((_, index) => [10_000 + index]),
        }))
        const bias = { 42: -5 }

        const result = await tauri.strongBan('target', bias)

        expect(invoked()).toHaveLength(1)
        const [{ tokenizer_id, mode, texts }] = invoked()
        expect([tokenizer_id, mode]).toEqual(['cl100k_base', 'ids'])
        expect(texts.length).toBeGreaterThanOrEqual(100)
        const repeatedFirstInput = texts.findIndex(
            (text: string, index: number) => index > 0 && text === texts[0],
        )
        expect(repeatedFirstInput).toBeGreaterThan(0)
        expect(Object.keys(result)).toHaveLength(1 + texts.length - repeatedFirstInput)
        expect(result[10_000 + repeatedFirstInput]).toBe(-100)
        expect(result[42]).toBe(-5)
    })

    it('falls back to the JavaScript result when the native boundary fails', async () => {
        const expected = await strongBan('fallback-target', { 42: -5 })
        localStorage.clear()
        const tauri = await tauriTokenizer()
        harness.invoke.mockRejectedValue(new Error('native boundary failed'))

        const actual = await tauri.strongBan('fallback-target', { 42: -5 })

        expect(actual).toEqual(expected)
    })

    it('preserves the JavaScript error and partial bias mutation on native failure', async () => {
        const input = '<|ENDOFTEXT|>'
        const baselineBias: Record<number, number> = {}
        let baselineError: unknown
        try {
            await strongBan(input, baselineBias)
        } catch (error) {
            baselineError = error
        }
        expect(Object.keys(baselineBias).length).toBeGreaterThan(0)
        localStorage.clear()
        const tauri = await tauriTokenizer()
        harness.invoke.mockRejectedValue(new Error('native boundary failed'))
        const fallbackBias: Record<number, number> = {}
        let fallbackError: unknown
        try {
            await tauri.strongBan(input, fallbackBias)
        } catch (error) {
            fallbackError = error
        }

        expect(fallbackError).toEqual(baselineError)
        expect(fallbackBias).toEqual(baselineBias)
    })
})

describe('encodeWithTokenizer', () => {
    it('encodes with o200k_base when it is named', async () => {
        const { Tiktoken } = await import('@dqbd/tiktoken')
        const o200k = await import('src/etc/o200k_base.json')
        const expected = new Tiktoken(o200k.bpe_ranks, o200k.special_tokens, o200k.pat_str)
        const text = '합성 테스트 문장입니다. Synthetic sentence.'
        try {
            expect([...await encodeWithTokenizer(text, 'o200k_base')]).toEqual([...expected.encode(text)])
        } finally {
            expected.free()
        }
    })
})

describe('Gemma and Llama tokenizers', () => {
    const turnText = '<|turn>user\nHello<turn|>'
    const turnIds = [105, 2364, 107, 9259, 106]

    beforeEach(() => {
        harness.aiModel = 'gemini-2.5-flash'
        harness.modelTokenizer = 10
        harness.customTokenizer = 'gemma'
        harness.googleClaudeTokenizing = false
        harness.useTokenizerCaching = false
        pluginV2.providerOptions.clear()
        vi.stubGlobal('fetch', vi.fn(async (input: string) => {
            if (input.startsWith('https://generativelanguage.googleapis.com/')) {
                return new Response('', { status: 503 })
            }
            if (!input.startsWith('/token/') || input.includes('..')) {
                throw new Error(`Unexpected tokenizer asset: ${input}`)
            }
            const bytes = await readFile(resolve('public', input.slice(1)))
            return new Response(bytes)
        }))
    })

    afterEach(() => {
        vi.unstubAllGlobals()
        harness.aiModel = 'gpt-4'
        harness.modelTokenizer = 1
        harness.customTokenizer = 'tik'
        harness.googleClaudeTokenizing = false
        pluginV2.providerOptions.clear()
    })

    it.each(gemma4Reference.cases)('matches the Gemma 4 reference IDs for $name', async ({ text, ids }) => {
        expect([...await encode(text)]).toEqual(ids)
        expect([...await encodeWithTokenizer(text, 'gemma')]).toEqual(ids)
    })

    it.each(['openrouter', 'reverse_proxy', 'custom'])(
        'uses Gemma 4 when %s selects gemma',
        async (aiModel) => {
            harness.aiModel = aiModel
            pluginV2.providerOptions.set('', { tokenizer: 'gemma' })
            expect([...await encode(turnText)]).toEqual(turnIds)
        },
    )

    it('uses Gemma 4 for the built-in Gemma tokenizer', async () => {
        harness.modelTokenizer = 9
        expect([...await encode(turnText)]).toEqual(turnIds)
    })

    it('uses the same Gemma 4 IDs when the Google token count API fails', async () => {
        harness.googleClaudeTokenizing = true
        expect([...await encode(turnText)]).toEqual(turnIds)
    })

    it('keeps a successful Google token count when the API is enabled', async () => {
        harness.googleClaudeTokenizing = true
        vi.stubGlobal('fetch', vi.fn(async () => Response.json({ totalTokens: 7 })))
        expect((await encode('API count')).length).toBe(7)
    })

    it.each(['openrouter', 'reverse_proxy', 'custom'])(
        'uses Llama 3 IDs when %s selects llama3',
        async (aiModel) => {
            harness.aiModel = aiModel
            harness.customTokenizer = 'llama3'
            pluginV2.providerOptions.set('', { tokenizer: 'llama3' })
            const expected = [...await encodeWithTokenizer('Hello, world!', 'llama3')]
            expect([...await encodeWithTokenizer('Hello, world!', 'llama')]).not.toEqual(expected)
            expect([...await encode('Hello, world!')]).toEqual(expected)
        },
    )
})

describe('native tokenizer for the model tokenizer', () => {
    const fetched: string[] = []

    beforeEach(() => {
        localStorage.clear()
        pluginV2.providerOptions.clear()
        harness.aiModel = 'synthetic-model'
        harness.useTokenizerCaching = false
        harness.googleClaudeTokenizing = false
        harness.invoke.mockReset()
        harness.invoke.mockImplementation(nativeAnswer)
        stubTokenizerAssets(fetched, false)
    })

    // The tokenizers a Tauri build invokes for a strong ban and for a count.
    async function nativeTokenizers() {
        const tauri = await tauriTokenizer()
        await tauri.strongBan('target', {}).catch(() => null)
        await tauri.tokenize('synthetic').catch(() => null)
        return invoked().map(request => request.tokenizer_id)
    }

    // The model a call fetches, or the IDs it returns when it fetches none.
    async function loaded(call: () => Promise<ArrayLike<number>>) {
        fetched.length = 0
        const ids = await call().then(ids => Array.from(ids), () => null)
        return fetched.length > 0 ? [...fetched] : ids
    }

    it.each([
        [0, 'cl100k_base'], [1, 'cl100k_base'], [8, 'cl100k_base'], [2, 'o200k_base'], [7, 'novellist'], [6, 'claude'],
        [5, 'novelai'], [3, 'mistral'], [4, 'llama'],
        [10, 'gemma'], [9, 'gemma'], [13, 'deepseek'], [14, 'deepseek-v4'], [15, 'glm4'], [16, 'glm5'], [11, 'cohere'],
    ] as const)('names for model tokenizer %i the %s tokenizer encode runs', async (modelTokenizer, expected) => {
        harness.modelTokenizer = modelTokenizer
        const name = expected === 'cl100k_base' ? 'tik' : expected
        const text = 'Synthetic text, 합성 문장 🙂'
        expect(await loaded(() => encode(text))).toEqual(await loaded(() => encodeWithTokenizer(text, name)))
        // novellist stays in JavaScript, where web-tokenizers is still faster.
        expect(await nativeTokenizers()).toEqual(expected === 'novellist' ? [] : [expected, expected])
    })

    it.each([[12, false], [10, true]] as const)(
        'keeps model tokenizer %i in JavaScript (Google count API: %s)',
        async (modelTokenizer, googleClaudeTokenizing) => {
            harness.modelTokenizer = modelTokenizer
            harness.googleClaudeTokenizing = googleClaudeTokenizing
            expect(await nativeTokenizers()).toEqual([])
        },
    )
})

describe('native tokenizer on Tauri', () => {
    const fetched: string[] = []
    const text = 'Synthetic text, 합성 문장 🙂'

    beforeEach(() => {
        localStorage.clear()
        fetched.length = 0
        harness.jsModels.length = 0
        harness.invoke.mockReset()
        harness.invoke.mockImplementation(nativeAnswer)
    })

    it.each([
        ['cl100k_base', 'tik', { modelTokenizer: 1 }],
        ['o200k_base', 'o200k_base', { modelTokenizer: 2 }],
        ['claude', 'claude', { modelTokenizer: 6 }],
        ['mistral', 'mistral', { aiModel: 'openrouter', customTokenizer: 'mistral' }],
        ['gemma', 'gemma', { aiModel: 'custom', pluginTokenizer: 'gemma' }],
    ] as const)('encodes and counts with the native %s tokenizer without loading a JavaScript model', async (tokenizerId, name, settings) => {
        useSettings(settings)
        stubTokenizerAssets(fetched, false)
        const tauri = await tauriTokenizer()

        expect(Array.from(await tauri.encode(text))).toEqual(nativeIds(text))
        expect(await Promise.all([tauri.tokenize(text), tauri.tokenize('two')])).toEqual([text.length, 3])
        expect(await new tauri.ChatTokenizer(1, 'noName').tokenizeChats([{ role: 'user', content: text }])).toBe(text.length + 1)
        await tauri.strongBan('target', {})
        expect(Array.from(await tauri.encodeWithTokenizer(text, name))).toEqual(nativeIds(text))

        expect(harness.jsModels).toEqual([])
        expect(fetched).toEqual([])
        expect(invoked().map(request => [request.tokenizer_id, request.mode])).toEqual([
            [tokenizerId, 'ids'], [tokenizerId, 'count'], [tokenizerId, 'count'], [tokenizerId, 'ids'], [tokenizerId, 'ids'],
        ])
    })

    it('loads JavaScript for a refused text only and sends the next call natively', async () => {
        const refused = 'synthetic refused text'
        const expected = Array.from(await encode(refused))
        harness.jsModels.length = 0
        const tauri = await tauriTokenizer()
        harness.invoke
            .mockRejectedValueOnce({ code: 'tokenizer_regex_limit', message: 'synthetic refusal', index: 0 })
            .mockImplementation(nativeAnswer)

        expect(Array.from(await tauri.encode(refused))).toEqual(expected)
        expect(harness.jsModels).toEqual(['tiktoken'])
        expect(Array.from(await tauri.encode('next text'))).toEqual(nativeIds('next text'))
        expect(await tauri.tokenize('next count')).toBe(10)

        expect(invoked().map(request => request.texts)).toEqual([[refused], ['next text'], ['next count']])
    })

    it('counts a family in JavaScript for the session after its native tokenizer fails to load', async () => {
        useSettings({ modelTokenizer: 6 })
        stubTokenizerAssets(fetched, true)
        const tauri = await tauriTokenizer()
        harness.invoke
            .mockRejectedValueOnce({ code: 'tokenizer_artifact', message: 'synthetic missing artifact' })
            .mockImplementation(nativeAnswer)

        const count = await tauri.tokenize(text)
        expect(fetched).toEqual(['/token/claude/claude.json'])
        expect(Array.from(await tauri.encode(text))).toHaveLength(count)
        await tauri.strongBan('target', {})
        await new tauri.ChatTokenizer(0, 'noName').countBatch().add({ role: 'user', content: text })
        expect(invoked().map(request => request.tokenizer_id)).toEqual(['claude'])

        useSettings({ modelTokenizer: 1 })
        expect(await tauri.tokenize('other family')).toBe(12)
        expect(invoked().map(request => request.tokenizer_id)).toEqual(['claude', 'cl100k_base'])
    })

    it.each([
        ['the selected model', undefined],
        ['the tokenizer a walk names', 'claude'],
    ] as const)('counts in JavaScript only the texts a walk reaches when the native tokenizer fails its prefetch, for %s', async (_name, tokenizerType) => {
        useSettings({ modelTokenizer: 6 })
        stubTokenizerAssets(fetched, true)
        const expected = (await encodeWithTokenizer('second', 'claude')).length
        fetched.length = 0
        const tauri = await tauriTokenizer()
        harness.invoke.mockRejectedValueOnce(new Error('synthetic ipc failure'))
        const counter = tauri.tokenCounter(tokenizerType)

        await counter.prefetch(['first', 'second'])
        expect(fetched).toEqual([])
        expect(await counter.count('second')).toBe(expected)
        expect(fetched).toEqual(['/token/claude/claude.json'])
        expect(invoked().map(request => [request.tokenizer_id, request.texts])).toEqual([['claude', ['first', 'second']]])
    })

    it.each([
        ['the selected model', undefined],
        ['the tokenizer a walk names', 'claude'],
    ] as const)('counts a text the native tokenizer refuses ahead of a walk when the walk reaches it, and keeps counting natively, for %s', async (_name, tokenizerType) => {
        useSettings({ modelTokenizer: 6 })
        stubTokenizerAssets(fetched, true)
        const refused = 'synthetic refused text'
        const expected = (await encodeWithTokenizer(refused, 'claude')).length
        fetched.length = 0
        const tauri = await tauriTokenizer()
        harness.invoke.mockImplementation(async (command: string, args: { request: NativeRequest }) => {
            const index = args.request.texts.indexOf(refused)
            if (index >= 0) throw { code: 'tokenizer_regex_limit', message: 'synthetic refusal', index }
            return nativeAnswer(command, args)
        })
        const counter = tauri.tokenCounter(tokenizerType)

        await counter.prefetch(['first', refused])
        expect(fetched).toEqual([])
        expect(await counter.count('first')).toBe(5)
        expect(await counter.count(refused)).toBe(expected)
        expect(fetched).toEqual(['/token/claude/claude.json'])
        expect(await counter.count('later')).toBe(5)
        expect(invoked().map(request => request.texts)).toEqual([['first', refused], ['first'], [refused], ['later']])
    })

    it.each([
        ['OpenRouter', { aiModel: 'openrouter', customTokenizer: 'novellist' }],
        ['a plugin provider', { aiModel: 'custom', pluginTokenizer: 'novellist' }],
    ] as const)('never invokes the native tokenizer for novellist through %s', async (_name, settings) => {
        useSettings(settings)
        stubTokenizerAssets(fetched, false)
        const tauri = await tauriTokenizer()

        await tauri.encode(text).catch(() => null)
        await tauri.tokenize(text).catch(() => null)
        await new tauri.ChatTokenizer(0, 'noName').tokenizeChats([{ role: 'user', content: text }]).catch(() => null)
        await tauri.strongBan('target', {}).catch(() => null)
        await tauri.encodeWithTokenizer(text, 'novellist').catch(() => null)

        expect(fetched.length).toBeGreaterThan(0)
        expect(new Set(fetched)).toEqual(new Set(['/token/trin/spiece.model']))
        expect(harness.invoke).not.toHaveBeenCalled()
    })
})
