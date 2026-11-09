// Writes the case files the Anthropic oracle (`providers-anthropic.oracle.test.ts`) reads: `requests`, `responses` and
// `streams` under tests/fixtures/rust-core/providers/anthropic. The cases are deterministic and use synthetic keys only.
// Run `node benchmarks/rust-core-oracles/providers-anthropic.cases.mjs` and then the oracle with RUST_CORE_ORACLE_WRITE=1
// to refresh the expectations.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultOut = resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/anthropic')
const out = process.argv[2] ?? defaultOut
mkdirSync(out, { recursive: true })

// LLMFlags numbers (src/ts/model/types.ts); custom flags replace the flags of the selected model.
const F = { hasImageInput: 0, hasPrefill: 4, hasCache: 5, hasFullSystemPrompt: 6, hasFirstSystemPrompt: 7, hasStreaming: 8, requiresAlternateRole: 9, mustStartWithUserInput: 10, claudeThinking: 21, claudeAdaptiveThinking: 22, claudeXHighEffort: 23, claudeThinkingOnByDefault: 27 }
const flags = (...names) => ({ enableCustomFlags: true, customFlags: names.map(name => F[name]) })

const sys = text => ({ role: 'system', content: text })
const user = (text, extra = {}) => ({ role: 'user', content: text, ...extra })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const image = (base64 = 'data:image/png;base64,AAAA') => ({ type: 'image', base64 })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const baseDb = {
    usePlainFetch: true,
    claudeAPIKey: 'sk-ant-synthetic',
    proxyKey: 'sk-proxy-synthetic',
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
    thinkingType: 'budget',
    adaptiveThinkingEffort: 'high',
    claude1HourCaching: false,
    claudeBatching: false,
    claudeRetrivalCaching: false,
    antiServerOverloads: false,
    jsonSchemaEnabled: false,
    jsonSchema: 'answer',
    extractJson: '',
    seperateParametersEnabled: false,
    seperateParametersByModel: false,
    simplifiedToolUse: false,
    autofillRequestUrl: false,
    systemContentReplacement: '',
    systemRoleReplacement: '',
    forceReplaceUrl: '',
    customProxyRequestModel: 'claude-proxy-model',
    customAPIFormat: 2,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

const separate = {
    seperateParametersEnabled: true,
    seperateParameters: {
        model: { temperature: 50, top_k: 10, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.5, frequency_penalty: 10, presence_penalty: 10, thinking_tokens: 2048, reasoning_effort: 0, verbosity: 0 },
        submodel: { temperature: 60, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        memory: { temperature: 61, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        emotion: { temperature: 62, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        translate: { temperature: 63, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        otherAx: { temperature: 70, top_k: 12, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.7, frequency_penalty: 12, presence_penalty: 12, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        overrides: { 'claude-opus-4-6': { temperature: 30, top_k: 5, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.3, frequency_penalty: 5, presence_penalty: 5, thinking_tokens: 4096, reasoning_effort: 0, verbosity: 0 } },
    },
}

const customModel = (extra = {}) => ({ id: 'xcustom:::one', name: 'One', internalId: 'my-claude', url: 'https://llm.example.test/v1/messages', key: 'sk-custom-synthetic', format: 2, flags: [F.hasFirstSystemPrompt], tokenizer: 0, params: '', ...extra })

const lookup = { name: 'lookup', description: 'Looks a word up.', inputSchema: { type: 'object', properties: { query: { type: 'string', description: 'The word.' } }, required: ['query'] } }
const clock = { name: 'clock', description: 'Tells the time.', inputSchema: { type: 'object', properties: { zone: { type: ['string', 'null'] } } } }

const A = 'claude-opus-4-6'
const S = 'claude-3-5-sonnet-latest'

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: A, ...entry })

    // Layout, system handling and message merging.
    add('basic', { messages: chat })
    add('stream', { stream: true, messages: chat })
    add('max-tokens', { maxTokens: 777, messages: chat })
    add('no-system', { messages: [user('Hello')] })
    add('multi-turn', { messages: talk })
    add('system-only-becomes-start', { db: flags('hasFullSystemPrompt'), messages: [sys('Only a system prompt.')] })
    add('assistant-first-gets-start', { messages: [sys('Be brief.'), bot('I begin.'), user('Hello')] })
    add('consecutive-users-merge', { messages: [sys('Be brief.'), user('One'), user('Two'), user('Three')] })
    add('consecutive-assistants-merge', { messages: [sys('Be brief.'), user('One'), bot('Two'), bot('Three'), user('Four')] })
    add('system-after-chat-becomes-user', { messages: [sys('Be brief.'), user('One'), sys('Reminder'), bot('Two')] })
    add('system-after-chat-merges-into-user', { messages: [sys('Be brief.'), user('One'), sys('Reminder'), user('Two')] })
    add('two-leading-systems', { messages: [sys('First.'), sys('Second.'), user('Hello')] })
    add('full-system-prompt-flag-keeps-systems', { db: flags('hasFullSystemPrompt', 'hasImageInput'), messages: [sys('First.'), user('Hello'), sys('Middle.'), bot('Hi')] })
    add('full-system-prompt-flag-system-merge', { db: flags('hasFullSystemPrompt'), messages: [sys('First.'), sys('Second.'), user('Hello')] })
    add('system-prompt-padded', { messages: [sys('  padded  '), user('Hello')] })
    add('empty-system-content', { db: flags('hasFullSystemPrompt'), messages: [sys(''), user('Hello')] })
    add('whitespace-only-system-becomes-start', { db: flags('hasFullSystemPrompt'), messages: [sys('   ')] })
    add('function-role-ignored', { messages: [sys('Be brief.'), { role: 'function', content: 'result', name: 'lookup' }, user('Hello')] })
    add('legacy-system-replacement', { model: 'claude-2.1', messages: [sys('Be brief.'), user('Hello')] })
    add('legacy-no-input-fails', { model: 'claude-2.1', messages: [] })
    add('legacy-function-only-fails', { model: 'claude-2.1', messages: [{ role: 'function', content: 'x' }] })
    add('first-system-only-throws', { messages: [sys('Only.')] })
    add('empty-content-message', { messages: [sys('Be brief.'), user('')] })
    add('system-content-replacement', { db: { systemContentReplacement: '[{{slot}}]', systemRoleReplacement: 'user', ...flags('hasPrefill') }, messages: [sys('Be brief.'), user('Hello')] })
    add('alternate-role-flag', { db: flags('hasFirstSystemPrompt', 'requiresAlternateRole'), messages: [sys('Be brief.'), user('One'), user('Two'), bot('Three')] })

    // Prompt caching.
    add('cache-point-user', { messages: [sys('Be brief.'), user('Hello', { cachePoint: true })] })
    add('cache-point-assistant', { messages: [sys('Be brief.'), user('Hello'), bot('Hi', { cachePoint: true })] })
    add('cache-point-on-merged-message', { messages: [sys('Be brief.'), user('One'), user('Two', { cachePoint: true })] })
    add('cache-point-then-merge-keeps-mark', { messages: [sys('Be brief.'), user('One', { cachePoint: true }), user('Two')] })
    add('cache-point-system-after-chat', { messages: [sys('Be brief.'), user('One'), sys('Reminder', { cachePoint: true })] })
    add('cache-one-hour', { db: { claude1HourCaching: true }, messages: [sys('Be brief.'), user('Hello', { cachePoint: true })] })
    add('cache-one-hour-merged', { db: { claude1HourCaching: true }, messages: [sys('Be brief.'), user('One'), user('Two', { cachePoint: true })] })
    add('cache-one-hour-custom-beta', { db: { claude1HourCaching: true, applyAdditionalParamsToAll: true, additionalParams: [['header::Anthropic-Beta', 'custom-beta']] }, messages: chat })

    // Images.
    add('image-single', { messages: [sys('Be brief.'), user('Look.', { multimodals: [image()] })] })
    add('image-two-reverse-order', { messages: [sys('Be brief.'), user('Look.', { multimodals: [image('data:image/png;base64,ONE'), image('data:image/jpeg;base64,TWO')] })] })
    add('image-with-cache-marks-the-image', { messages: [sys('Be brief.'), user('Look.', { multimodals: [image()], cachePoint: true })] })
    add('image-in-merged-message', { messages: [sys('Be brief.'), user('One'), user('Look.', { multimodals: [image()] })] })
    add('image-in-assistant-message', { messages: [sys('Be brief.'), user('One'), bot('Seen.', { multimodals: [image()] })] })
    add('image-without-comma', { messages: [sys('Be brief.'), user('Look.', { multimodals: [image('plain')] })] })
    add('image-without-media-type', { messages: [sys('Be brief.'), user('Look.', { multimodals: [image('base64,AAAA')] })] })
    add('video-ignored', { messages: [sys('Be brief.'), user('Look.', { multimodals: [{ type: 'video', base64: 'data:video/mp4;base64,AAAA' }] })] })

    // Sampling and thinking.
    add('sampling-disabled', { db: { temperature: -1000 }, messages: chat })
    add('legacy-model-sampling', { model: S, messages: chat })
    add('separate-parameters-by-mode', { mode: 'memory', db: separate, messages: chat })
    add('separate-parameters-submodel', { mode: 'submodel', db: separate, messages: chat })
    add('separate-parameters-by-model', { db: { ...separate, seperateParametersByModel: true }, messages: chat })
    add('thinking-budget', { messages: chat })
    add('thinking-budget-zero-removed', { db: { thinkingTokens: 0 }, messages: chat })
    add('thinking-off', { db: { thinkingType: 'off' }, messages: chat })
    add('thinking-off-on-by-default-model', { model: 'claude-opus-5', db: { thinkingType: 'off' }, messages: chat })
    add('thinking-on-by-default-model-adaptive', { model: 'claude-opus-5', db: { thinkingType: 'adaptive' }, messages: chat })
    add('thinking-adaptive', { db: { thinkingType: 'adaptive' }, messages: chat })
    add('thinking-adaptive-low-effort', { db: { thinkingType: 'adaptive', adaptiveThinkingEffort: 'low' }, messages: chat })
    add('thinking-adaptive-xhigh-unsupported', { db: { thinkingType: 'adaptive', adaptiveThinkingEffort: 'xhigh' }, messages: chat })
    add('thinking-adaptive-xhigh-supported', { model: 'claude-opus-4-8', db: { thinkingType: 'adaptive', adaptiveThinkingEffort: 'xhigh' }, messages: chat })
    add('thinking-adaptive-effort-missing', { model: 'claude-opus-4-8', db: { thinkingType: 'adaptive', adaptiveThinkingEffort: null }, messages: chat })
    add('thinking-adaptive-only-model-under-budget-setting', { model: 'claude-opus-4-7', messages: chat })
    add('thinking-adaptive-without-flag-uses-budget', { db: { thinkingType: 'adaptive', ...flags('hasFirstSystemPrompt', 'claudeThinking') }, messages: chat })
    add('thinking-budget-without-parameter', { model: S, messages: chat })
    add('thinking-budget-5k', { model: 'claude-sonnet-4-5-20250929', db: { thinkingTokens: 5000 }, maxTokens: 9000, messages: chat })

    // Tools.
    add('tools-one', { tools: [lookup], messages: chat })
    add('tools-two-simplified-schema', { tools: [lookup, clock], messages: chat })
    add('tools-stream', { tools: [lookup], stream: true, messages: chat })

    // Betas and headers.
    add('large-max-tokens-beta', { maxTokens: 20000, messages: chat })
    add('large-max-tokens-and-cache-beta', { maxTokens: 20000, db: { claude1HourCaching: true }, messages: chat })
    add('max-tokens-at-limit-no-beta', { maxTokens: 8192, messages: chat })
    // Without plain fetch the web build sends the request through the Risu proxy; only the body and the preview are compared.
    add('plain-fetch-off', { db: { usePlainFetch: false }, messages: chat, transport: 'risu-proxy' })

    // Additional parameters.
    const extra = (pairs, more = {}) => ({ db: { applyAdditionalParamsToAll: true, additionalParams: pairs, ...more }, messages: chat })
    add('additional-number', extra([['top_k', '5']]))
    add('additional-json', extra([['metadata', 'json::{"user_id":"u1"}']]))
    add('additional-string-and-bool', extra([['label', '"hello"'], ['flag', 'true'], ['nothing', 'null']]))
    add('additional-dotted', extra([['thinking.budget_tokens', '2000'], ['extra.deep.value', '1']]))
    add('additional-header', extra([['header::x-trace', 'abc']]))
    add('additional-header-replaces-api-key', extra([['header::x-api-key', 'sk-other-synthetic']]))
    add('additional-none-removes-body-key', extra([['temperature', '{{none}}']]))
    add('additional-none-removes-header', extra([['header::accept', '{{none}}']]))
    add('additional-model-override', extra([['model', 'claude-override']]))
    add('additional-max-tokens-string-beta', extra([['max_tokens', '"9000"']]))
    add('additional-not-applied-without-flag', { db: { additionalParams: [['top_k', '5']] }, messages: chat })
    add('additional-custom-beta-header', extra([['header::anthropic-beta', 'my-beta']], {}))

    // Tool-call markers inside message content.
    const call = (id, name, arg) => ({ call: { id, name, arg }, response: [{ type: 'text', text: `${name} done` }] })
    add('marker-decoded', { messages: [sys('Be brief.'), user('Go.'), bot('Before <tool_call>one</tool_call> after'), user('Next.')], decodings: { one: call('toolu_1', 'lookup', { query: 'x' }) } })
    add('marker-only-text', { messages: [sys('Be brief.'), user('Go.'), bot('<tool_call>one</tool_call>'), user('Next.')], decodings: { one: call('toolu_1', 'lookup', { query: 'x' }) } })
    add('marker-two-in-one-message', { messages: [sys('Be brief.'), user('Go.'), bot('A<tool_call>one</tool_call>B<tool_call>two</tool_call>C'), user('Next.')], decodings: { one: call('toolu_1', 'lookup', { query: 'x' }), two: call('toolu_2', 'clock', {}) } })
    add('marker-undecodable-removed', { messages: [sys('Be brief.'), user('Go.'), bot('A<tool_call>nope</tool_call>B'), user('Next.')] })
    add('marker-across-line-not-matched', { messages: [sys('Be brief.'), user('Go.'), bot('A<tool_call>o\nne</tool_call>B'), user('Next.')], decodings: { 'o\nne': call('toolu_1', 'lookup', {}) } })
    add('marker-in-user-message', { messages: [sys('Be brief.'), user('Go <tool_call>one</tool_call>'), user('Next.')], decodings: { one: call('toolu_1', 'lookup', { query: 'x' }) } })
    add('marker-image-and-unsupported-response', {
        messages: [sys('Be brief.'), user('Go.'), bot('<tool_call>one</tool_call>'), user('Next.')],
        decodings: { one: { call: { id: 'toolu_1', name: 'lookup', arg: { query: 'x' } }, response: [{ type: 'text', text: 'T' }, { type: 'image', mimeType: 'image/png', data: 'QUJD' }, { type: 'resource', uri: 'u' }] } },
    })
    add('marker-lazy-match', { messages: [sys('Be brief.'), user('Go.'), bot('<tool_call>a</tool_call>x</tool_call>'), user('Next.')], decodings: { a: call('toolu_1', 'lookup', {}) } })

    // Reverse proxy and custom models.
    const proxy = (id, entry) => add(id, { model: 'reverse_proxy', ...entry })
    proxy('proxy-basic', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages' }, messages: chat })
    proxy('proxy-flattens-content', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages' }, messages: [sys('Be brief.'), user('Hello', { cachePoint: true }), bot('Hi')] })
    proxy('proxy-image-message-becomes-null', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages' }, messages: [sys('Be brief.'), user('Look.', { multimodals: [image()] })] })
    proxy('proxy-autofill-v1', { db: { autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/v1' }, messages: chat })
    proxy('proxy-autofill-v1-slash', { db: { autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/v1/' }, messages: chat })
    proxy('proxy-autofill-host-only', { db: { autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test' }, messages: chat })
    proxy('proxy-autofill-host-slash', { db: { autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/' }, messages: chat })
    proxy('proxy-autofill-path', { db: { autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/api' }, messages: chat })
    proxy('proxy-autofill-messages', { db: { autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/x/messages' }, messages: chat })
    proxy('proxy-autofill-messages-slash', { db: { autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/x/messages/' }, messages: chat })
    proxy('proxy-no-autofill-keeps-url', { db: { forceReplaceUrl: 'https://proxy.example.test/api' }, messages: chat })
    // An empty URL is not nullish, so `??` keeps it and `new URL('')` throws.
    proxy('proxy-empty-url-is-kept', { db: { forceReplaceUrl: '' }, messages: chat, declines: 'url-form' })
    proxy('proxy-empty-key-falls-back', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages', proxyKey: '' }, messages: chat })
    proxy('proxy-additional-params', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages', additionalParams: [['top_k', '7'], ['header::x-route', 'r']] }, messages: chat })
    proxy('proxy-nanogpt-messages-format', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages', customAPIFormat: 22 }, messages: chat })
    proxy('proxy-legacy-format', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages', customAPIFormat: 3 }, messages: chat })
    proxy('proxy-bedrock-format-takes-normal-path', { db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages', customAPIFormat: 17 }, messages: chat })
    add('custom-model', { model: 'xcustom:::one', customModels: [customModel()], messages: chat })
    add('custom-model-stream', { model: 'xcustom:::one', stream: true, customModels: [customModel()], messages: chat })
    add('custom-model-empty-url-is-kept', { model: 'xcustom:::one', customModels: [customModel({ url: '' })], messages: chat, declines: 'url-form' })
    add('custom-model-empty-key-falls-back', { model: 'xcustom:::one', customModels: [customModel({ key: '' })], messages: chat })
    add('custom-model-nanogpt-messages', { model: 'xcustom:::one', customModels: [customModel({ format: 22 })], messages: chat })
    add('custom-model-image', { model: 'xcustom:::one', customModels: [customModel({ flags: [F.hasFirstSystemPrompt, F.hasImageInput] })], messages: [sys('Be brief.'), user('Look.', { multimodals: [image()] })] })
    add('custom-model-params', { model: 'xcustom:::one', customModels: [customModel({ params: 'top_k=9\nheader::x-custom=yes' })], messages: chat })
    add('custom-model-unicode-url', { model: 'xcustom:::one', customModels: [customModel({ url: 'https://llm.example.test/v1/me ssages?a=b c' })], messages: chat })
    add('custom-model-userinfo-url', { model: 'xcustom:::one', customModels: [customModel({ url: 'https://user:pw@llm.example.test/v1/messages' })], messages: chat, declines: 'url-form' })

    // Variants the native path hands back to TS.

    writeFileSync(`${out}/requests.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}


// Buffered responses, including the tool round `requestClaudeHTTP` runs.
function writeResponses() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: A, messages: chat, ...entry })
    const ok = content => ({ json: { id: 'msg_1', type: 'message', role: 'assistant', content } })
    const text = value => ({ type: 'text', text: value })
    const thinking = value => ({ type: 'thinking', thinking: value, signature: 'sig' })
    const use = (id, name, input, extra = {}) => ({ type: 'tool_use', id, name, input, ...extra })
    const toolDb = (more = {}) => ({ db: more, tools: [lookup, clock] })

    // Text and thinking.
    add('text-basic', { reply: ok([text('Hello there.')]) })
    add('text-blocks-concatenate', { reply: ok([text('One. '), text('Two.')]) })
    add('thinking-then-text', { reply: ok([thinking('Hmm.'), text('Done.')]) })
    add('thinking-only-closes-at-end', { reply: ok([thinking('Hmm.')]) })
    add('thinking-text-thinking', { reply: ok([thinking('A'), text('B'), thinking('C')]) })
    add('two-thinking-blocks-share-one-tag', { reply: ok([thinking('A'), thinking('B'), text('C')]) })
    add('redacted-thinking', { reply: ok([{ type: 'redacted_thinking', data: 'xx' }, text('Done.')]) })
    add('thinking-field-missing', { reply: ok([{ type: 'thinking' }, text('Done.')]) })
    add('thinking-field-null', { reply: ok([{ type: 'thinking', thinking: null }, text('Done.')]) })
    add('text-field-missing', { reply: ok([{ type: 'text' }]) })
    add('text-field-null', { reply: ok([{ type: 'text', text: null }]) })
    add('text-field-number', { reply: ok([{ type: 'text', text: 5 }]) })
    add('text-field-bool', { reply: ok([{ type: 'text', text: false }]) })
    add('text-field-array', { reply: ok([{ type: 'text', text: ['a', 'b'] }]) })
    add('unknown-and-non-object-blocks-ignored', { reply: ok([{ type: 'image' }, 'plain', 7, text('Kept.')]) })
    add('empty-text', { reply: ok([text('')]) })
    add('unicode-text', { reply: ok([text('Café 😀   end')]) })

    // Failures of the reply.
    add('content-empty-array', { reply: ok([]) })
    add('content-missing', { reply: { json: { id: 'msg_1', type: 'message' } } })
    add('content-null', { reply: { json: { content: null } } })
    add('content-empty-string', { reply: { json: { content: '' } } })
    add('content-string-declines', { reply: { json: { content: 'abc' } }, declines: 'content-not-array' })
    add('content-object-declines', { reply: { json: { content: { type: 'text', text: 'x' } } }, declines: 'content-not-array' })
    add('content-null-entry-declines', { reply: ok([text('A'), null]), declines: 'content-entry-null' })
    add('error-field-in-ok-response', { reply: { json: { type: 'error', error: { type: 'invalid_request_error', message: 'Bad.' } } } })
    add('error-field-string', { reply: { json: { error: 'plain' } } })
    add('error-field-overloaded', { reply: { json: { error: { type: 'overloaded_error', message: 'Overloaded' } } } })
    add('error-field-takes-precedence-over-content', { reply: { json: { error: { message: 'No.' }, content: [text('Yes.')] } } })
    add('http-500-json', { reply: { status: 500, json: { type: 'error', error: { type: 'api_error', message: 'Internal.' } } } })
    add('http-529-overloaded', { reply: { status: 529, json: { type: 'error', error: { type: 'overloaded_error', message: 'Overloaded' } } } })
    add('http-400-plain-text-body', { reply: { status: 400, contentType: 'text/plain', text: 'bad request' } })
    add('http-200-plain-text-body', { reply: { status: 200, contentType: 'text/plain', text: 'not json' } })
    add('http-502-html-body', { reply: { status: 502, contentType: 'text/html', text: '<html>Bad gateway</html>' } })
    add('http-200-empty-body', { reply: { status: 200, text: '' } })
    add('http-401-unicode-error', { reply: { status: 401, json: { error: { message: 'Clé invalide 😀' } } } })
    add('transport-error', { reply: { throws: 'fetch failed' } })
    add('data-null-throws', { reply: { text: 'null' } })
    add('data-string', { reply: { json: 'just a string' } })
    add('data-array', { reply: { json: [1, 2] } })
    add('data-number-zero', { reply: { json: 0 } })
    add('huge-error-message-is-bounded', { reply: { status: 500, json: { error: { message: 'e'.repeat(3000) } } } })

    // Extracting JSON.
    const extract = (id, content, more = {}) => add(id, { extractJson: 'answer', db: { jsonSchemaEnabled: true, jsonSchema: 'answer', ...more.db }, reply: ok([text(content)]), ...more.entry })
    extract('extract-json-plain', '{"answer":"yes"}')
    extract('extract-json-not-json-passthrough', 'no object here')
    extract('extract-json-invalid-json-passthrough', '{"answer":')
    extract('extract-json-missing-key', '{"other":"x"}')
    extract('extract-json-number-value', '{"answer":42}')
    extract('extract-json-null-value', '{"answer":null}')
    extract('extract-json-code-fence', '```json\n{"answer":"fenced"}\n```')
    extract('extract-json-after-thoughts', '{"answer":"after"}', { entry: { reply: ok([thinking('Think.'), text('{"answer":"after"}')]) } })
    extract('extract-json-padded', '  {"answer":"padded"}  ')
    extract('extract-json-dotted-object-declines', '{"answer":{"a":1}}', { db: { jsonSchema: 'answer.a' }, entry: { declines: 'extract-json-nested-object' } })
    add('extract-json-needs-the-setting', { extractJson: 'answer', db: { jsonSchemaEnabled: false }, reply: ok([text('{"answer":"yes"}')]) })
    add('extract-json-needs-the-argument', { db: { jsonSchemaEnabled: true, jsonSchema: 'answer' }, reply: ok([text('{"answer":"yes"}')]) })

    // Tool rounds.
    const done = ok([text('All done.')])
    const found = [[{ type: 'text', text: 'found' }]]
    add('tool-single', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: found })
    add('tool-with-leading-text', { ...toolDb(), replies: [ok([text('Let me look.'), use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: found })
    add('tool-keeps-unknown-block-fields', { ...toolDb(), replies: [ok([thinking('Plan.'), use('toolu_1', 'lookup', { query: 'x' }, { caller: { type: 'direct' } })]), done], toolReplies: found })
    add('tool-two-calls', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' }), use('toolu_2', 'clock', {})]), done], toolReplies: [[{ type: 'text', text: 'one' }], [{ type: 'text', text: 'two' }]] })
    add('tool-text-between-calls', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' }), text('Next.'), use('toolu_2', 'clock', {})]), done], toolReplies: [[{ type: 'text', text: 'one' }], [{ type: 'text', text: 'two' }]] })
    add('tool-image-and-other-results', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: [[{ type: 'text', text: 'T' }, { type: 'image', mimeType: 'image/png', data: 'QUJD' }, { type: 'resource', uri: 'u' }]] })
    add('tool-empty-result', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: [[]] })
    add('tool-second-round', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), ok([use('toolu_2', 'clock', {})]), done], toolReplies: [[{ type: 'text', text: 'one' }], [{ type: 'text', text: 'two' }]] })
    add('tool-remembered', { ...toolDb(), rememberToolUsage: true, replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: found })
    add('tool-remembered-two-rounds', { ...toolDb(), rememberToolUsage: true, replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), ok([use('toolu_2', 'clock', {})]), done], toolReplies: [[{ type: 'text', text: 'one' }], [{ type: 'text', text: 'two' }]] })
    add('tool-remembered-then-follow-up-fails', { ...toolDb(), rememberToolUsage: true, replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), { status: 500, json: { error: { message: 'Down.' } } }], toolReplies: found })
    add('tool-follow-up-overloaded', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), { status: 529, json: { error: { type: 'overloaded_error', message: 'Overloaded' } } }], toolReplies: found })
    add('tool-follow-up-extract-json', { ...toolDb({ jsonSchemaEnabled: true, jsonSchema: 'answer' }), extractJson: 'answer', replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), ok([text('{"answer":"final"}')])], toolReplies: found })
    add('tool-remembered-with-extract-json', { ...toolDb({ jsonSchemaEnabled: true, jsonSchema: 'answer' }), extractJson: 'answer', rememberToolUsage: true, replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), ok([text('{"answer":"final"}')])], toolReplies: found })
    add('tool-throws', { ...toolDb(), replies: [ok([use('toolu_1', 'lookup', { query: 'x' })])], toolReplies: [{ throws: 'tool failed' }] })
    add('tool-name-not-text-declines', { ...toolDb(), replies: [ok([use('toolu_1', 5, {})])], toolReplies: found, declines: 'tool-use-name-not-text' })
    add('tool-id-missing', { ...toolDb(), replies: [ok([{ type: 'tool_use', name: 'lookup', input: {} }]), done], toolReplies: found })
    add('tool-input-null', { ...toolDb(), replies: [ok([{ type: 'tool_use', id: 'toolu_1', name: 'lookup', input: null }]), done], toolReplies: found })
    add('tool-after-prefill-appends-to-the-assistant-message', { ...toolDb(), messages: [sys('Be brief.'), user('Hello'), bot('Sure, ')], replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: found })
    const proxyDb = { forceReplaceUrl: 'https://proxy.example.test/v1/messages' }
    add('tool-reverse-proxy-string-content', { ...toolDb(proxyDb), model: 'reverse_proxy', replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: found })
    add('tool-reverse-proxy-prefill', { ...toolDb(proxyDb), model: 'reverse_proxy', messages: [sys('Be brief.'), user('Hello'), bot('Sure, ')], replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: found })
    add('tool-reverse-proxy-null-message-declines', { ...toolDb(proxyDb), model: 'reverse_proxy', messages: [sys('Be brief.'), user('Look.', { multimodals: [image()] })], replies: [ok([use('toolu_1', 'lookup', { query: 'x' })])], toolReplies: found, declines: 'tool-round-message', terminal: true })
    add('tool-use-with-thinking-setting-off', { ...toolDb({ thinkingType: 'off' }), replies: [ok([use('toolu_1', 'lookup', { query: 'x' })]), done], toolReplies: found })

    writeFileSync(`${out}/responses.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

// Event streams.
function writeStreams() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: A, stream: true, messages: chat, ...entry })
    const ev = value => `data: ${JSON.stringify(value)}\n\n`
    const delta = (kind, field, value) => ev({ type: 'content_block_delta', index: 0, delta: { type: kind, [field]: value } })
    const textDelta = value => delta('text_delta', 'text', value)
    const thinkDelta = value => delta('thinking_delta', 'thinking', value)
    const start = ev({ type: 'message_start', message: { id: 'msg_1', role: 'assistant', content: [], usage: { input_tokens: 3 } } })
    const stop = ev({ type: 'message_stop' })
    const reply = (...chunks) => ({ chunks: chunks.map(utf8 => ({ utf8 })) })
    const hex = value => Buffer.from(value, 'utf8').toString('hex')
    const bom = 'efbbbf'
    const textBody = (value, extra = '') => `data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"${value}"${extra}}}\n\n`

    add('text-one-chunk', { reply: reply(start + textDelta('Hello') + textDelta(' world') + stop) })
    add('text-chunk-per-event', { reply: reply(start, textDelta('Hello'), textDelta(' world'), stop) })
    add('text-split-mid-line', { reply: reply('data: {"type":"content_block_delta","delta":{"type":"text_d', 'elta","text":"Hi"}}\n\n', stop) })
    add('text-chunk-ends-after-data-line', { reply: reply(textDelta('A').trimEnd() + '\n', '\n', textDelta('B'), stop) })
    add('text-last-line-without-newline', { reply: reply(textDelta('A'), 'data: ' + JSON.stringify({ type: 'content_block_delta', delta: { type: 'text_delta', text: 'B' } })) })
    add('text-delta-legacy-type', { reply: reply(ev({ type: 'content_block_delta', delta: { type: 'text', text: 'Old' } }), stop) })
    add('text-crlf', { reply: reply(textBody('A').replaceAll('\n', '\r\n') + textBody('B').replaceAll('\n', '\r\n')) })
    add('text-with-event-lines', { reply: reply('event: content_block_delta\n' + textDelta('A') + 'event: ping\ndata: {"type":"ping"}\n\n') })
    add('data-without-space-ignored', { reply: reply(textBody('A').replace('data: ', 'data:'), textDelta('B')) })
    add('malformed-json-ignored', { reply: reply('data: {"type":"content_block_delta","delta":{\n\n', textDelta('A'), 'data: not json\n\n', textDelta('B')) })
    add('json-null-and-number-lines', { reply: reply('data: null\n\n', 'data: 5\n\n', 'data: "text"\n\n', 'data: []\n\n', textDelta('A')) })
    add('unknown-delta-types-ignored', { reply: reply(delta('input_json_delta', 'partial_json', '{"a":'), delta('signature_delta', 'signature', 'sig'), textDelta('A')) })
    add('delta-missing', { reply: reply(ev({ type: 'content_block_delta' }), ev({ type: 'content_block_delta', delta: 'text' }), textDelta('A')) })
    add('tool-use-events-ignored', { tools: [lookup], reply: reply(start, ev({ type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'toolu_1', name: 'lookup', input: {} } }), delta('input_json_delta', 'partial_json', '{"query":"x"}'), ev({ type: 'message_delta', delta: { stop_reason: 'tool_use' }, usage: { output_tokens: 9 } }), stop) })
    add('message-events-ignored', { reply: reply(start, ev({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }), textDelta('A'), ev({ type: 'content_block_stop', index: 0 }), ev({ type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 1 } }), stop) })
    add('text-delta-number', { reply: reply(delta('text_delta', 'text', 5), textDelta('!')) })
    add('text-delta-null', { reply: reply(delta('text_delta', 'text', null), textDelta('!')) })
    add('text-delta-missing', { reply: reply(ev({ type: 'content_block_delta', delta: { type: 'text_delta' } }), textDelta('!')) })
    add('text-delta-bool', { reply: reply(delta('text_delta', 'text', true), textDelta('!')) })
    add('text-delta-object-after-text-declines', { reply: reply(textDelta('A'), delta('text_delta', 'text', { a: 1 }), textDelta('!')), declines: 'stream-value-not-text', terminal: true })
    add('text-delta-object-first-declines', { reply: reply(delta('text_delta', 'text', { a: 1 }), textDelta('!')), declines: 'stream-value-not-text' })

    // Thinking.
    add('thinking-then-text', { reply: reply(start, thinkDelta('Hmm. '), thinkDelta('Yes.'), textDelta('Answer.'), stop) })
    add('thinking-never-closed', { reply: reply(start, thinkDelta('Hmm.'), stop) })
    add('thinking-text-thinking', { reply: reply(thinkDelta('A'), textDelta('B'), thinkDelta('C'), stop) })
    add('thinking-legacy-type', { reply: reply(ev({ type: 'content_block_delta', delta: { type: 'thinking', thinking: 'Old' } }), textDelta('B'), stop) })
    add('thinking-delta-missing-field', { reply: reply(ev({ type: 'content_block_delta', delta: { type: 'thinking_delta' } }), textDelta('B')) })
    add('redacted-thinking', { reply: reply(ev({ type: 'content_block_delta', delta: { type: 'redacted_thinking' } }), textDelta('B'), stop) })
    add('thinking-open-tag-lost-on-replay', { reply: reply(thinkDelta('A') + thinkDelta('B').trimEnd() + '\n', '\n', textDelta('C')) })
    add('thinking-single-chunk-without-blank-line', { reply: reply(thinkDelta('A').trimEnd() + '\n') })
    add('thinking-only-then-more-chunks', { reply: reply(thinkDelta('A'), thinkDelta('B'), thinkDelta('C')) })

    // Errors inside the stream.
    const err = error => ev({ type: 'error', error })
    add('error-event', { reply: reply(textDelta('A'), err({ type: 'api_error', message: 'Boom.' }), textDelta('B')) })
    add('error-event-without-message', { reply: reply(err({ type: 'api_error' })) })
    add('error-event-without-error', { reply: reply(ev({ type: 'error' })) })
    add('error-event-empty-message', { reply: reply(err({ message: '' })) })
    add('error-event-zero-message', { reply: reply(err({ message: 0 })) })
    add('error-event-null-message', { reply: reply(err({ message: null })) })
    add('error-event-number-message-dropped', { reply: reply(err({ message: 5 }), textDelta('A')) })
    add('error-event-object-message-dropped', { reply: reply(err({ message: { a: 1 } }), textDelta('A')) })
    add('error-event-after-thinking', { reply: reply(thinkDelta('A'), err({ message: 'Boom.' })) })
    add('error-event-overloaded-without-retry-setting-is-dropped', { db: { antiServerOverloads: false }, reply: reply(err({ type: 'overloaded_error', message: 'Overloaded' })) })
    add('error-event-overloaded-with-retry-setting-is-dropped', { db: { antiServerOverloads: true }, reply: reply(err({ type: 'overloaded_error', message: 'Overloaded' })) })
    add('error-event-overloaded-after-text-is-dropped', { db: { antiServerOverloads: true }, reply: reply(textDelta('A'), err({ message: 'Server overload!' }), textDelta('B')) })
    add('error-event-overloaded-lowercase-is-dropped', { reply: reply(err({ message: 'overload' })) })
    add('error-event-overloaded-inside-a-sentence-is-dropped', { reply: reply(err({ message: 'The OVERLOADED server' })) })
    add('error-event-not-overload-with-retry-setting', { db: { antiServerOverloads: true }, reply: reply(err({ message: 'Other.' })) })

    // Bytes.
    const accents = Buffer.from('é😀', 'utf8').toString('hex')
    add('multibyte-split-between-chunks', { reply: { chunks: [{ utf8: 'data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"' }, { hex: accents.slice(0, 6) }, { hex: accents.slice(6) }, { utf8: '"}}\n\n' }] } })
    add('multibyte-whole', { reply: reply(textDelta('Café 😀  ')) })
    add('byte-order-mark-at-start', { reply: { chunks: [{ hex: bom + hex(textDelta('A')) }, { utf8: textDelta('B') }] } })
    add('byte-order-mark-in-later-chunk', { reply: { chunks: [{ utf8: textDelta('A') }, { hex: bom + hex(textDelta('B')) }] } })
    add('invalid-utf8-byte', { reply: { chunks: [{ hex: hex('data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"a') + 'ff' + hex('b"}}\n\n') }] } })
    add('empty-body', { reply: { chunks: [] } })
    add('empty-chunks', { reply: { chunks: [{ utf8: '' }, { utf8: textDelta('A') }, { utf8: '' }] } })
    add('only-blank-lines', { reply: reply('\n\n\n') })
    add('many-events', { reply: reply(Array.from({ length: 30 }, (_, index) => textDelta(String(index % 10))).join('') + stop) })
    add('long-text-split-in-small-chunks', { reply: { chunks: textDelta('x'.repeat(200)).match(/[^]{1,17}/g).map(utf8 => ({ utf8 })) } })

    // Responses that are not event streams.
    add('http-500-json-body', { reply: { status: 500, json: { type: 'error', error: { type: 'api_error', message: 'Internal.' } } } })
    add('http-401-plain-body', { reply: { status: 401, contentType: 'text/plain', text: 'unauthorized' } })
    add('http-404-empty-body', { reply: { status: 404, text: '' } })
    add('http-429-body-with-bom', { reply: { status: 429, chunks: [{ hex: bom + hex('slow down') }], contentType: 'text/plain' } })
    add('http-400-invalid-utf8', { reply: { status: 400, chunks: [{ hex: hex('bad ') + 'ff' }], contentType: 'text/plain' } })
    add('http-500-huge-body-is-bounded', { reply: { status: 500, contentType: 'text/plain', chunks: [{ utf8: 'h'.repeat(3000) }] } })
    add('http-200-json-body-is-read-as-events', { reply: { status: 200, json: { content: [{ type: 'text', text: 'x' }] } } })
    add('http-200-plain-body-is-read-as-events', { reply: { status: 200, contentType: 'text/plain', text: textBody('Plain') } })

    // Settings that reach the stream request.
    add('stream-with-reverse-proxy', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages' }, reply: reply(textDelta('Proxied.'), stop) })
    add('stream-with-custom-model', { model: 'xcustom:::one', customModels: [customModel()], reply: reply(textDelta('Custom.'), stop) })

    writeFileSync(`${out}/streams.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

// Bedrock: the request is signed with SigV4 at a fixed clock; the reply is read by the Bedrock branch of requestClaude.
function writeBedrock() {
    const cases = []
    const clockAt = '2026-01-02T03:04:05Z'
    const KEY = 'AKIDSYNTHETIC:synthetic-secret:us-east-1'
    const model = (internalId, key = KEY, extra = {}) => customModel({
        id: 'xcustom:::bedrock', internalId, format: 17, key, flags: [F.hasFirstSystemPrompt, F.claudeThinking, F.hasImageInput], ...extra,
    })
    const add = (id, entry = {}) => cases.push({
        id, model: 'xcustom:::bedrock', customModels: [model('anthropic.claude-3-5-sonnet-20240620-v1:0')], messages: chat, clock: clockAt, ...entry,
    })
    const withModel = (internalId, more = {}, key = KEY, modelExtra = {}) => ({ customModels: [model(internalId, key, modelExtra)], ...more })
    const extra = (pairs, more = {}) => ({
        model: 'anthropic.claude-opus-4-6-v1', customModels: [], db: { claudeAPIKey: KEY, applyAdditionalParamsToAll: true, additionalParams: pairs, ...more },
    })
    const ok = content => ({ json: { id: 'msg_1', type: 'message', role: 'assistant', content } })
    const text = value => ({ type: 'text', text: value })

    // Request bytes.
    add('basic')
    add('stream-setting-is-ignored', { stream: true })
    add('model-with-date-before-global', withModel('anthropic.claude-sonnet-4-20250514-v1:0'))
    add('model-with-date-at-global', withModel('anthropic.claude-sonnet-4-5-20250929-v1:0'))
    add('model-with-date-after-global', withModel('anthropic.claude-opus-4-1-20251101-v1:0'))
    add('model-with-date-zero-falls-to-version', withModel('anthropic.claude-opus-4-5-00000000-v1:0'))
    add('model-version-4-5-is-global', withModel('anthropic.claude-opus-4-5-v1:0'))
    add('model-version-4-1-is-regional', withModel('anthropic.claude-opus-4-1-v1:0'))
    add('model-version-5-0-is-global', withModel('anthropic.claude-5-0-v1:0'))
    add('model-version-3-7-is-regional', withModel('anthropic.claude-3-7-sonnet-v1:0'))
    add('model-version-haiku-4-5', withModel('anthropic.claude-haiku-4-5-v1:0'))
    add('model-without-version-is-regional', withModel('anthropic.titan-v1'))
    add('model-eight-digits-inside-longer-run', withModel('anthropic.m-123456789012-v1:0'))
    add('model-empty-id', withModel(''))
    add('model-with-colon-and-dots', withModel('anthropic.claude-3-haiku-20240307-v1:0'))
    add('model-slash-declines', { ...withModel('anthropic/claude'), declines: 'bedrock-model-form' })
    add('model-space-declines', { ...withModel('anthropic claude'), declines: 'bedrock-model-form' })
    add('model-percent-declines', { ...withModel('anthropic%2Fclaude'), declines: 'bedrock-model-form' })
    add('model-non-ascii-declines', { ...withModel('anthropic.claude-é'), declines: 'bedrock-model-form' })
    add('region-other', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC:synthetic-secret:eu-west-3'))
    add('region-uppercase', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC:synthetic-secret:US-EAST-1'))
    add('region-with-dot-declines', { ...withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC:synthetic-secret:us.east'), declines: 'bedrock-region-form' })
    add('region-with-slash-declines', { ...withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC:synthetic-secret:us/east'), declines: 'bedrock-region-form' })
    add('key-with-extra-part-is-cut', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC:synthetic-secret:us-east-1:extra'))
    add('key-with-two-parts-throws', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC:synthetic-secret'))
    add('key-with-one-part-throws', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC'))
    add('key-with-empty-secret-throws', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC::us-east-1'))
    add('key-with-empty-id-throws', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, ':synthetic-secret:us-east-1'))
    add('key-with-empty-region-throws', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKIDSYNTHETIC:synthetic-secret:'))
    add('key-empty-falls-back-to-claude-key', { ...withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, ''), db: { claudeAPIKey: 'AKIDFALLBACK:fallback-secret:ap-south-1' } })
    add('key-empty-and-no-claude-key-throws', { ...withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, ''), db: { claudeAPIKey: '' } })
    add('key-with-symbols', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKID+/=:sec/ret+with=symbols/:us-east-1'))
    add('key-non-ascii-secret', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, 'AKID:sécret😀:us-east-1'))
    add('clock-year-end', { clock: '2026-12-31T23:59:59Z' })
    add('clock-leap-day', { clock: '2028-02-29T00:00:00Z' })
    add('clock-milliseconds-are-dropped', { clock: '2026-01-02T03:04:05.987Z' })
    add('system-and-multi-turn', { messages: talk })
    add('no-system', { messages: [user('Hello')] })
    add('thinking-budget-sets-temperature', {})
    add('thinking-off-keeps-sampling', { db: { thinkingType: 'off' } })
    add('thinking-adaptive', { db: { thinkingType: 'adaptive' } })
    add('thinking-adaptive-effort-null', { db: { thinkingType: 'adaptive', adaptiveThinkingEffort: null } })
    add('tools', { tools: [lookup, clock] })
    add('image', { messages: [sys('Be brief.'), user('Look.', { multimodals: [image()] })] })
    add('cache-and-one-hour', { db: { claude1HourCaching: true }, messages: [sys('Be brief.'), user('Hello', { cachePoint: true })] })
    add('max-tokens-over-8192-has-no-beta', { maxTokens: 16000 })
    add('unicode-body', { messages: [sys('Be brief.'), user('Café 😀   end')] })
    add('lone-surrogate-body', { messages: [sys('Be brief.'), user('a\ud800b')] })
    add('reverse-proxy-takes-the-plain-path', {
        model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages', customAPIFormat: 17 }, customModels: [],
    })
    add('claude-format-model-is-not-bedrock', { model: A, customModels: [] })

    // Additional parameters and header canonicalization.
    add('builtin-model-without-params', { model: 'anthropic.claude-opus-4-6-v1', customModels: [], db: { claudeAPIKey: KEY } })
    add('builtin-model-key-from-claude-key-only', { model: 'anthropic.claude-opus-4-6-v1', customModels: [], db: { claudeAPIKey: 'AKIDB:secretb:us-west-2' } })
    add('builtin-date-model', { model: 'anthropic.claude-opus-4-5-20251101-v1:0', customModels: [], db: { claudeAPIKey: KEY } })
    add('custom-model-params-lines', withModel('anthropic.claude-3-5-sonnet-20240620-v1:0', {}, KEY, { params: 'top_k=9\nheader::x-custom=yes\nanthropic_beta=[1]' }))
    add('param-body-key', extra([['top_k', '7']]))
    add('param-header', extra([['header::x-synthetic', 'value']]))
    add('param-header-uppercase-name', extra([['header::X-Synthetic', 'value']]))
    add('param-header-spaces-collapse', extra([['header::x-synthetic', '  synthetic   value ']]))
    add('param-header-no-break-space', extra([['header::x-synthetic', ' synthetic value ']]))
    add('param-header-tabs', extra([['header::x-synthetic', '\tsynthetic\t\tvalue\t']]))
    add('param-header-newline', extra([['header::x-synthetic', 'one\ntwo\r\nthree']]))
    add('param-header-empty-value', extra([['header::x-synthetic', '']]))
    add('param-header-non-ascii-value', extra([['header::x-synthetic', 'café 😀']]))
    add('param-header-lone-surrogate-declines', { ...extra([['header::x-synthetic', 'a\ud800b']]), declines: 'isolated-surrogate-in-header-or-url' })
    add('param-header-sorted', extra([['header::x-zeta', '1'], ['header::x-alpha', '2'], ['header::Accept-Language', 'en']]))
    add('param-header-sort-by-code-unit', extra([['header::x-b', '1'], ['header::X-A', '2'], ['header::x-a_', '3'], ['header::x-a-', '4']]))
    add('param-header-host-duplicate', extra([['header::host', 'other.example.test']]))
    add('param-header-content-type-duplicate', extra([['header::content-type', 'text/plain']]))
    add('param-header-accept-override', extra([['header::accept', 'text/event-stream']]))
    add('param-header-authorization-is-replaced', extra([['header::Authorization', 'Bearer synthetic']]))
    add('param-header-date-is-dropped', extra([['header::Date', 'Thu, 01 Jan 2026 00:00:00 GMT']]))
    add('param-header-amz-date-is-dropped', extra([['header::X-Amz-Date', '20200101T000000Z']]))
    add('param-header-payload-hash-is-used', extra([['header::x-amz-content-sha256', 'UNSIGNED-PAYLOAD']]))
    add('param-header-payload-hash-uppercase', extra([['header::X-Amz-Content-Sha256', 'deadbeef']]))
    add('param-header-user-agent-unsigned', extra([['header::User-Agent', 'synthetic/1.0']]))
    add('param-header-proxy-prefix-unsigned', extra([['header::Proxy-Thing', 'p']]))
    add('param-header-sec-prefix-unsigned', extra([['header::Sec-Thing', 's']]))
    add('param-header-always-unsigned-list', extra([
        ['header::Cache-Control', 'no-cache'], ['header::Connection', 'close'], ['header::Expect', '100'], ['header::From', 'a@b'],
        ['header::Keep-Alive', '1'], ['header::Max-Forwards', '2'], ['header::Pragma', 'p'], ['header::Referer', 'r'], ['header::TE', 't'],
        ['header::Trailer', 'x'], ['header::Transfer-Encoding', 'chunked'], ['header::Upgrade', 'u'], ['header::X-Amzn-Trace-Id', 'root'],
    ]))
    add('param-header-amz-security-token', extra([['header::x-amz-security-token', 'token']]))
    add('param-header-amz-other', extra([['header::x-amz-target', 'target']]))
    add('param-header-anthropic-beta', extra([['header::anthropic-beta', 'custom-beta']]))
    add('param-header-non-ascii-name-declines', { ...extra([['header::x-café', 'v']]), declines: 'header-name-non-ascii' })
    add('param-body-and-header', extra([['top_k', '3'], ['header::x-synthetic', 'v'], ['metadata', '{"user_id":"u"}']]))
    add('param-anthropic-version-override', extra([['anthropic_version', 'other']]))
    add('param-stream-key-is-kept', extra([['stream', 'true']]))
    add('param-model-key-is-kept', extra([['model', 'other']]))
    add('param-temperature-after-thinking', extra([['temperature', '0.2']]))
    add('param-top-p-after-thinking', extra([['top_p', '0.2']]))

    // Replies.
    add('reply-text', { reply: ok([text('Hello there.')]) })
    add('reply-text-blocks', { reply: ok([text('One. '), text('Two.')]) })
    add('reply-thinking-then-text', { reply: ok([{ type: 'thinking', thinking: 'Hmm.', signature: 's' }, text('Done.')]) })
    add('reply-thinking-only', { reply: ok([{ type: 'thinking', thinking: 'Hmm.' }]) })
    add('reply-redacted-thinking', { reply: ok([{ type: 'redacted_thinking', data: 'x' }, text('Done.')]) })
    add('reply-thinking-field-missing', { reply: ok([{ type: 'thinking' }, text('Done.')]) })
    add('reply-text-field-missing', { reply: ok([{ type: 'text' }]) })
    add('reply-text-field-null', { reply: ok([{ type: 'text', text: null }]) })
    add('reply-text-field-number', { reply: ok([{ type: 'text', text: 5 }]) })
    add('reply-unknown-block-ignored', { reply: ok([{ type: 'image' }, 'plain', 7, text('Kept.')]) })
    add('reply-tool-use-block-is-ignored', { tools: [lookup], reply: ok([text('Let me look.'), { type: 'tool_use', id: 'toolu_1', name: 'lookup', input: { query: 'x' }, }]) })
    add('reply-only-tool-use-is-empty-success', { tools: [lookup], reply: ok([{ type: 'tool_use', id: 'toolu_1', name: 'lookup', input: {} }]) })
    add('reply-empty-content-fails', { reply: ok([]) })
    add('reply-content-missing-fails', { reply: { json: { id: 'msg_1' } } })
    add('reply-content-null-fails', { reply: { json: { content: null } } })
    add('reply-content-empty-string-fails', { reply: { json: { content: '' } } })
    add('reply-content-string-reads-no-blocks', { reply: { json: { content: 'abc' } } })
    add('reply-content-string-with-extract-json', { extractJson: 'answer', db: { jsonSchemaEnabled: true, jsonSchema: 'answer' }, reply: { json: { content: 'abc' } } })
    add('reply-content-object-throws', { reply: { json: { content: { type: 'text', text: 'x' } } } })
    add('reply-content-number-throws', { reply: { json: { content: 5 } } })
    add('reply-content-true-throws', { reply: { json: { content: true } } })
    add('reply-content-zero-fails', { reply: { json: { content: 0 } } })
    add('reply-null-entry-throws', { reply: ok([text('A'), null]) })
    add('model-internal-id-missing-throws', { customModels: [model(undefined)] })
    add('reply-error-field', { reply: { json: { type: 'error', error: { type: 'invalid_request_error', message: 'Bad.' } } } })
    add('reply-error-field-string', { reply: { json: { error: 'plain' } } })
    add('reply-error-beats-content', { reply: { json: { error: { message: 'No.' }, content: [text('Yes.')] } } })
    add('reply-http-403', { reply: { status: 403, json: { message: 'The security token included in the request is invalid.' } } })
    add('reply-http-500-overloaded-text', { reply: { status: 500, json: { message: 'Server overloaded' } } })
    add('reply-http-400-plain-text', { reply: { status: 400, contentType: 'text/plain', text: 'bad request' } })
    add('reply-http-200-plain-text', { reply: { status: 200, text: 'not json' } })
    add('reply-http-200-empty-body', { reply: { status: 200, text: '' } })
    add('reply-transport-error', { reply: { throws: 'fetch failed' } })
    add('reply-data-null-throws', { reply: { text: 'null' } })
    add('reply-data-string', { reply: { json: 'just a string' } })
    add('reply-huge-error-is-bounded', { reply: { status: 500, json: { error: { message: 'e'.repeat(3000) } } } })
    add('reply-extract-json', { extractJson: 'answer', db: { jsonSchemaEnabled: true, jsonSchema: 'answer' }, reply: ok([text('{"answer":"yes"}')]) })
    add('reply-extract-json-after-thoughts', { extractJson: 'answer', db: { jsonSchemaEnabled: true, jsonSchema: 'answer' }, reply: ok([{ type: 'thinking', thinking: 'T' }, text('{"answer":"yes"}')]) })
    add('reply-extract-json-needs-the-setting', { extractJson: 'answer', db: { jsonSchemaEnabled: false }, reply: ok([text('{"answer":"yes"}')]) })
    add('reply-remember-tool-usage-has-no-effect', { rememberToolUsage: true, tools: [lookup], reply: ok([text('Hi.')]) })

    writeFileSync(`${out}/bedrock.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

// claudeBatching: the batch is created, polled every three seconds and read back as JSON lines.
function writeBatching() {
    const cases = []
    const clockAt = '2026-01-02T03:04:05Z'
    const add = (id, entry = {}) => {
        const { db, ...rest } = entry
        cases.push({ id, model: A, messages: chat, clock: clockAt, db: { claudeBatching: true, ...db }, ...rest })
    }
    const created = { json: { id: 'msgbatch_synthetic', type: 'message_batch', processing_status: 'in_progress' } }
    const running = { json: { id: 'msgbatch_synthetic', processing_status: 'in_progress' } }
    const ended = { json: { id: 'msgbatch_synthetic', processing_status: 'ended' } }
    const line = result => ({ custom_id: '00000000-0000-4000-8000-000000000001', result })
    const message = content => ({ type: 'succeeded', message: { id: 'msg_1', type: 'message', role: 'assistant', content } })
    const text = value => ({ type: 'text', text: value })
    const jsonl = (...lines) => ({ text: lines.map(item => (typeof item === 'string' ? item : JSON.stringify(item))).join('\n') + '\n', contentType: 'application/x-jsonl' })
    const done = jsonl(line(message([text('Batch done.')])))
    const flow = (...middle) => [created, ...middle]
    const happy = [created, ended, done]
    const errored = error => jsonl(line({ type: 'errored', error }))

    // Creating the batch.
    add('basic', { replies: happy })
    add('polls-until-ended', { replies: flow(running, running, running, ended, done) })
    add('stream-setting-is-removed-from-the-batch', { stream: true, replies: happy })
    add('thinking-and-text', { replies: flow(ended, jsonl(line(message([{ type: 'thinking', thinking: 'Hmm.', signature: 's' }, text('Done.')])))) })
    add('redacted-thinking', { replies: flow(ended, jsonl(line(message([{ type: 'redacted_thinking', data: 'x' }, text('Done.')])))) })
    add('thinking-only-closes-at-the-end', { replies: flow(ended, jsonl(line(message([{ type: 'thinking', thinking: 'Hmm.' }])))) })
    add('text-blocks-join', { replies: flow(ended, jsonl(line(message([text('One. '), text('Two.')])))) })
    add('tool-use-blocks-are-ignored', { tools: [lookup], replies: flow(ended, jsonl(line(message([text('Look.'), { type: 'tool_use', id: 'toolu_1', name: 'lookup', input: {} }])))) })
    add('tools-and-system-in-the-batch', { tools: [lookup, clock], messages: talk, replies: happy })
    add('image-in-the-batch', { messages: [sys('Be brief.'), user('Look.', { multimodals: [image()] })], replies: happy })
    add('max-tokens-and-caching-headers', { maxTokens: 16000, db: { claude1HourCaching: true }, replies: happy })
    add('additional-params', { db: { applyAdditionalParamsToAll: true, additionalParams: [['top_k', '7'], ['header::x-route', 'r']] }, replies: happy })
    add('reverse-proxy-url', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages' }, replies: happy })
    add('reverse-proxy-trailing-slash', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/messages/' }, replies: happy })
    add('custom-model-url', { model: 'xcustom:::one', customModels: [customModel()], replies: happy })
    add('plain-fetch-off-has-no-browser-header', { db: { usePlainFetch: false }, replies: happy, transport: 'risu-proxy' })
    add('unicode-result', { replies: flow(ended, jsonl(line(message([text('Café 😀   end')])))) })

    // The reply to the create request.
    add('create-status-400-json', { replies: [{ status: 400, json: { type: 'error', error: { type: 'invalid_request_error', message: 'Bad.' } } }] })
    add('create-status-401-plain', { replies: [{ status: 401, contentType: 'text/plain', text: 'unauthorized' }] })
    add('create-status-500-empty', { replies: [{ status: 500, text: '' }] })
    add('create-status-201-is-a-failure', { replies: [{ status: 201, json: { id: 'msgbatch_synthetic' } }] })
    add('create-without-id', { replies: [{ json: { type: 'message_batch' } }] })
    add('create-id-empty-string', { replies: [{ json: { id: '' } }] })
    add('create-id-null', { replies: [{ json: { id: null } }] })
    add('create-id-zero', { replies: [{ json: { id: 0 } }] })
    add('create-body-array-has-no-id', { replies: [{ json: [] }] })
    add('create-body-string-has-no-id', { replies: [{ json: 'text' }] })
    add('create-body-null-throws', { replies: [{ text: 'null' }] })
    add('create-body-not-json-throws', { replies: [{ status: 200, text: 'not json' }] })
    add('create-body-empty-throws', { replies: [{ status: 200, text: '' }] })
    add('create-transport-error-throws', { replies: [{ throws: 'fetch failed' }] })
    add('create-id-number-declines', { replies: [{ json: { id: 5 } }, ended, done], declines: 'batch-id-not-text', terminal: true })
    add('create-id-object-declines', { replies: [{ json: { id: { a: 1 } } }, ended, done], declines: 'batch-id-not-text', terminal: true })
    add('create-id-true-declines', { replies: [{ json: { id: true } }, ended, done], declines: 'batch-id-not-text', terminal: true })
    add('create-id-with-slash-declines', { replies: [{ json: { id: 'a/b' } }, ended, done], declines: 'batch-id-form', terminal: true })
    add('create-id-with-space-declines', { replies: [{ json: { id: 'a b' } }, ended, done], declines: 'batch-id-form', terminal: true })
    add('create-id-with-query-declines', { replies: [{ json: { id: 'a?b' } }, ended, done], declines: 'batch-id-form', terminal: true })

    // Polling the status.
    add('status-500-ends-the-stream', { replies: flow({ status: 500, contentType: 'text/plain', text: 'Internal.' }) })
    add('status-404-empty', { replies: flow({ status: 404, text: '' }) })
    add('status-after-polls-fails', { replies: flow(running, { status: 503, json: { error: { message: 'Down.' } } }) })
    add('status-huge-body', { replies: flow({ status: 500, contentType: 'text/plain', text: 'h'.repeat(3000) }) })
    add('status-transport-error-is-retried', { replies: flow({ throws: 'net down' }, ended, done) })
    add('status-not-json-is-retried', { replies: flow({ text: 'oops' }, ended, done) })
    add('status-null-is-retried', { replies: flow({ text: 'null' }, ended, done) })
    add('status-array-is-not-ended', { replies: flow({ json: [] }, ended, done) })
    add('status-string-is-not-ended', { replies: flow({ json: 'ended' }, ended, done) })
    add('status-ended-in-other-case-is-not-ended', { replies: flow({ json: { processing_status: 'ENDED' } }, ended, done) })
    add('status-field-missing-keeps-polling', { replies: flow({ json: {} }, ended, done) })
    add('status-byte-order-mark-is-stripped', { replies: flow({ chunks: [{ hex: 'efbbbf' }, { utf8: '{"processing_status":"ended"}' }], contentType: 'application/json' }, done) })
    add('status-canceling-keeps-polling', { replies: flow({ json: { processing_status: 'canceling' } }, ended, done) })

    // Reading the results.
    add('results-500-ends-the-stream', { replies: flow(ended, { status: 500, contentType: 'text/plain', text: 'Internal.' }) })
    add('results-404-json', { replies: flow(ended, { status: 404, json: { error: { message: 'Gone.' } } }) })
    add('results-transport-error-is-retried', { replies: flow(ended, { throws: 'net down' }, ended, done) })
    add('results-empty-is-retried', { replies: flow(ended, { text: '' }, ended, done) })
    add('results-only-blank-lines-is-retried', { replies: flow(ended, { text: '\n  \n \n' }, ended, done) })
    add('results-unknown-type-is-retried', { replies: flow(ended, jsonl(line({ type: 'mystery' })), ended, done) })
    add('results-invalid-lines-are-dropped', { replies: flow(ended, jsonl('not json', '{"broken":', line(message([text('Kept.')])))) })
    add('results-null-and-scalars-are-skipped', { replies: flow(ended, jsonl('null', '5', '"text"', '[]', 'true', line(message([text('Kept.')])))) })
    add('results-crlf-lines', { replies: flow(ended, { text: JSON.stringify(line(message([text('Kept.')]))) + '\r\n' }) })
    add('results-no-trailing-newline', { replies: flow(ended, { text: JSON.stringify(line(message([text('Kept.')]))) }) })
    add('results-no-break-space-line-is-dropped', { replies: flow(ended, { text: ' ' + JSON.stringify(line(message([text('Lost.')]))) + '\n' + JSON.stringify(line(message([text('Kept.')]))) }) })
    add('results-byte-order-mark-is-stripped', { replies: flow(ended, { chunks: [{ hex: 'efbbbf' }, { utf8: JSON.stringify(line(message([text('Kept.')]))) + '\n' }] }) })
    add('results-invalid-utf8-byte', { replies: flow(ended, { chunks: [{ utf8: '{"result":{"type":"succeeded","message":{"content":[{"type":"text","text":"a' }, { hex: 'ff' }, { utf8: 'b"}]}}}\n' }] }) })
    add('results-first-succeeded-wins', { replies: flow(ended, jsonl(line(message([text('First.')])), line(message([text('Second.')])))) })
    add('results-noise-before-succeeded', { replies: flow(ended, jsonl(line({ type: 'mystery' }), { other: 1 }, line(message([text('Kept.')])))) })
    add('results-succeeded-before-errored', { replies: flow(ended, jsonl(line(message([text('Won.')])), line({ type: 'errored', error: { error: { type: 'x', message: 'Lost.' } } }))) })
    add('results-errored-before-succeeded', { replies: flow(ended, jsonl(line({ type: 'errored', error: { error: { type: 'x', message: 'Won.' } } }), line(message([text('Lost.')])))) })
    add('results-canceled-ends-without-text', { replies: flow(ended, jsonl(line({ type: 'canceled' }))) })
    add('results-expired', { replies: flow(ended, jsonl(line({ type: 'expired' }))) })
    add('results-without-result-key', { replies: flow(ended, jsonl({ custom_id: 'x' }, line(message([text('Kept.')])))) })
    add('results-result-null', { replies: flow(ended, jsonl({ result: null }, line(message([text('Kept.')])))) })
    add('results-result-string-type', { replies: flow(ended, jsonl({ result: 'succeeded' }, line(message([text('Kept.')])))) })
    add('results-type-not-text', { replies: flow(ended, jsonl({ result: { type: ['succeeded'] } }, line(message([text('Kept.')])))) })

    // A succeeded line that cannot be read is retried like any other failure inside the loop.
    add('succeeded-message-missing-is-retried', { replies: flow(ended, jsonl(line({ type: 'succeeded' })), ended, done) })
    add('succeeded-message-null-is-retried', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: null })), ended, done) })
    add('succeeded-message-string-is-empty', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: 'text' }))) })
    add('succeeded-message-array-is-empty', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: [] }))) })
    add('succeeded-content-missing-is-empty', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { id: 'm' } }))) })
    add('succeeded-content-null-is-empty', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { content: null } }))) })
    add('succeeded-content-empty-array-is-empty', { replies: flow(ended, jsonl(line(message([])))) })
    add('succeeded-content-string-is-empty', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { content: 'abc' } }))) })
    add('succeeded-content-empty-string-is-empty', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { content: '' } }))) })
    add('succeeded-content-object-is-retried', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { content: { type: 'text', text: 'x' } } })), ended, done) })
    add('succeeded-content-number-is-retried', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { content: 5 } })), ended, done) })
    add('succeeded-content-zero-is-retried', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { content: 0 } })), ended, done) })
    add('succeeded-content-false-is-retried', { replies: flow(ended, jsonl(line({ type: 'succeeded', message: { content: false } })), ended, done) })
    add('succeeded-content-null-entry-is-retried', { replies: flow(ended, jsonl(line(message([text('A'), null]))), ended, done) })
    add('succeeded-content-scalar-entries-are-ignored', { replies: flow(ended, jsonl(line(message([5, 'x', text('Kept.')])))) })
    add('succeeded-text-field-missing', { replies: flow(ended, jsonl(line(message([{ type: 'text' }])))) })
    add('succeeded-text-field-null', { replies: flow(ended, jsonl(line(message([{ type: 'text', text: null }])))) })
    add('succeeded-text-field-number', { replies: flow(ended, jsonl(line(message([{ type: 'text', text: 5 }])))) })
    add('succeeded-text-field-object', { replies: flow(ended, jsonl(line(message([{ type: 'text', text: { a: 1 } }])))) })

    // Errors in the results.
    add('errored-with-nested-message', { replies: flow(ended, errored({ type: 'error', error: { type: 'invalid_request_error', message: 'Bad.' } })) })
    add('errored-nested-type-missing', { replies: flow(ended, errored({ error: { message: 'Bad.' } })) })
    add('errored-nested-message-number', { replies: flow(ended, errored({ error: { type: 'e', message: 5 } })) })
    add('errored-nested-message-true', { replies: flow(ended, errored({ error: { type: 'e', message: true } })) })
    add('errored-nested-type-number', { replies: flow(ended, errored({ error: { type: 7, message: 'Bad.' } })) })
    add('errored-nested-type-null', { replies: flow(ended, errored({ error: { type: null, message: 'Bad.' } })) })
    add('errored-nested-message-empty-uses-json', { replies: flow(ended, errored({ error: { type: 'e', message: '' } })) })
    add('errored-nested-message-null-uses-json', { replies: flow(ended, errored({ error: { type: 'e', message: null } })) })
    add('errored-nested-message-zero-uses-json', { replies: flow(ended, errored({ error: { type: 'e', message: 0 } })) })
    add('errored-without-nested-error-uses-json', { replies: flow(ended, errored({ type: 'api_error', message: 'Flat.' })) })
    add('errored-error-is-a-string', { replies: flow(ended, errored('plain')) })
    add('errored-error-is-a-number', { replies: flow(ended, errored(42)) })
    add('errored-error-is-null', { replies: flow(ended, errored(null)) })
    add('errored-error-is-an-array', { replies: flow(ended, errored([1, 'a'])) })
    add('errored-error-missing', { replies: flow(ended, jsonl(line({ type: 'errored' }))) })
    add('errored-nested-error-is-a-string', { replies: flow(ended, errored({ error: 'text' })) })
    add('errored-unicode-message', { replies: flow(ended, errored({ error: { type: 'e', message: 'Clé invalide 😀' } })) })
    add('errored-nested-message-object', { replies: flow(ended, errored({ error: { type: 'e', message: { a: 1 } } })) })
    add('errored-nested-type-object', { replies: flow(ended, errored({ error: { type: { a: 1 }, message: 'Bad.' } })) })
    add('errored-nested-message-array', { replies: flow(ended, errored({ error: { type: 'e', message: ['a'] } })) })

    // Cancelling.
    add('abort-cancels-the-batch', { abortAtSleep: 1, replies: flow({ json: {} }, ended, jsonl(line({ type: 'canceled' }))) })
    add('abort-after-a-poll', { abortAtSleep: 2, replies: flow(running, { json: {} }, ended, jsonl(line({ type: 'canceled' }))) })
    add('abort-cancel-transport-error-is-ignored', { abortAtSleep: 1, replies: flow({ throws: 'net down' }, ended, jsonl(line({ type: 'canceled' }))) })
    add('abort-cancel-status-500-is-ignored', { abortAtSleep: 1, replies: flow({ status: 500, text: 'no' }, ended, jsonl(line({ type: 'canceled' }))) })
    add('abort-is-cancelled-once', { abortAtSleep: 1, replies: flow({ json: {} }, running, running, ended, jsonl(line({ type: 'canceled' }))) })
    add('abort-but-the-batch-succeeds', { abortAtSleep: 1, replies: flow({ json: {} }, ended, done) })
    add('abort-then-status-fails', { abortAtSleep: 1, replies: flow({ json: {} }, { status: 500, text: 'down' }) })
    add('abort-then-timeout', { abortAtSleep: 1, replies: flow({ json: {}, advanceMs: 87_000_001 }) })

    // The 24 hour limit.
    add('timeout-after-a-long-poll', { replies: flow({ json: { processing_status: 'in_progress' }, advanceMs: 87_000_000 }) })
    add('timeout-at-the-limit-keeps-polling', { replies: flow({ json: { processing_status: 'in_progress' }, advanceMs: 86_994_000 }, ended, done) })
    add('timeout-one-millisecond-over', { replies: flow({ json: { processing_status: 'in_progress' }, advanceMs: 86_994_001 }) })
    add('timeout-while-results-keep-failing', { replies: flow(ended, { throws: 'down', advanceMs: 87_000_000 }) })
    add('retried-success-does-not-time-out', { replies: flow(ended, { text: '', advanceMs: 60_000_000 }, ended, done) })

    writeFileSync(`${out}/batching.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

writeRequests()
writeResponses()
writeStreams()
writeBedrock()
writeBatching()
