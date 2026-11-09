// Writes the case files the Mistral oracle (providers-mistral.oracle.test.ts) reads: requests and responses under
// tests/fixtures/rust-core/providers/mistral. The cases are deterministic and use synthetic keys only.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultOut = resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/mistral')
const out = process.argv[2] ?? defaultOut
mkdirSync(out, { recursive: true })

// LLMFlags numbers (src/ts/model/types.ts); custom flags replace the flags of the selected model.
const F = { hasImageInput: 0, hasPrefill: 4, hasFullSystemPrompt: 6, hasFirstSystemPrompt: 7, hasStreaming: 8, DeveloperRole: 14 }
const flags = (...names) => ({ enableCustomFlags: true, customFlags: names.map(name => F[name]) })

const sys = text => ({ role: 'system', content: text })
const user = (text, extra = {}) => ({ role: 'user', content: text, ...extra })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const fn = text => ({ role: 'function', content: text, name: 'lookup' })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const baseDb = {
    usePlainFetch: true,
    cohereAPIKey: 'sk-synthetic-cohere',
    mistralKey: 'sk-synthetic-mistral',
    proxyKey: 'sk-synthetic-proxy',
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
    reverseProxyOobaMode: false,
    seperateParametersEnabled: false,
    seperateParametersByModel: false,
    extractJson: '',
    systemContentReplacement: '',
    systemRoleReplacement: '',
    forceReplaceUrl: '',
    customProxyRequestModel: 'my-proxy-model',
    customAPIFormat: 4,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

const sep = (t, k, p, f) => ({ temperature: t, top_k: k, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: p, frequency_penalty: f, presence_penalty: f, thinking_tokens: 0, reasoning_effort: 1, verbosity: 1 })
const separate = {
    seperateParametersEnabled: true,
    seperateParameters: {
        model: sep(50, 10, 0.5, 10), submodel: sep(60, 11, 0.6, 11), memory: sep(61, 11, 0.6, 11), emotion: sep(62, 11, 0.6, 11),
        translate: sep(63, 11, 0.6, 11), otherAx: sep(70, 12, 0.7, 12),
        overrides: { 'mistral-large-latest': sep(30, 5, 0.3, -1000) },
    },
}

const customModel = (extra = {}) => ({ id: 'xcustom:::one', name: 'One', internalId: 'my-model', url: 'https://llm.example.test/v1/chat', key: 'sk-synthetic-custom', format: 4, flags: [F.hasFullSystemPrompt], tokenizer: 0, params: '', ...extra })

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}
const ok = json => ({ replies: [{ json }] })
const status = (code, body) => ({ replies: [{ status: code, json: body }] })

const M = 'mistral-large-latest'

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: M, ...entry })
    add('basic', { messages: chat })
    add('multi-turn', { messages: talk })
    add('no-system', { messages: [user('Hello')] })
    add('user-first-then-assistant', { messages: [user('Hello'), bot('Hey.'), user('Again')] })
    add('empty-messages', { messages: [] })
    add('system-only', { messages: [sys('Only system.')] })
    add('consecutive-users-merge', { messages: [sys('Be brief.'), user('One'), user('Two'), user('Three')] })
    add('consecutive-assistants-merge', { messages: [sys('Be brief.'), user('Hi'), bot('One'), bot('Two'), user('Again')] })
    add('system-after-user-appends', { messages: [sys('Be brief.'), user('Hi'), sys('Aside.'), bot('Hey.'), user('Again')] })
    add('system-after-assistant-pushes-user', { messages: [sys('Be brief.'), user('Hi'), bot('Hey.'), sys('Aside.'), user('Again')] })
    add('system-after-system-merges', { messages: [sys('One.'), sys('Two.'), user('Hi')] })
    add('function-role', { messages: [sys('Be brief.'), user('Hi'), bot('Checking.'), fn('result'), user('Again')] })
    add('function-after-user-merges', { messages: [sys('Be brief.'), user('Hi'), fn('result'), bot('Ok.')] })
    add('assistant-first', { messages: [bot('Hey.'), user('Hi')] })
    add('function-first', { messages: [fn('result'), user('Hi')] })
    add('empty-contents-dropped', { messages: [sys('Be brief.'), user('Hi'), bot(''), user('Again')] })
    add('empty-contents-kept-without-new-handle', { db: { newOAIHandle: false }, messages: [sys('Be brief.'), user('Hi'), bot(''), user('Again')] })
    add('new-chat-memo-clears-content', { messages: [sys('Be brief.'), user('Hi', { memo: 'NewChat' }), bot('Hey.'), user('Again')] })
    add('new-chat-memo-kept-without-new-handle', { db: { newOAIHandle: false }, messages: [sys('Be brief.'), user('Hi', { memo: 'NewChat' }), bot('Hey.'), user('Again')] })
    add('other-memo-kept', { messages: [sys('Be brief.'), user('Hi', { memo: 'Other' }), user('Again')] })
    add('message-fields-are-ignored', { messages: [sys('Be brief.'), { role: 'user', content: 'Hi', name: 'Alice', memo: 'm', attr: ['a'], removable: true, cachePoint: true }, bot('Hey', { thoughts: ['t'] })] })
    add('unicode-and-escapes', { messages: [sys('日本語 \u{1F600}'), user('"quoted" \\ back\nline  ')] })
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' }, messages: chat })
    add('image-on-assistant-ignored', { messages: [user('Hi'), bot('Hey', { multimodals: [{ type: 'image', base64: 'data:image/png;base64,AAAA' }] }), user('Again')] })
    add('image-on-system-ignored', { messages: [sys('Look', { multimodals: [{ type: 'image', base64: 'data:image/png;base64,AAAA' }] }), user('Hello')] })
    add('image-on-user-declines', { messages: [sys('Be brief.'), user('Look', { multimodals: [{ type: 'image', base64: 'data:image/png;base64,AAAA' }] })], declines: 'multimodal-user-message' })
    add('tool-call-marker-declines', { messages: [sys('Be brief.'), user('Hi'), bot('A<tool_call>k1</tool_call>B'), user('Again')], decodings: { k1: { call: { id: 'call_1', name: 'lookup', arg: '{}' }, response: [{ type: 'text', text: 'ok' }] } }, declines: 'tool-call-marker' })
    add('tool-call-marker-in-user-declines', { messages: [sys('Be brief.'), user('Hi <tool_call>k1</tool_call>')], declines: 'tool-call-marker' })
    add('ooba-mode-declines', { model: 'reverse_proxy', db: { reverseProxyOobaMode: true, forceReplaceUrl: 'https://proxy.example.test/v1' }, messages: chat, declines: 'reverse-proxy-ooba-mode' })
    add('developer-role-flag', { model: 'xcustom:::one', customModels: [customModel({ flags: [F.hasFullSystemPrompt, F.DeveloperRole] })], messages: chat })
    for (const model of ['mistral-small-latest', 'mistral-medium-latest', 'mistral-large-2411', 'open-mistral-nemo']) add(`model-${model}`, { model, messages: chat })
    add('max-tokens', { maxTokens: 777, messages: chat })
    add('stream-flag-is-ignored', { stream: true, messages: chat })
    add('temperature-disabled', { db: { temperature: -1000 }, messages: chat })
    add('penalties-disabled', { db: { frequencyPenalty: -1000, PresensePenalty: -1000 }, messages: chat })
    add('top-p-zero', { db: { top_p: 0 }, messages: chat })
    add('top-k-and-other-parameters-not-sent', { db: { top_k: 5, min_p: 0.5, top_a: 0.5, repetition_penalty: 2, thinkingTokens: 5 }, messages: chat })
    add('separate-parameters-model', { db: separate, messages: chat })
    add('separate-parameters-submodel', { db: separate, mode: 'submodel', messages: chat })
    add('separate-parameters-memory', { db: separate, mode: 'memory', messages: chat })
    add('separate-parameters-by-model-missing-throws', { db: { ...separate, seperateParametersByModel: true, seperateParameters: { ...separate.seperateParameters, overrides: {} } }, messages: chat })
    add('separate-parameters-by-model', { db: { ...separate, seperateParametersByModel: true, seperateParameters: { ...separate.seperateParameters, overrides: { [M]: sep(33, 6, 0.33, 5) } } }, messages: chat })
    add('additional-params-are-not-applied', { db: { additionalParams: [['seed', '7'], ['header::X-Test', 'on']] }, messages: chat })
    add('mistral-key-in-header', { db: { mistralKey: 'sk-other' }, messages: chat })
    add('reverse-proxy', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/chat/completions' }, messages: chat })
    add('reverse-proxy-model-is-the-alias', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/x', customProxyRequestModel: 'another' }, messages: chat })
    add('reverse-proxy-url-normalized', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://Proxy.Example.test:8443/a b?x=1#f' }, messages: chat })
    add('reverse-proxy-no-url-declines', { model: 'reverse_proxy', messages: chat, declines: 'url-form' })
    add('reverse-proxy-userinfo-declines', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://u:p@proxy.example.test/chat' }, messages: chat, declines: 'url-form' })
    add('custom-model', { model: 'xcustom:::one', customModels: [customModel()], messages: chat })
    add('custom-model-no-url', { model: 'xcustom:::one', customModels: [customModel({ url: undefined })], messages: chat })
    add('custom-model-no-key-uses-mistral-key', { model: 'xcustom:::one', customModels: [customModel({ key: undefined })], messages: chat })
    add('custom-model-empty-key-kept', { model: 'xcustom:::one', customModels: [customModel({ key: '' })], messages: chat })
    const raw = (id, messages, extra = {}) => add(id, { model: 'xcustom:::one', customModels: [customModel()], messages, ...extra })
    raw('raw-system-after-user-appends', [sys('S'), user('A'), sys('M'), bot('B'), user('C')])
    raw('raw-system-after-assistant-pushes-user', [sys('S'), user('A'), bot('B'), sys('M'), user('C')])
    raw('raw-system-after-system-merges', [sys('S'), sys('T'), user('A')])
    raw('raw-system-last', [sys('S'), user('A'), sys('M')])
    raw('raw-function-after-assistant', [sys('S'), user('A'), bot('B'), fn('R'), bot('C')])
    raw('raw-function-after-user-merges', [sys('S'), user('A'), fn('R')])
    raw('raw-function-first', [fn('R'), user('A')])
    raw('raw-assistant-first', [bot('B'), user('A')])
    raw('raw-user-first-system-second', [user('A'), sys('M'), user('B')])
    raw('raw-consecutive-all', [sys('S'), user('A'), user('B'), bot('C'), bot('D'), sys('E'), sys('F')])
    raw('raw-developer-role', [sys('S'), user('A'), sys('M')], { customModels: [customModel({ flags: [F.hasFullSystemPrompt, F.DeveloperRole] })] })
    raw('raw-developer-after-user', [user('A'), sys('M'), bot('B')], { customModels: [customModel({ flags: [F.hasFullSystemPrompt, F.DeveloperRole] })] })
    raw('raw-empty-first-user-dropped', [user(''), user('A')])
    raw('raw-empty-contents-kept', [sys(''), user(''), bot('')], { db: { newOAIHandle: false } })
    raw('raw-function-media-kept', [sys('S'), { role: 'function', content: '', name: 'lookup', multimodals: [{ type: 'image', base64: 'data:image/png;base64,AAAA' }] }, user('A')])
    add('other-format-declines', { model: 'gpt4o', messages: chat, declines: 'format' })
    write('requests', cases)
}

const reply = (...choices) => ok({ choices })
const msg = content => ({ message: { role: 'assistant', content } })
function writeResponses() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: M, messages: chat, ...entry })
    add('content', reply(msg('Hello there.')))
    add('content-unicode', reply(msg('日本語 \u{1F600} "q" \\ \n')))
    add('content-empty', reply(msg('')))
    add('content-null', reply(msg(null)))
    add('content-missing', reply({ message: { role: 'assistant' } }))
    add('second-choice-ignored', reply(msg('First.'), msg('Second.')))
    add('extra-fields', ok({ id: 'x', choices: [{ index: 0, message: { role: 'assistant', content: 'Hi' }, finish_reason: 'stop' }], usage: { total_tokens: 3 } }))
    add('choices-empty-fails', ok({ choices: [] }))
    add('choices-missing-fails', ok({ id: 'x' }))
    add('choices-null-fails', ok({ choices: null }))
    add('choices-string-fails', ok({ choices: 'abc' }))
    add('choices-object-with-index-key', ok({ choices: { 0: msg('Indexed.') } }))
    add('choice-without-message-fails', reply({ finish_reason: 'stop' }))
    add('choice-null-fails', reply(null))
    add('message-null-fails', reply({ message: null }))
    add('data-null-fails', ok(null))
    add('data-array-fails', ok([1, 2]))
    add('data-string-fails', ok('text'))
    add('data-number-fails', ok(5))
    add('data-numbers-echo', { replies: [{ text: '{"x":1.0,"y":1e21,"z":0.1,"big":12345678901234567890,"s":"\\u00e9","t":[1,2,{"a":null}]}' }] })
    add('content-bom', { replies: [{ text: '﻿' + JSON.stringify({ choices: [msg('Bom.')] }) }] })
    add('http-400-error-message', status(400, { error: { message: 'Bad key.', type: 'invalid_request_error' } }))
    add('http-400-error-without-message', status(400, { error: { type: 'invalid_request_error' } }))
    add('http-400-error-message-empty', status(400, { error: { message: '' } }))
    add('http-400-error-string', status(400, { error: 'plain' }))
    add('http-400-error-message-number', status(400, { error: { message: 5 } }))
    add('http-400-error-message-array', status(400, { error: { message: ['a', 'b'] } }))
    add('http-400-error-message-object', status(400, { error: { message: { a: 1 } } }))
    add('http-400-no-error', status(400, { message: 'invalid api token' }))
    add('http-500-json-null-throws', status(500, null))
    add('http-500-json-number', status(500, 5))
    add('http-401-text', { replies: [{ status: 401, text: 'unauthorized' }] })
    add('http-500-empty', { replies: [{ status: 500, text: '' }] })
    add('http-500-html', { replies: [{ status: 500, text: '<html>oops</html>' }] })
    add('http-200-not-json', { replies: [{ text: 'not json' }] })
    add('http-200-empty-body', { replies: [{ text: '' }] })
    add('http-201-ok', { replies: [{ status: 201, json: { choices: [msg('Created.')] } }] })
    add('http-302-with-choices', { replies: [{ status: 302, json: { choices: [msg('Redirected.')] } }] })
    add('transport-error', { replies: [{ throws: 'fetch failed' }] })
    add('http-error-long-message', status(400, { error: { message: 'x'.repeat(3000) } }))
    add('content-number-declines', { ...reply(msg(5)), declines: 'response-content-not-text' })
    add('content-array-declines', { ...reply(msg([{ type: 'text', text: 'a' }])), declines: 'response-content-not-text' })
    add('content-object-declines', { ...reply(msg({ a: 1 })), declines: 'response-content-not-text' })
    add('content-false-declines', { ...reply(msg(false)), declines: 'response-content-not-text' })
    add('error-message-with-to-string-declines', { ...status(400, { error: { message: { toString: 1 } } }), declines: 'thrown' })
    write('responses', cases)
}

writeRequests()
writeResponses()
