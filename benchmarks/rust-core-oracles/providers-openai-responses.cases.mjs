// Writes the case files the OpenAI Responses oracle (`providers-openai-responses.oracle.test.ts`) reads: `requests`,
// `responses` and `streams` under tests/fixtures/rust-core/providers/openai-responses. The cases are deterministic and use
// synthetic keys only. Run `node benchmarks/rust-core-oracles/providers-openai-responses.cases.mjs` and then the oracle with
// RUST_CORE_ORACLE_WRITE=1 to refresh the expectations.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultOut = resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/openai-responses')
const out = process.argv[2] ?? defaultOut
mkdirSync(out, { recursive: true })

// LLMFlags numbers (src/ts/model/types.ts); custom flags replace the flags of the selected model.
const F = { hasImageInput: 0, hasPrefill: 4, hasFullSystemPrompt: 6, hasFirstSystemPrompt: 7, hasStreaming: 8, DeveloperRole: 14 }
const flags = (...names) => ({ enableCustomFlags: true, customFlags: names.map(name => F[name]) })

const sys = text => ({ role: 'system', content: text })
const user = (text, extra = {}) => ({ role: 'user', content: text, ...extra })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const media = (type, base64) => ({ type, base64 })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const baseDb = {
    usePlainFetch: true,
    openAIKey: 'sk-synthetic-openai',
    proxyKey: 'sk-synthetic-proxy',
    nanogptKey: 'sk-synthetic-nanogpt',
    OaiCompAPIKeys: { deepseek: 'sk-synthetic-deepseek' },
    requestRetrys: 0,
    genTime: 1,
    useStreaming: false,
    applyAdditionalParamsToAll: false,
    additionalParams: [],
    maxResponse: 300,
    temperature: 80,
    top_k: 40,
    repetition_penalty: 1.1,
    min_p: 0.05,
    top_a: 0.2,
    top_p: 0.9,
    frequencyPenalty: 20,
    PresensePenalty: 30,
    thinkingTokens: 1024,
    reasoningEffort: 1,
    verbosity: 1,
    newOAIHandle: true,
    gptVisionQuality: 'low',
    jsonSchemaEnabled: false,
    jsonSchema: '{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}',
    strictJsonSchema: true,
    extractJson: '',
    seperateParametersEnabled: false,
    seperateParametersByModel: false,
    simplifiedToolUse: false,
    modelTools: [],
    autofillRequestUrl: false,
    systemContentReplacement: '',
    systemRoleReplacement: '',
    forceReplaceUrl: '',
    proxyRequestModel: '',
    customProxyRequestModel: 'gpt-4.1',
    customAPIFormat: 18,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

const separate = {
    seperateParametersEnabled: true,
    seperateParameters: {
        model: { temperature: 50, top_k: 10, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.5, frequency_penalty: 10, presence_penalty: 10, thinking_tokens: 0, reasoning_effort: 2, verbosity: 2 },
        submodel: { temperature: 60, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        memory: { temperature: 61, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        emotion: { temperature: 62, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        translate: { temperature: 63, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        otherAx: { temperature: 70, top_k: 12, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.7, frequency_penalty: 12, presence_penalty: 12, thinking_tokens: 0, reasoning_effort: 1, verbosity: 1 },
        overrides: { 'gpt-5-response-api': { temperature: 30, top_k: 5, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.3, frequency_penalty: -1000, presence_penalty: 5, thinking_tokens: 0, reasoning_effort: 3, verbosity: 0 } },
    },
}

const customModel = (extra = {}) => ({ id: 'xcustom:::one', name: 'One', internalId: 'my-responses-model', url: 'https://llm.example.test/v1/responses', key: 'sk-synthetic-custom', format: 18, flags: [F.hasFullSystemPrompt, F.hasStreaming], tokenizer: 0, params: '', ...extra })

const lookup = { name: 'lookup', description: 'Looks a word up.', inputSchema: { type: 'object', properties: { query: { type: 'string', description: 'The word.' } }, required: ['query'] } }
const clock = { name: 'clock', description: 'Tells the time.', inputSchema: { type: 'object', properties: { zone: { type: ['string', 'null'] } } } }
const schema = '{"type":"object","properties":{"answer":{"type":"string"},"score":{"type":"number"}},"required":["answer"]}'

const G = 'gpt4o-response-api'
const R5 = 'gpt-5-response-api'

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: G, ...entry })
    const proxy = (id, url, extra = {}) => add(id, { model: 'reverse_proxy', db: { forceReplaceUrl: url, ...extra.db }, messages: chat, ...extra.entry })

    // Layout and messages.
    add('basic', { messages: chat })
    add('stream', { stream: true, messages: chat })
    add('max-tokens', { maxTokens: 777, messages: chat })
    add('no-system', { messages: [user('Hello')] })
    add('multi-turn', { messages: talk })
    add('developer-role-for-system', { model: R5, messages: chat })
    add('developer-role-by-custom-flags', { db: flags('hasFullSystemPrompt', 'DeveloperRole'), messages: chat })
    add('system-after-user-stays-system', { messages: [sys('Be brief.'), user('One'), sys('Reminder'), bot('Two')] })
    add('empty-user-text-dropped', { messages: [sys('Be brief.'), user(''), user('Hello')] })
    add('empty-user-text-kept-without-new-handle', { db: { newOAIHandle: false }, messages: [sys('Be brief.'), user(''), user('Hello')] })
    add('empty-assistant-keeps-an-empty-message', { messages: [sys('Be brief.'), user('Hi'), bot(''), user('Hello')] })
    add('function-role-is-dropped', { messages: [sys('Be brief.'), user('Hi'), { role: 'function', content: 'result', name: 'lookup' }, bot('Done')] })
    add('message-fields-are-ignored', { messages: [sys('Be brief.'), { role: 'user', content: 'Hi', name: 'Alice', memo: 'm', attr: ['a'], removable: true, cachePoint: true }, bot('Hey', { thoughts: ['t'] })] })
    add('unicode-and-escapes', { messages: [sys('Be brief.'), user('日本語 \u{1F600} "quoted" \\ back\nline  ')] })
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' }, messages: chat })

    // Media.
    add('image', { messages: [sys('Be brief.'), user('Look', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('image-detail-high', { db: { gptVisionQuality: 'high' }, messages: [user('Look', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('image-without-text', { messages: [user('', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('image-without-text-without-new-handle', { db: { newOAIHandle: false }, messages: [user('', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('audio-video-and-signature-become-files', { messages: [user('Hear', { multimodals: [media('audio', 'data:audio/wav;base64,BBBB'), media('video', 'data:video/mp4;base64,CCCC'), media('signature', 'c2ln')] })] })
    add('media-on-assistant-is-ignored', { messages: [user('Hi'), bot('Hey', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('media-on-system', { messages: [sys('Look', { multimodals: [media('image', 'data:image/png;base64,AAAA')] }), user('Hello')] })

    // Remembered tool calls.
    const call = (id, name, arg, text) => ({ call: { id, name, arg }, response: [{ type: 'text', text }] })
    const marker = key => `<tool_call>${key}</tool_call>`
    add('marker-in-assistant', {
        messages: [sys('Be brief.'), user('Look it up'), bot(`Checking.${marker('k1')}\n\nDone.`), user('Thanks')],
        decodings: { k1: call('call_1', 'lookup', '{"query":"x"}', '{"found":true}') },
    })
    add('marker-only', { messages: [user('Hi'), bot(marker('k1'))], decodings: { k1: call('call_1', 'lookup', '{"query":"x"}', 'ok') } })
    add('two-markers', {
        messages: [user('Hi'), bot(`A${marker('k1')}B${marker('k2')}C`)],
        decodings: { k1: call('call_1', 'lookup', '{"query":"x"}', 'one'), k2: call('call_2', 'clock', '{}', 'two') },
    })
    add('marker-with-whitespace-text-carries-over', { messages: [user('Hi'), bot(` ${marker('k1')} x`)], decodings: { k1: call('call_1', 'lookup', '{}', 'ok') } })
    add('marker-undecodable-is-dropped', { messages: [user('Hi'), bot(`A${marker('missing')}B`)] })
    add('marker-unclosed-stays-text', { messages: [user('Hi'), bot('A<tool_call>k1')], decodings: { k1: call('call_1', 'lookup', '{}', 'ok') } })
    add('marker-spans-lines', { messages: [user('Hi'), bot('A<tool_call>k\n1</tool_call>B')], decodings: { 'k\n1': call('call_1', 'lookup', '{}', 'ok') } })
    add('marker-response-non-text-parts', { messages: [user('Hi'), bot(marker('k1'))], decodings: { k1: { call: { id: 'c', name: 'lookup', arg: '{}' }, response: [{ type: 'image', mimeType: 'image/png', data: 'AA' }, { type: 'text', text: 'a' }, { type: 'text', text: 'b' }] } } })
    add('marker-in-user-is-text', { messages: [user(`Hi ${marker('k1')}`)], decodings: { k1: call('call_1', 'lookup', '{}', 'ok') } })

    // Tools and the search tool.
    add('tools', { tools: [lookup, clock], messages: chat })
    add('search-tool', { db: { modelTools: ['search'] }, messages: chat })
    add('tools-and-search-tool', { db: { modelTools: ['search'] }, tools: [lookup], messages: chat })
    add('other-model-tools-ignored', { db: { modelTools: ['image'] }, messages: chat })

    // Parameters and reasoning.
    add('gpt5-parameters', { model: R5, messages: chat })
    add('gpt5-effort-minimal', { model: R5, db: { reasoningEffort: -1 }, messages: chat })
    add('gpt5-effort-low', { model: R5, db: { reasoningEffort: 0 }, messages: chat })
    add('gpt5-effort-high', { model: R5, db: { reasoningEffort: 2 }, messages: chat })
    add('gpt5-effort-xhigh-falls-back-to-high', { model: R5, db: { reasoningEffort: 3 }, messages: chat })
    add('gpt5-effort-unknown-is-medium', { model: R5, db: { reasoningEffort: 9 }, messages: chat })
    add('gpt5-verbosity-low', { model: R5, db: { verbosity: 0 }, messages: chat })
    add('gpt5-verbosity-high', { model: R5, db: { verbosity: 2 }, messages: chat })
    add('gpt5-verbosity-unknown-is-medium', { model: R5, db: { verbosity: 7 }, messages: chat })
    add('gpt51-effort-none', { model: 'gpt-5.1-response-api', db: { reasoningEffort: -1 }, messages: chat })
    add('gpt54-pro-effort-min-medium', { model: 'gpt-5.4-pro-response-api', db: { reasoningEffort: 0 }, messages: chat })
    add('gpt54-pro-effort-xhigh', { model: 'gpt-5.4-pro-response-api', db: { reasoningEffort: 3 }, messages: chat })
    add('o3-reasoning-only', { model: 'o3-response-api', messages: chat })
    add('o3-developer-role', { model: 'o3-response-api', messages: chat })
    add('chat-latest-without-reasoning', { model: 'gpt-5.2-chat-latest-response-api', messages: chat })
    add('temperature-disabled', { db: { temperature: -1000 }, messages: chat })
    add('top-p-zero', { db: { top_p: 0 }, messages: chat })
    add('separate-parameters-model', { model: R5, db: separate, messages: chat })
    add('separate-parameters-submodel', { model: R5, db: separate, mode: 'submodel', messages: chat })
    add('separate-parameters-memory', { model: R5, db: separate, mode: 'memory', messages: chat })
    add('separate-parameters-by-model', { model: R5, db: { ...separate, seperateParametersByModel: true }, messages: chat })
    add('separate-parameters-by-model-missing-throws', { db: { ...separate, seperateParametersByModel: true }, messages: chat })

    // Structured output.
    add('schema-argument', { schema, messages: chat })
    add('schema-argument-with-verbosity', { model: R5, schema, messages: chat })
    add('schema-database-declines', { db: { jsonSchemaEnabled: true }, messages: chat, declines: 'schema-from-database' })
    add('schema-not-strict', { schema, db: { strictJsonSchema: false }, messages: chat })
    add('schema-strict-null-is-true', { schema, db: { strictJsonSchema: null }, messages: chat })
    add('schema-with-pro-model', { model: 'gpt-5.4-pro-response-api', schema, messages: chat })

    // Additional parameters.
    const extra = pairs => ({ model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/responses', additionalParams: pairs }, messages: chat })
    add('additional-number', extra([['seed', '7']]))
    add('additional-string-and-bool', extra([['user', '"abc"'], ['parallel_tool_calls', 'false']]))
    add('additional-null-and-text', extra([['x', 'null'], ['y', 'plain text']]))
    add('additional-json', extra([['metadata', 'json::{"a":1}']]))
    add('additional-nested', extra([['reasoning.effort', 'high'], ['text.verbosity', 'low']]))
    add('additional-header', extra([['header::X-Test', 'on'], ['header::Authorization', 'Bearer other']]))
    add('additional-none-removes', extra([['header::Content-Type', '{{none}}'], ['store', '{{none}}'], ['max_output_tokens', '{{none}}']]))
    add('additional-store-true-keeps-items', { ...extra([['store', 'true']]), messages: [sys('Be brief.'), user('Hi'), bot('Hey'), user('Hello')] })
    add('additional-stream-overridden', { ...extra([['stream', 'true']]), messages: chat })
    add('additional-stream-overridden-streaming', { ...extra([['stream', 'false']]), stream: true, messages: chat })
    add('additional-input-declines', { ...extra([['input', 'x']]), declines: 'additional-parameter-input' })
    add('additional-input-nested-declines', { ...extra([['input.0', 'x']]), declines: 'additional-parameter-input' })
    add('additional-ignored-for-openai-model', { db: { additionalParams: [['seed', '7']] }, messages: chat })
    add('additional-applied-for-custom-model', { model: 'xcustom:::one', customModels: [customModel({ params: 'seed=9\nheader::X-Custom=yes' })], messages: chat })
    add('additional-apply-to-all-does-not-reach-openai', { db: { additionalParams: [['seed', '7']], applyAdditionalParamsToAll: true }, messages: chat })

    // Endpoints and keys.
    add('openai-endpoint', { messages: chat })
    add('reverse-proxy', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/responses' }, messages: chat })
    add('reverse-proxy-stream', { model: 'reverse_proxy', stream: true, db: { forceReplaceUrl: 'https://proxy.example.test/v1/responses' }, messages: chat })
    add('reverse-proxy-no-url-declines', { model: 'reverse_proxy', messages: chat, declines: 'url-form' })
    add('reverse-proxy-risu-prefix', { model: 'reverse_proxy', db: { forceReplaceUrl: 'risu::https://proxy.example.test/v1/responses' }, messages: chat })
    add('reverse-proxy-model-and-key', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/x', customProxyRequestModel: 'my-proxy-model', proxyKey: 'sk-other' }, messages: chat })
    add('reverse-proxy-empty-model-uses-id', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/x', customProxyRequestModel: '' }, messages: chat })
    add('reverse-proxy-no-autofill', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test' }, messages: chat })
    for (const [name, url] of [
        ['bare-host', 'https://proxy.example.test'], ['bare-host-slash', 'https://proxy.example.test/'], ['v1', 'https://proxy.example.test/v1'],
        ['v1-slash', 'https://proxy.example.test/v1/'], ['responses', 'https://proxy.example.test/v1/responses'], ['responses-slash', 'https://proxy.example.test/v1/responses/'],
        ['other-path', 'https://proxy.example.test/api'], ['other-path-slash', 'https://proxy.example.test/api/'], ['query', 'https://proxy.example.test/api?x=1'],
        ['query-v1', 'https://proxy.example.test/v1?x=1'], ['fragment', 'https://proxy.example.test/api#frag'], ['port', 'https://Proxy.Example.test:8443/api'],
        ['azure', 'https://res.openai.azure.com/openai/responses?api-version=2025-03-01-preview'],
        ['azure-deployment', 'https://res.openai.azure.com/openai/deployments/x/responses?api-version=2025-03-01-preview'],
        ['api-version-without-responses', 'https://res.openai.azure.com/openai?api-version=1'], ['http', 'http://proxy.example.test/api'],
        ['empty-segments', 'https://proxy.example.test//v1'], ['space-in-path', 'https://proxy.example.test/my path'],
        ['risu-prefix', 'risu::https://proxy.example.test/api'],
    ]) {
        add(`autofill-${name}`, { model: 'reverse_proxy', db: { forceReplaceUrl: url, autofillRequestUrl: true }, messages: chat })
        add(`autofill-${name}-stream`, { model: 'reverse_proxy', stream: true, db: { forceReplaceUrl: url, autofillRequestUrl: true }, messages: chat })
    }
    add('autofill-userinfo-declines', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://user:pw@proxy.example.test/api', autofillRequestUrl: true }, messages: chat, declines: 'url-form' })
    add('autofill-ip-host-declines', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://127.0.0.1:8080/api', autofillRequestUrl: true }, messages: chat, declines: 'url-form' })
    add('autofill-not-a-url-declines', { model: 'reverse_proxy', db: { forceReplaceUrl: 'proxy.example.test/api', autofillRequestUrl: true }, messages: chat, declines: 'url-form' })
    add('autofill-encoded-query-name-declines', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/api?api%2Dversion=1', autofillRequestUrl: true }, messages: chat, declines: 'url-form' })
    add('autofill-only-for-reverse-proxy', { model: 'xcustom:::one', db: { autofillRequestUrl: true }, customModels: [customModel({ url: 'https://llm.example.test/api' })], messages: chat })
    add('custom-model', { model: 'xcustom:::one', customModels: [customModel()], messages: chat })
    add('custom-model-stream', { model: 'xcustom:::one', stream: true, customModels: [customModel()], messages: chat })
    add('custom-model-no-url-declines', { model: 'xcustom:::one', customModels: [customModel({ url: '' })], messages: chat, declines: 'url-form' })
    add('custom-model-empty-key-kept', { model: 'xcustom:::one', customModels: [customModel({ key: '' })], messages: chat })
    add('nanogpt-declines', { model: 'nanogpt', messages: chat, declines: 'format' })
    add('other-format-declines', { model: 'gpt4o', messages: chat, declines: 'format' })
    add('url-userinfo-buffered-declines', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://user:pw@proxy.example.test/v1/responses' }, messages: chat, declines: 'url-form' })
    add('url-userinfo-stream-is-raw', { model: 'reverse_proxy', stream: true, db: { forceReplaceUrl: 'https://user:pw@proxy.example.test/v1/responses' }, messages: chat })
    add('empty-messages', { messages: [] })
    add('system-only', { messages: [sys('Only a system prompt.')] })

    write('requests', cases)
}

const text = value => ({ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: value, annotations: [] }] })
const reasoningItem = extra => ({ type: 'reasoning', id: 'rs_1', ...extra })

function writeResponses() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: G, messages: chat, ...entry })
    const ok = json => ({ replies: [{ json }] })
    const status = (code, body) => ({ replies: [{ status: code, json: body }] })

    add('output-text-message', ok({ output: [text('Hello there.')] }))
    add('two-messages-join-with-newline', ok({ output: [text('One.'), text('Two.')] }))
    add('message-with-two-parts', ok({ output: [{ type: 'message', content: [{ type: 'output_text', text: 'A' }, { type: 'output_text', text: 'B' }] }] }))
    add('top-level-output-text-wins', ok({ output_text: 'Top.', output: [text('Message.')] }))
    add('top-level-output-text-empty-string', ok({ output_text: '', output: [text('Message.')] }))
    add('top-level-output-text-alone', ok({ output_text: 'Only top.' }))
    add('empty-text-part-skipped', ok({ output: [{ type: 'message', content: [{ type: 'output_text', text: '' }, { type: 'output_text', text: 'Kept.' }] }] }))
    add('non-message-items-ignored', ok({ output: [{ type: 'web_search_call', id: 'ws_1' }, text('Kept.')] }))
    add('null-and-primitive-items-ignored', ok({ output: [null, 5, 'x', text('Kept.')] }))
    add('message-without-content', ok({ output: [{ type: 'message' }, text('Kept.')] }))
    add('message-content-null', ok({ output: [{ type: 'message', content: null }, text('Kept.')] }))
    add('content-part-null-ignored', ok({ output: [{ type: 'message', content: [null, { type: 'output_text', text: 'Kept.' }] }] }))
    add('unicode-text', ok({ output: [text('日本語 \u{1F600} "q" \\ \n')] }))

    // Reasoning.
    add('reasoning-summary-array', ok({ output: [reasoningItem({ summary: [{ type: 'summary_text', text: 'Thought A.' }, { type: 'summary_text', text: 'Thought B.' }] }), text('Answer.')] }))
    add('reasoning-summary-string', ok({ output: [reasoningItem({ summary: 'Plain summary.' }), text('Answer.')] }))
    add('reasoning-content-array', ok({ output: [reasoningItem({ content: [{ type: 'reasoning_text', text: 'Deep.' }] }), text('Answer.')] }))
    add('reasoning-direct-keys', ok({ output: [reasoningItem({ text: 'T1', summary_text: 'T2', reasoning_text: 'T3', reasoning: 'T4' }), text('Answer.')] }))
    add('reasoning-nested-arrays-and-keys', ok({ output: [reasoningItem({ summary: [[{ summary: 'Inner.' }], { reasoning: 'R.' }, null, 5, ''] }), text('Answer.')] }))
    add('reasoning-empty', ok({ output: [reasoningItem({ summary: [] }), text('Answer.')] }))
    add('reasoning-two-items', ok({ output: [reasoningItem({ summary: 'First.' }), reasoningItem({ summary: 'Second.' }), text('Answer.')] }))
    add('reasoning-only', ok({ output: [reasoningItem({ summary: 'Only thinking.' })] }))
    add('reasoning-after-text', ok({ output: [text('Answer.'), reasoningItem({ summary: 'Late.' })] }))
    add('reasoning-skipped-when-text-has-thoughts-tag', ok({ output: [reasoningItem({ summary: 'Hidden.' }), text('<Thoughts>\nOwn\n</Thoughts>\nAnswer.')] }))

    // Refusals.
    add('refusal-only', ok({ output: [{ type: 'message', content: [{ type: 'refusal', refusal: 'No.' }] }] }))
    add('two-refusals-join', ok({ output: [{ type: 'message', content: [{ type: 'refusal', refusal: 'No.' }, { type: 'refusal', refusal: 'Never.' }] }] }))
    add('refusal-with-text-is-ignored', ok({ output: [{ type: 'message', content: [{ type: 'refusal', refusal: 'No.' }, { type: 'output_text', text: 'But yes.' }] }] }))
    add('refusal-ignores-reasoning', ok({ output: [reasoningItem({ summary: 'Hidden.' }), { type: 'message', content: [{ type: 'refusal', refusal: 'No.' }] }] }))
    add('refusal-empty-ignored', ok({ output: [{ type: 'message', content: [{ type: 'refusal', refusal: '' }] }] }))

    // Structured output.
    add('extract-json', { extractJson: 'answer', schema, ...ok({ output: [text('{"answer":"yes","score":1}')] }) })
    add('extract-json-with-reasoning', { extractJson: 'answer', schema, ...ok({ output: [reasoningItem({ summary: 'Hmm.' }), text('{"answer":"yes"}')] }) })
    add('extract-json-invalid-text', { extractJson: 'answer', schema, ...ok({ output: [text('not json')] }) })
    add('extract-json-skipped-for-refusal', { extractJson: 'answer', schema, ...ok({ output: [{ type: 'message', content: [{ type: 'refusal', refusal: 'No.' }] }] }) })
    add('extract-json-without-schema-is-plain', { extractJson: 'answer', ...ok({ output: [text('{"answer":"yes"}')] }) })

    // Status and errors.
    add('status-failed', ok({ status: 'failed', error: { code: 'server_error', message: 'Boom.' } }))
    add('status-failed-without-error', ok({ status: 'failed', id: 'resp_1' }))
    add('error-field-with-completed-status', ok({ status: 'completed', error: { message: 'Odd.' }, output: [text('Ignored.')] }))
    add('error-field-string', ok({ error: 'plain' }))
    add('error-field-null-is-ignored', ok({ error: null, output: [text('Kept.')] }))
    add('status-incomplete-with-text', ok({ status: 'incomplete', incomplete_details: { reason: 'max_output_tokens' }, output: [text('Cut off')] }))
    add('status-incomplete-without-text', ok({ status: 'incomplete', incomplete_details: { reason: 'content_filter' }, output: [] }))
    add('status-incomplete-without-details', ok({ status: 'incomplete', output: [text('Cut')] }))
    add('status-incomplete-without-anything', ok({ status: 'incomplete' }))
    add('status-queued-is-read-as-complete', ok({ status: 'queued', output: [text('Hi.')] }))
    add('empty-output', ok({ status: 'completed', output: [] }))
    add('empty-output-with-reason', ok({ status: 'completed', incomplete_details: { reason: 'other' }, output: [] }))
    add('no-output-key', ok({ id: 'resp_1', usage: { total_tokens: 3 } }))
    add('reply-null', ok(null))
    add('reply-array', ok([1, 2]))
    add('reply-string', ok('text'))
    add('reply-number', ok(5))
    add('empty-reply-echoes-numbers', { replies: [{ text: '{"x":1.0,"y":1e21,"z":0.1,"big":12345678901234567890,"s":"\\u00e9","t":[1,2,{"a":null}]}' }] })

    // HTTP failures.
    add('http-400-json', status(400, { error: { message: 'Bad key.', type: 'invalid_request_error' } }))
    add('http-401-text', { replies: [{ status: 401, text: 'unauthorized' }] })
    add('http-500-empty', { replies: [{ status: 500, text: '' }] })
    add('http-500-html', { replies: [{ status: 500, text: '<html>oops</html>' }] })
    add('http-429-json', status(429, { error: { message: 'Slow down.' } }))
    add('http-200-not-json', { replies: [{ text: 'not json' }] })
    add('http-200-empty-body', { replies: [{ text: '' }] })
    add('http-200-bom', { replies: [{ text: '﻿' + JSON.stringify({ output: [text('Bom.')] }) }] })
    add('http-201-ok', status(201, { output: [text('Created.')] }))
    add('transport-error', { replies: [{ throws: 'fetch failed' }] })
    add('http-error-long-message', status(400, { error: { message: 'x'.repeat(3000) } }))

    // Replies the native path leaves to the TypeScript one.
    const functionCall = { type: 'function_call', id: 'fc_1', call_id: 'call_1', name: 'lookup', arguments: '{"query":"x"}', status: 'completed' }
    add('function-call-declines', { tools: [lookup], toolReplies: [[{ type: 'text', text: '{"found":true}' }]], replies: [{ json: { output: [functionCall] } }, { json: { output: [text('Done.')] } }], declines: 'tool-round' })
    add('function-call-without-name-is-ignored', ok({ output: [{ type: 'function_call', call_id: 'call_1' }, text('Kept.')] }))
    add('function-call-without-call-id-is-ignored', ok({ output: [{ type: 'function_call', name: 'lookup' }, text('Kept.')] }))
    add('output-string-declines', ok({ output: 'text' }), )
    cases[cases.length - 1].declines = 'response-output-not-array'
    add('output-object-declines', ok({ output: { a: 1 } }))
    cases[cases.length - 1].declines = 'response-output-not-array'
    add('content-string-declines', ok({ output: [{ type: 'message', content: 'text' }] }))
    cases[cases.length - 1].declines = 'response-content-not-array'
    add('text-number-declines', ok({ output: [{ type: 'message', content: [{ type: 'output_text', text: 5 }] }] }))
    cases[cases.length - 1].declines = 'response-text-not-text'
    add('refusal-object-declines', ok({ output: [{ type: 'message', content: [{ type: 'refusal', refusal: { a: 1 } }] }] }))
    cases[cases.length - 1].declines = 'response-text-not-text'

    write('responses', cases)
}

function writeStreams() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: G, stream: true, messages: chat, ...entry })
    const ev = (type, extra = {}) => `event: ${type}\ndata: ${JSON.stringify({ type, ...extra })}\n\n`
    const delta = value => ev('response.output_text.delta', { delta: value })
    const reasoning = value => ev('response.reasoning_summary_text.delta', { delta: value })
    const reply = (...chunks) => ({ chunks: chunks.map(utf8 => ({ utf8 })) })
    const hexReply = (...values) => ({ chunks: values.map(value => ({ hex: Buffer.from(value, 'utf8').toString('hex') })) })
    const completed = response => ev('response.completed', { response })
    const done = 'data: [DONE]\n\n'

    add('text-one-chunk', { replies: [reply(delta('Hello') + delta(' world'))] })
    add('text-chunk-per-event', { replies: [reply(delta('Hello'), delta(' world'))] })
    add('text-split-mid-event', { replies: [reply('event: x\ndata: {"type":"response.output_text.delta","del', 'ta":"Hi"}\n\n', delta('!'))] })
    add('text-split-mid-separator', { replies: [reply('data: {"type":"response.output_text.delta","delta":"A"}\n', '\ndata: {"type":"response.output_text.delta","delta":"B"}\n\n')] })
    add('text-crlf', { replies: [reply(delta('A').replaceAll('\n', '\r\n'), delta('B').replaceAll('\n', '\r\n'))] })
    add('text-mixed-separators', { replies: [reply('data: {"type":"response.output_text.delta","delta":"A"}\r\n\n', 'data: {"type":"response.output_text.delta","delta":"B"}\n\r\n')] })
    add('text-three-newlines', { replies: [reply('data: {"type":"response.output_text.delta","delta":"A"}\n\n\ndata: {"type":"response.output_text.delta","delta":"B"}\n\n')] })
    add('last-event-without-blank-line', { replies: [reply(delta('A'), 'data: {"type":"response.output_text.delta","delta":"B"}')] })
    add('last-event-with-one-newline', { replies: [reply(delta('A'), 'data: {"type":"response.output_text.delta","delta":"B"}\n')] })
    add('trailing-whitespace-only-is-ignored', { replies: [reply(delta('A'), ' \n ')] })
    add('data-without-space', { replies: [reply('data:{"type":"response.output_text.delta","delta":"A"}\n\n')] })
    add('data-with-two-spaces', { replies: [reply('data:  {"type":"response.output_text.delta","delta":"A"}\n\n')] })
    add('data-with-tab', { replies: [reply('data:\t{"type":"response.output_text.delta","delta":"A"}\n\n')] })
    add('data-with-nbsp', { replies: [reply('data: {"type":"response.output_text.delta","delta":"A"}\n\n')] })
    add('multi-line-data-joins-with-newline', { replies: [reply('data: {"type":"response.output_text.delta",\ndata: "delta":"A"}\n\n')] })
    add('event-without-data-gives-no-snapshot', { replies: [reply('event: ping\n\n', delta('A'), ': comment\n\n')] })
    add('event-with-other-fields', { replies: [reply('id: 1\nretry: 5\nevent: x\ndata: {"type":"response.output_text.delta","delta":"A"}\n\n')] })
    add('done-line', { replies: [reply(delta('A'), done)] })
    add('done-line-then-more', { replies: [reply(delta('A'), done, delta('B'))] })
    add('malformed-json-gives-a-repeat', { replies: [reply(delta('A'), 'data: {broken\n\n', delta('B'))] })
    add('empty-data-gives-a-repeat', { replies: [reply(delta('A'), 'data: \n\n', delta('B'))] })
    add('json-null-and-primitives-give-repeats', { replies: [reply('data: null\n\n', 'data: 5\n\n', 'data: "x"\n\n', 'data: []\n\n', delta('A'))] })
    add('unknown-event-type-gives-a-repeat', { replies: [reply(delta('A'), ev('response.created', { response: { id: 'r' } }), ev('response.in_progress', {}))] })
    add('empty-stream', { replies: [{ chunks: [] }] })
    add('empty-chunks', { replies: [reply('', delta('A'), '')] })
    add('multibyte-split-between-chunks', { replies: [{ chunks: [{ hex: Buffer.from('data: {"type":"response.output_text.delta","delta":"', 'utf8').toString('hex') + 'e3' }, { hex: '8182' + Buffer.from('"}\n\n', 'utf8').toString('hex') }] }] })
    add('bom-at-stream-start', { replies: [{ chunks: [{ hex: 'efbbbf' + Buffer.from(delta('A'), 'utf8').toString('hex') }, { hex: Buffer.from(delta('B'), 'utf8').toString('hex') }] }] })
    add('bom-inside-stream-stays', { replies: [{ chunks: [{ hex: Buffer.from(delta('A'), 'utf8').toString('hex') }, { hex: 'efbbbf' + Buffer.from(delta('B'), 'utf8').toString('hex') }] }] })
    add('unicode-text', { replies: [reply(delta('日本語'), delta(' \u{1F600}'))] })
    add('delta-null-and-missing-add-nothing', { replies: [reply(ev('response.output_text.delta', { delta: null }), ev('response.output_text.delta', {}), delta('A'))] })
    add('refusal-delta', { replies: [reply(ev('response.refusal.delta', { delta: 'No.' }), delta(' Yes.'))] })

    // Text completion events.
    add('text-done-replaces', { replies: [reply(delta('Hel'), ev('response.output_text.done', { text: 'Hello' }))] })
    add('text-done-matching-suffix-keeps', { replies: [reply(delta('Hello'), ev('response.output_text.done', { text: 'lo' }))] })
    add('text-done-empty-ignored', { replies: [reply(delta('Hello'), ev('response.output_text.done', { text: '' }))] })
    add('completed-replaces-text', { replies: [reply(delta('Hel'), completed({ output: [text('Hello final')] }))] })
    add('completed-with-top-level-text', { replies: [reply(delta('Hel'), completed({ output_text: 'Top' }))] })
    add('completed-empty-keeps-text', { replies: [reply(delta('Hello'), completed({ output: [] }))] })
    add('completed-with-null-response', { replies: [reply(delta('Hello'), ev('response.completed', { response: null }))] })
    add('completed-with-reasoning-resets-streamed-reasoning', { replies: [reply(reasoning('Streamed.'), completed({ output: [reasoningItem({ summary: 'Final.' }), text('Answer')] }))] })
    add('completed-text-with-thoughts-tag-clears-reasoning', { replies: [reply(reasoning('Streamed.'), completed({ output: [text('<Thoughts>\nOwn\n</Thoughts>\nAnswer')] }))] })
    add('completed-keeps-reasoning-without-tag', { replies: [reply(reasoning('Streamed.'), completed({ output: [text('Answer')] }))] })
    add('completed-extract-json', { extractJson: 'answer', schema, replies: [reply(completed({ output: [text('{"answer":"yes"}')] }))] })
    add('stream-extract-json', { extractJson: 'answer', schema, replies: [reply(delta('{"answer":'), delta('"yes"}'))] })
    add('stream-extract-json-partial-is-empty', { extractJson: 'answer', schema, replies: [reply(delta('{"answ'))] })

    // Reasoning events.
    add('reasoning-then-text', { replies: [reply(reasoning('Hmm.'), delta('Answer.'))] })
    add('reasoning-deltas-join-with-blank-line', { replies: [reply(reasoning('A.'), reasoning('B.'), delta('Done'))] })
    add('reasoning-repeat-suffix-skipped', { replies: [reply(reasoning('A.'), reasoning('A.'), reasoning('B.'))] })
    add('reasoning-text-delta', { replies: [reply(ev('response.reasoning_text.delta', { delta: 'Raw.' }), delta('x'))] })
    add('reasoning-legacy-delta', { replies: [reply(ev('response.reasoning.delta', { delta: 'Old.' }), delta('x'))] })
    add('reasoning-empty-delta-ignored', { replies: [reply(reasoning(''), delta('x'))] })
    add('reasoning-done-replaces', { replies: [reply(reasoning('Hm'), ev('response.reasoning_summary_text.done', { text: 'Hmm full.' }), delta('x'))] })
    add('reasoning-text-done-replaces', { replies: [reply(reasoning('Hm'), ev('response.reasoning_text.done', { text: 'Hmm full.' }))] })
    add('reasoning-done-matching-suffix-keeps', { replies: [reply(reasoning('Hmm full.'), ev('response.reasoning_summary_text.done', { text: 'full.' }))] })
    add('reasoning-only', { replies: [reply(reasoning('Only.'))] })

    // Errors inside the stream.
    add('error-event', { replies: [reply(delta('A'), ev('error', { error: { message: 'Bad.' } }), delta('B'))] })
    add('error-event-without-error-field', { replies: [reply(ev('error', { message: 'Bad.' }))] })
    add('response-failed-event', { replies: [reply(delta('A'), ev('response.failed', { response: { id: 'r' }, error: { code: 'x' } }))] })
    add('response-error-event', { replies: [reply(ev('response.error', { error: { code: 'x' } }))] })
    add('error-null-uses-event', { replies: [reply(ev('error', { error: null }))] })

    // Failures before the stream starts.
    add('status-500', { replies: [{ status: 500, text: 'upstream broke' }] })
    add('status-400-json', { replies: [{ status: 400, text: JSON.stringify({ error: { message: 'Bad key.' } }) }] })
    add('status-500-empty', { replies: [{ status: 500, text: '' }] })
    add('status-200-not-an-event-stream', { replies: [{ text: JSON.stringify({ output: [text('Plain.')] }) }] })
    add('status-200-chunks-with-json-type', { replies: [{ contentType: 'application/json', chunks: [{ utf8: delta('A') }] }] })
    add('status-200-content-type-with-charset', { replies: [{ contentType: 'text/event-stream; charset=utf-8', chunks: [{ utf8: delta('A') }] }] })
    add('status-201-is-not-a-stream', { replies: [{ status: 201, text: 'created' }] })
    add('status-500-bom', { replies: [{ status: 500, text: '﻿broken' }] })

    // Replies the native path leaves to the TypeScript one.
    const functionItem = { type: 'function_call', id: 'fc_1', call_id: 'call_1', name: 'lookup', arguments: '{"query":"x"}', status: 'completed' }
    const followUp = reply(delta('Done.'))
    add('function-call-item-declines', { tools: [lookup], toolReplies: [[{ type: 'text', text: '{"found":true}' }]], replies: [reply(delta('A'), ev('response.output_item.done', { item: functionItem })), followUp], declines: 'tool-round', terminal: true })
    add('function-call-arguments-delta-declines', { tools: [lookup], toolReplies: [[{ type: 'text', text: '{}' }]], replies: [reply(delta('A'), ev('response.function_call_arguments.delta', { call_id: 'call_1', delta: '{}' })), followUp], declines: 'tool-round', terminal: true })
    add('function-call-in-completed-declines', { tools: [lookup], toolReplies: [[{ type: 'text', text: '{}' }]], replies: [reply(completed({ output: [functionItem] })), followUp], declines: 'tool-round', terminal: true })
    add('delta-number-declines', { replies: [reply(ev('response.output_text.delta', { delta: 5 }))], declines: 'stream-value-not-text' })
    add('done-text-number-declines', { replies: [reply(delta('A'), ev('response.output_text.done', { text: 5 }))], declines: 'stream-value-not-text', terminal: true })
    add('reasoning-delta-number-declines', { replies: [reply(ev('response.reasoning_summary_text.delta', { delta: 5 }))], declines: 'stream-value-not-text' })
    add('completed-output-string-declines', { replies: [reply(ev('response.completed', { response: { output: 'x' } }))], declines: 'response-output-not-array' })

    write('streams', cases)
}

writeRequests()
writeResponses()
writeStreams()
