import { expect, it, vi } from 'vitest'
import { prefetchTokenIds, tokenizeNum } from 'src/ts/tokenizer'
import { requestChatDataMain } from './request'

const mocks = vi.hoisted(() => ({
    globalFetch: vi.fn(),
    db: {
        aiModel: 'novelai_kayra',
        genTime: 1,
        temperature: 70,
        NAIsettings: {},
        novelai: { token: 'synthetic-token' },
    },
}))
vi.mock('src/ts/globalApi.svelte', () => ({ fetchNative: vi.fn(), globalFetch: mocks.globalFetch }))
vi.mock('src/ts/storage/database.svelte', () => ({
    getDatabase: () => mocks.db,
    getCurrentCharacter: vi.fn(),
    getCurrentChat: vi.fn(),
}))
vi.mock('src/ts/model/modellist', async () => {
    const types = await import('src/ts/model/types')
    return {
        ...types,
        getModelInfo: () => ({ id: 'novelai_kayra', format: types.LLMFormat.NovelAI, flags: [], internalID: 'kayra-v1' }),
    }
})
vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule({
    risuEscape: vi.fn(),
    risuUnescape: vi.fn(),
}))
vi.mock('src/ts/plugins/plugins.svelte', () => ({ pluginProcess: vi.fn(), pluginV2: {} }))
vi.mock('src/ts/tokenizer', () => ({ prefetchTokenIds: vi.fn(), tokenizeNum: vi.fn() }))
vi.mock('src/ts/util', () => ({ sleep: vi.fn() }))
vi.mock('../mcp/mcp', () => ({ getTools: vi.fn() }))
vi.mock('../models/nai', () => ({ NovelAIBadWordIds: [], stringlizeNAIChat: vi.fn(() => 'synthetic prompt') }))
vi.mock('../prompt', () => ({ OobaParams: {} }))
vi.mock('../stringlize', () => ({
    getStopStrings: vi.fn(),
    stringlizeAINChat: vi.fn(),
    unstringlizeAIN: vi.fn(),
    unstringlizeChat: (text: string) => text,
}))
vi.mock('../templates/chatTemplate', () => ({ applyChatTemplate: vi.fn() }))
vi.mock('../transformers', () => ({ runTransformers: vi.fn() }))
vi.mock('../triggers', () => ({ runTrigger: vi.fn() }))
vi.mock('./anthropic', () => ({ requestClaude: vi.fn() }))
vi.mock('./google', () => ({ requestGoogleCloudVertex: vi.fn() }))
vi.mock('./openAI/requests', () => ({
    requestOpenAI: vi.fn(),
    requestOpenAILegacyInstruct: vi.fn(),
    requestOpenAIResponseAPI: vi.fn(),
}))
vi.mock('./shared', () => ({
    applyAdditionalParameters: (body: unknown) => body,
    applyParameters: vi.fn(),
    getAdditionalParameters: () => [],
}))

it('sends the token IDs of the NovelAI bias strings from one batch', async () => {
    vi.mocked(prefetchTokenIds).mockImplementation(async (texts) => new Map(texts.map((text) => [text, [text.length, 7]])))
    vi.mocked(tokenizeNum).mockResolvedValue([99])
    mocks.globalFetch.mockResolvedValue({ ok: true, data: { output: 'synthetic output' } })

    await requestChatDataMain({
        formated: [{ role: 'user', content: 'synthetic' }],
        bias: {},
        biasString: [['first', 2], ['second', -1]],
    }, 'model')

    expect(vi.mocked(prefetchTokenIds).mock.calls).toEqual([[['first', 'second']]])
    const body = mocks.globalFetch.mock.calls[0][1].body
    expect(body.parameters.logit_bias_exp.map(({ sequence, bias }: { sequence: number[], bias: number }) => [sequence, bias]))
        .toEqual([[[5, 7], 2], [[6, 7], -1]])
})
