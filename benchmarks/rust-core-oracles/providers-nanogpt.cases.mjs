// Synthetic NanoGPT chat-completions cases for the existing OpenAI fetch-boundary oracle.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/nanogpt')
mkdirSync(out, { recursive: true })
const baseDb = {
    usePlainFetch: true, requestRetrys: 0, genTime: 1, useStreaming: false,
    openAIKey: 'sk-synthetic-openai', proxyKey: 'sk-synthetic-proxy', openrouterKey: 'sk-synthetic-router',
    nanogptKey: 'sk-synthetic-nano', nanogptRequestModel: 'synthetic/model', nanogptProvider: '', nanogptUseSubscriptionEndpoint: false,
    OaiCompAPIKeys: {}, applyAdditionalParamsToAll: false, additionalParams: [], maxResponse: 300,
    temperature: 80, top_k: 40, repetition_penalty: 1.1, min_p: 0.05, top_a: 0.2, top_p: 0.9,
    frequencyPenalty: 20, PresensePenalty: 30, thinkingTokens: 1024, reasoningEffort: 1, verbosity: 1,
    newOAIHandle: true, generationSeed: -1, gptVisionQuality: 'low', jsonSchemaEnabled: false,
    jsonSchema: '{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}', strictJsonSchema: true,
    extractJson: '', OAIPrediction: '', seperateParametersEnabled: false, seperateParametersByModel: false,
    simplifiedToolUse: false, reverseProxyOobaMode: false, reverseProxyOobaArgs: {}, proxyRequestModel: '', customProxyRequestModel: '',
    openrouterRequestModel: 'synthetic/router', openrouterFallback: false, openrouterMiddleOut: false,
    openrouterProvider: { order: [], only: [], ignore: [] }, useInstructPrompt: false, deepseekThinkingType: 'disabled',
    autofillRequestUrl: false, openAIFlexProcessing: false, systemContentReplacement: '', systemRoleReplacement: '', forceReplaceUrl: '',
    enableCustomFlags: false, customFlags: [], localNetworkMode: false,
}
const sys = content => ({ role: 'system', content })
const user = content => ({ role: 'user', content })
const bot = (content, extra = {}) => ({ role: 'assistant', content, ...extra })
const chat = [sys('Synthetic system.'), user('Synthetic question.')]
const lookup = { name: 'lookup', description: 'Synthetic lookup.', inputSchema: { type: 'object', properties: { query: { type: 'string' } }, required: ['query'] } }
const flags = (...customFlags) => ({ enableCustomFlags: true, customFlags })
const add = (list, id, extra = {}) => list.push({ id, model: 'nanogpt', messages: chat, ...extra })
const write = (name, cases) => writeFileSync(resolve(out, `${name}.cases.json`), `${JSON.stringify({ schema: 1, baseDb, cases }, null, 2)}\n`)

const requests = []
for (const subscription of [false, true]) for (const provider of ['', 'synthetic-provider', 'provider/with:punctuation']) {
    add(requests, `${subscription ? 'subscription' : 'standard'}-${provider ? provider.startsWith('provider/') ? 'punctuation-provider' : 'provider' : 'auto'}`, {
        db: { nanogptUseSubscriptionEndpoint: subscription, nanogptProvider: provider },
    })
}
for (const model of ['', 'gpt35', 'gpt4o', 'synthetic/model:variant', 'model 😀', 'model\ud800']) {
    add(requests, `model-${requests.length}`, { db: { nanogptRequestModel: model } })
}
add(requests, 'empty-key', { db: { nanogptKey: '' } })
add(requests, 'explicit-key', { key: 'sk-synthetic-override' })
add(requests, 'explicit-empty-key', { key: '' })
add(requests, 'custom-url-ignored', { customURL: 'https://ignored.example.test/completions' })
add(requests, 'subscription-custom-url-ignored', { customURL: 'https://ignored.example.test/completions', db: { nanogptUseSubscriptionEndpoint: true } })
add(requests, 'sampling-seed-prediction', { db: { generationSeed: 73, OAIPrediction: 'Synthetic prediction.', temperature: 0, top_p: 0 } })
add(requests, 'schema', { schema: baseDb.jsonSchema })
add(requests, 'database-schema', { db: { jsonSchemaEnabled: true, strictJsonSchema: false } })
add(requests, 'additional-ignored', { db: { additionalParams: [['model', 'ignored'], ['header::X-Provider', 'ignored']] } })
add(requests, 'additional-overrides', { db: { applyAdditionalParamsToAll: true, nanogptProvider: 'original', additionalParams: [['model', 'override/model'], ['header::Authorization', 'Bearer sk-synthetic-additional'], ['header::X-Provider', 'overridden'], ['stream', 'true']] } })
add(requests, 'additional-delete-provider', { db: { applyAdditionalParamsToAll: true, nanogptProvider: 'original', additionalParams: [['header::X-Provider', '{{none}}'], ['max_completion_tokens', '{{none}}'], ['max_tokens', '41']] } })
add(requests, 'flex-not-added', { db: { openAIFlexProcessing: true } })
add(requests, 'multigen-not-enabled', { db: { genTime: 3 }, tools: [lookup] })
add(requests, 'image', { messages: [sys('Synthetic image.'), { ...user('Describe.'), multimodals: [{ type: 'image', base64: 'data:image/png;base64,c3ludGhldGlj' }] }], bias: { 23: -90 } })
add(requests, 'text-bias-removed-with-main-image-model', { bias: { 23: -90 } })
add(requests, 'text-bias-kept-with-main-text-model', { mainModel: 'gpt35', bias: { 23: -90 } })
add(requests, 'blank-and-example-name', { messages: [sys(''), { ...user('Example.'), name: 'example_user' }, bot('Example answer.', { name: 'example_assistant' }), user('')] })
add(requests, 'developer-role-and-max-tokens', { db: flags(6, 8, 14), maxTokens: 47 })
add(requests, 'stream-request', { stream: true, db: { nanogptUseSubscriptionEndpoint: true, nanogptProvider: 'stream-provider' } })
add(requests, 'stream-forces-additional-false', { stream: true, db: { applyAdditionalParamsToAll: true, additionalParams: [['stream', 'false']] } })
add(requests, 'tools', { tools: [lookup] })
write('requests', requests)

const one = (content, extra = {}) => ({ json: { choices: [{ message: bot(content, extra) }] } })
const tool = { id: 'synthetic-call', type: 'function', function: { name: 'lookup', arguments: '{"query":"synthetic"}' } }
const responses = []
add(responses, 'text', { reply: one('Synthetic answer.') })
add(responses, 'subscription-provider-text', { db: { nanogptUseSubscriptionEndpoint: true, nanogptProvider: 'synthetic-provider' }, reply: one('Subscription answer.') })
add(responses, 'null-content', { reply: one(null) })
add(responses, 'reasoning', { reply: one('Answer.', { reasoning_content: 'Synthetic reasoning.' }) })
add(responses, 'legacy-text', { reply: { json: { choices: [{ text: 'Synthetic legacy text.' }] } } })
add(responses, 'multiple-choices-stays-single', { db: { genTime: 3 }, reply: { json: { choices: [{ message: bot('First.') }, { message: bot('Second.') }] } } })
add(responses, 'extract-json', { schema: baseDb.jsonSchema, extractJson: 'answer', reply: one('{"answer":"Synthetic extraction."}') })
add(responses, 'http-error', { reply: { status: 429, json: { error: { message: 'Synthetic limit.' } } } })
add(responses, 'http-error-without-message', { reply: { status: 500, json: { detail: 'Synthetic failure.' } } })
add(responses, 'missing-choices', { reply: { json: { result: 'Synthetic absent choice.' } } })
add(responses, 'transport-error', { reply: { throws: 'Synthetic network failure.' } })
for (const simplified of [false, true]) add(responses, `tool-round-${simplified}`, {
    db: { nanogptUseSubscriptionEndpoint: true, nanogptProvider: 'tool-provider', simplifiedToolUse: simplified }, tools: [lookup],
    replies: [one('', { tool_calls: [tool] }), one('Synthetic final.')], toolReplies: [[{ type: 'text', text: 'Synthetic tool reply.' }]],
})
write('responses', responses)

const streams = []
const event = delta => `data: ${JSON.stringify({ choices: [{ index: 0, delta }] })}\n\n`
const done = 'data: [DONE]\n\n'
const chunks = parts => ({ chunks: parts.map(utf8 => ({ utf8 })) })
add(streams, 'content', { stream: true, reply: chunks([event({ content: 'Synthetic' }), event({ content: ' answer.' }), done]) })
add(streams, 'subscription-provider', { stream: true, db: { nanogptUseSubscriptionEndpoint: true, nanogptProvider: 'stream-provider' }, reply: chunks([event({ content: 'Subscribed.' }), done]) })
add(streams, 'reasoning', { stream: true, reply: chunks([event({ reasoning_content: 'Thinking.' }), event({ content: 'Answer.' }), done]) })
add(streams, 'event-split', { stream: true, reply: chunks(['data: {"cho', 'ices":[{"index":0,"delta":{"content":"Split."}}]}\n', '\n', done]) })
add(streams, 'unicode-byte-split', { stream: true, reply: { chunks: [
    { utf8: 'data: {"choices":[{"index":0,"delta":{"content":"' }, { hex: 'f09f' }, { hex: '9880' }, { utf8: '"}}]}\n\n' }, { utf8: done },
] } })
add(streams, 'empty', { stream: true, reply: chunks([done]) })
add(streams, 'malformed-event-ignored', { stream: true, reply: chunks(['data: invalid\n\n', event({ content: 'Valid.' }), done]) })
add(streams, 'non-success-status', { stream: true, reply: { status: 429, contentType: 'text/plain', ...chunks(['Synthetic rate limit.']) } })
add(streams, 'wrong-content-type', { stream: true, reply: { contentType: 'application/json', ...chunks(['{"error":"Synthetic content type."}']) } })
add(streams, 'extract-json', { stream: true, schema: baseDb.jsonSchema, extractJson: 'answer', reply: chunks([event({ content: '{"answer":"Synthetic stream."}' }), done]) })
add(streams, 'multigen-stays-single', { stream: true, db: { genTime: 3 }, reply: chunks([event({ content: 'Only.' }), done]) })
add(streams, 'tool-round', {
    stream: true, tools: [lookup], db: { nanogptUseSubscriptionEndpoint: true, nanogptProvider: 'tool-provider' },
    replies: [chunks([event({ tool_calls: [{ index: 0, ...tool }] }), done]), chunks([event({ content: 'Final.' }), done])],
    toolReplies: [[{ type: 'text', text: 'Synthetic stream tool.' }]],
})
write('streams', streams)
console.log(`NanoGPT cases: ${requests.length} requests, ${responses.length} responses, ${streams.length} streams`)
