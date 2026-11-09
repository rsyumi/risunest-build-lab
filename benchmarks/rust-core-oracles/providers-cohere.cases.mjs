// Writes the case files the Cohere oracle (providers-cohere.oracle.test.ts) reads: requests and responses under
// tests/fixtures/rust-core/providers/cohere. The cases are deterministic and use synthetic keys only.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultOut = resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/cohere')
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
    customAPIFormat: 8,
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
        overrides: { 'cohere-command-r': sep(30, 5, 0.3, -1000) },
    },
}

const customModel = (extra = {}) => ({ id: 'xcustom:::one', name: 'One', internalId: 'my-model', url: 'https://llm.example.test/v1/chat', key: 'sk-synthetic-custom', format: 8, flags: [F.hasFullSystemPrompt], tokenizer: 0, params: '', ...extra })

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}
const ok = json => ({ replies: [{ json }] })
const status = (code, body) => ({ replies: [{ status: code, json: body }] })

const C = 'cohere-command-r'

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: C, ...entry })
    add('basic', { messages: chat })
    add('multi-turn', { messages: talk })
    add('no-system-user-first', { messages: [user('Hello'), bot('Hey.'), user('Again')] })
    add('only-user-message-throws', { messages: [user('Hello')] })
    add('empty-messages-throws', { messages: [] })
    add('system-only-throws', { messages: [sys('Only system.')] })
    add('assistant-last', { messages: [sys('Be brief.'), user('Hi'), bot('Hey there.')] })
    add('function-last', { messages: [sys('Be brief.'), user('Hi'), bot('Checking.'), fn('result')] })
    add('two-trailing-non-user', { messages: [sys('Be brief.'), user('Hi'), bot('One.'), bot('Two.')] })
    add('trailing-system', { messages: [sys('Be brief.'), user('Hi'), bot('One.'), sys('Reminder.')] })
    add('no-user-message-fails', { messages: [bot('Hey.')] })
    add('assistant-and-system-without-user-fails', { messages: [sys('Be brief.'), bot('Hey.')] })
    add('system-in-history', { messages: [sys('Be brief.'), user('Hi'), sys('Aside.'), bot('Hey.'), user('Again')] })
    add('function-in-history-dropped', { messages: [sys('Be brief.'), user('Hi'), fn('result'), bot('Hey.'), user('Again')] })
    add('empty-contents-filtered', { messages: [sys('Be brief.'), user('Hi'), bot(''), user('')] })
    add('empty-user-last', { messages: [sys('Be brief.'), user('Hi'), bot('Hey.'), user('')] })
    add('empty-system-skips-preamble', { messages: [sys(''), user('Hi'), bot('Hey.'), user('Again')] })
    add('preamble-without-history', { messages: [sys('You are brief.'), user('Hi')] })
    add('preamble-with-history', { messages: [sys('You are brief.'), user('Hi'), bot('Hey.'), user('Again')] })
    add('unicode-and-escapes', { messages: [sys('日本語 \u{1F600}'), user('"quoted" \\ back\nline  ')] })
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' }, messages: chat })
    add('tool-call-marker-stays-text', { messages: [sys('Be brief.'), user('Hi'), bot('A<tool_call>k1</tool_call>B'), user('Again')] })
    for (const model of ['cohere-command-r-03-2024', 'cohere-command-r-plus-04-2024', 'cohere-command-r-08-2024', 'cohere-command-r-plus', 'cohere-command-r-plus-08-2024']) add(`model-${model}`, { model, messages: chat })
    add('max-tokens-is-not-sent', { maxTokens: 77, messages: chat })
    add('stream-flag-is-ignored', { stream: true, messages: chat })
    add('temperature-disabled', { db: { temperature: -1000 }, messages: chat })
    add('penalties-disabled', { db: { frequencyPenalty: -1000, PresensePenalty: -1000 }, messages: chat })
    add('top-k-zero-kept', { db: { top_k: 0 }, messages: chat })
    add('separate-parameters-model', { db: separate, messages: chat })
    add('separate-parameters-submodel', { db: separate, mode: 'submodel', messages: chat })
    add('separate-parameters-memory', { db: separate, mode: 'memory', messages: chat })
    add('separate-parameters-by-model-missing-throws', { db: { ...separate, seperateParametersByModel: true }, messages: chat })
    add('separate-parameters-by-model', { db: { ...separate, seperateParametersByModel: true, seperateParameters: { ...separate.seperateParameters, overrides: { [C]: sep(33, 6, 0.33, 5) } } }, messages: chat })
    const extra = pairs => ({ db: { additionalParams: pairs }, messages: chat })
    add('additional-number', extra([['seed', '7']]))
    add('additional-string-and-bool', extra([['user', '"abc"'], ['return_prompt', 'false']]))
    add('additional-json', extra([['metadata', 'json::{"a":1}']]))
    add('additional-nested', extra([['connectors.0', 'json::{"id":"web"}'], ['x.y', '1']]))
    add('additional-header', extra([['header::X-Test', 'on'], ['header::Authorization', 'Bearer other']]))
    add('additional-none-removes', extra([['safety_mode', '{{none}}'], ['k', '{{none}}'], ['header::Content-Type', '{{none}}']]))
    add('additional-overrides-body-keys', extra([['message', 'replaced'], ['chat_history', 'json::[]'], ['safety_mode', 'CONTEXTUAL']]))
    add('additional-null-and-text', extra([['x', 'null'], ['y', 'plain text']]))
    add('additional-applies-to-every-model', { model: 'cohere-command-r-plus-04-2024', ...extra([['seed', '9']]) })
    add('additional-ignored-when-empty-value', extra([['seed', '']]))
    add('cohere-key-in-header', { db: { cohereAPIKey: 'sk-other' }, messages: chat })
    add('reverse-proxy', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/v1/chat' }, messages: chat })
    add('reverse-proxy-query-url', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://Proxy.Example.test:8443/a b?x=1#f' }, messages: chat })
    add('reverse-proxy-no-url-declines', { model: 'reverse_proxy', messages: chat, declines: 'url-form' })
    add('reverse-proxy-userinfo-declines', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://u:p@proxy.example.test/chat' }, messages: chat, declines: 'url-form' })
    add('reverse-proxy-model-params', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/chat', ...separate, seperateParametersByModel: true, seperateParameters: { ...separate.seperateParameters, overrides: { reverse_proxy: sep(44, 4, 0.44, 4) } } }, messages: chat })
    add('custom-model', { model: 'xcustom:::one', customModels: [customModel()], messages: chat })
    add('custom-model-no-url', { model: 'xcustom:::one', customModels: [customModel({ url: undefined })], messages: chat })
    add('custom-model-no-key-uses-cohere-key', { model: 'xcustom:::one', customModels: [customModel({ key: undefined })], messages: chat })
    add('custom-model-empty-key-kept', { model: 'xcustom:::one', customModels: [customModel({ key: '' })], messages: chat })
    add('custom-model-params', { model: 'xcustom:::one', customModels: [customModel({ params: 'seed=9\nheader::X-Custom=yes' })], messages: chat })
    const raw = (id, messages, extra = {}) => add(id, { model: 'xcustom:::one', customModels: [customModel()], messages, ...extra })
    raw('raw-assistant-last-with-history', [sys('Be brief.'), user('A'), bot('B'), user('C'), bot('D')])
    raw('raw-function-last-with-history', [user('A'), bot('B'), user('C'), bot('D'), fn('E')])
    raw('raw-system-last-with-history', [user('A'), bot('B'), user('C'), sys('D')])
    raw('raw-three-trailing', [sys('S'), user('A'), bot('B'), user('C'), bot('D'), sys('E'), fn('F')])
    raw('raw-system-first-only-user', [sys('S'), user('A')])
    raw('raw-system-in-history', [sys('S'), user('A'), sys('M'), bot('B'), user('C')])
    raw('raw-function-in-history', [user('A'), fn('M'), bot('B'), user('C')])
    raw('raw-no-user-fails', [sys('S'), bot('B')])
    raw('raw-only-assistant-fails', [bot('B')])
    raw('raw-only-system-throws', [sys('S')])
    raw('raw-empty-contents', [sys(''), user(''), bot(''), user('')])
    add('other-format-declines', { model: 'gpt4o', messages: chat, declines: 'format' })
    write('requests', cases)
}

const text = value => ({ text: value })
function writeResponses() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: C, messages: chat, ...entry })
    add('text', ok(text('Hello there.')))
    add('text-unicode', ok(text('日本語 \u{1F600} "q" \\ \n')))
    add('text-whitespace-only', ok(text(' ')))
    add('text-with-extra-fields', ok({ text: 'Hi', generation_id: 'g', meta: { tokens: 3 } }))
    add('text-empty-fails', ok(text('')))
    add('text-missing-fails', ok({ generation_id: 'g' }))
    add('text-null-fails', ok({ text: null }))
    add('text-zero-fails', ok({ text: 0 }))
    add('text-false-fails', ok({ text: false }))
    add('data-null-fails', ok(null))
    add('data-array-fails', ok([1, 2]))
    add('data-string-fails', ok('text'))
    add('data-number-fails', ok(5))
    add('data-numbers-echo', { replies: [{ text: '{"x":1.0,"y":1e21,"z":0.1,"big":12345678901234567890,"s":"\\u00e9","t":[1,2,{"a":null}]}' }] })
    add('text-bom', { replies: [{ text: '﻿{"text":"Bom."}' }] })
    add('http-400-json', status(400, { message: 'invalid api token' }))
    add('http-400-with-text', status(400, { text: 'Looks like success', message: 'no' }))
    add('http-500-json-null', status(500, null))
    add('http-401-text', { replies: [{ status: 401, text: 'unauthorized' }] })
    add('http-500-empty', { replies: [{ status: 500, text: '' }] })
    add('http-500-html', { replies: [{ status: 500, text: '<html>oops</html>' }] })
    add('http-200-not-json', { replies: [{ text: 'not json' }] })
    add('http-200-empty-body', { replies: [{ text: '' }] })
    add('http-201-ok', status(201, text('Created.')))
    add('http-302-with-text', status(302, text('Redirected.')))
    add('transport-error', { replies: [{ throws: 'fetch failed' }] })
    add('http-error-long-message', { replies: [{ status: 400, text: 'x'.repeat(3000) }] })
    add('text-number-declines', { ...ok({ text: 5 }), declines: 'response-text-not-text' })
    add('text-true-declines', { ...ok({ text: true }), declines: 'response-text-not-text' })
    add('text-object-declines', { ...ok({ text: { a: 1 } }), declines: 'response-text-not-text' })
    add('text-array-declines', { ...ok({ text: ['a'] }), declines: 'response-text-not-text' })
    write('responses', cases)
}

writeRequests()
writeResponses()
