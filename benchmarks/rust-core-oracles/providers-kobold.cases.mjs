// Writes the case files the Kobold oracle (providers-kobold.oracle.test.ts) reads: requests and responses under
// tests/fixtures/rust-core/providers/kobold. The cases are deterministic and use synthetic values only.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/kobold')
mkdirSync(out, { recursive: true })

const sys = text => ({ role: 'system', content: text })
const user = text => ({ role: 'user', content: text })
const bot = text => ({ role: 'assistant', content: text })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const baseDb = {
    usePlainFetch: true,
    koboldURL: 'http://kobold.example.test:5001',
    instructChatTemplate: 'chatml',
    username: 'Mina',
    maxContext: 4000,
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
    seperateParametersEnabled: false,
    seperateParametersByModel: false,
    applyAdditionalParamsToAll: false,
    additionalParams: [],
    systemContentReplacement: '',
    systemRoleReplacement: '',
    proxyKey: 'sk-synthetic-proxy',
    forceReplaceUrl: '',
    customProxyRequestModel: 'my-proxy-model',
    customAPIFormat: 14,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

const sep = (t, k, p, r) => ({ temperature: t, top_k: k, repetition_penalty: r, min_p: 0, top_a: 0.5, top_p: p, frequency_penalty: 0, presence_penalty: 0, thinking_tokens: 0, reasoning_effort: 1, verbosity: 1 })
const separate = {
    seperateParametersEnabled: true,
    seperateParameters: {
        model: sep(50, 10, 0.5, 1.2), submodel: sep(60, 11, 0.6, 1.3), memory: sep(61, 11, 0.6, 1.3), emotion: sep(62, 11, 0.6, 1.3),
        translate: sep(63, 11, 0.6, 1.3), otherAx: sep(70, 12, 0.7, 1.4),
        overrides: { kobold: sep(30, 5, 0.3, -1000) },
    },
}

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

const K = 'kobold'

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: K, messages: chat, ...entry })
    add('basic', {})
    add('multi-turn', { messages: talk })
    add('unicode-and-escapes', { messages: [sys('日本語 \u{1F600}'), user('"quoted" \\ back\nline  ')] })
    add('empty-messages', { messages: [] })
    add('max-tokens-argument', { maxTokens: 77 })
    add('url-with-path-kept', { db: { koboldURL: 'http://kobold.example.test:5001/custom/generate' } })
    add('url-two-unit-path-replaced', { db: { koboldURL: 'http://kobold.example.test/ab' } })
    add('url-three-unit-path-kept', { db: { koboldURL: 'http://kobold.example.test/abc' } })
    add('url-trailing-slash-replaced', { db: { koboldURL: 'https://kobold.example.test/' } })
    add('url-query-kept', { db: { koboldURL: 'http://kobold.example.test/?key=1' } })
    add('url-default-port-dropped', { db: { koboldURL: 'HTTP://Kobold.Example.Test:80' } })
    add('url-userinfo-declines', { declines: 'url-form', db: { koboldURL: 'http://user:pw@kobold.example.test/' } })
    add('url-invalid-declines', { declines: 'url-form', db: { koboldURL: 'not a url' } })
    add('max-context', { db: { maxContext: 8192 } })
    add('temperature-disabled', { db: { temperature: -1000 } })
    add('top-k-zero-kept', { db: { top_k: 0 } })
    add('repetition-penalty-disabled', { db: { repetition_penalty: -1000 } })
    add('separate-parameters-model', { db: separate })
    add('separate-parameters-submodel', { db: separate, mode: 'submodel' })
    add('separate-parameters-memory', { db: separate, mode: 'memory' })
    add('separate-parameters-by-model', { db: { ...separate, seperateParametersByModel: true } })
    add('separate-parameters-by-model-missing-throws', { db: { ...separate, seperateParametersByModel: true, seperateParameters: { ...separate.seperateParameters, overrides: {} } } })
    const extra = pairs => ({ db: { additionalParams: pairs, applyAdditionalParamsToAll: true } })
    add('additional-not-applied-without-flag', { db: { additionalParams: [['seed', '7']] } })
    add('additional-number', extra([['seed', '7']]))
    add('additional-string-and-bool', extra([['sampler_seed', '"abc"'], ['use_default_badwordsids', 'false']]))
    add('additional-json', extra([['sampler_order', 'json::[6,0,1,3,4,2,5]']]))
    add('additional-nested', extra([['grammar.root', '"x"']]))
    add('additional-overrides-prompt', extra([['prompt', 'replaced']]))
    add('additional-removes-n', extra([['n', '{{none}}']]))
    add('additional-header', extra([['header::X-Test', 'on'], ['header::content-type', 'text/plain']]))
    add('additional-header-removed', extra([['header::content-type', '{{none}}']]))
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' } })
    add('template-llama3', { db: { instructChatTemplate: 'llama3' }, messages: talk })
    add('template-alpaca', { db: { instructChatTemplate: 'alpaca' }, messages: talk })
    add('custom-model', {
        model: 'xcustom:::kobold', customModels: [{ id: 'xcustom:::kobold', name: 'K', internalId: 'k', url: 'http://ignored.example.test/', key: 'sk-synthetic-custom', format: 14, flags: [], tokenizer: 0, params: 'seed=9' }],
    })
    add('reverse-proxy-format', { model: 'reverse_proxy', db: { additionalParams: [['seed', '3']] } })
    write('requests', cases)
    return cases.length
}

function writeResponses() {
    const cases = []
    const add = (id, replies, entry = {}) => cases.push({ id, model: K, messages: chat, replies, ...entry })
    add('text', [{ json: { results: [{ text: 'Hello there.' }] } }])
    add('text-unicode', [{ json: { results: [{ text: '안녕 \u{1F600}\n' } ] } }])
    add('text-empty', [{ json: { results: [{ text: '' }] } }])
    add('extra-results-ignored', [{ json: { results: [{ text: 'first' }, { text: 'second' }] } }])
    add('status-201-ok', [{ status: 201, json: { results: [{ text: 'created' }] } }])
    add('text-number-declines', [{ json: { results: [{ text: 7 }] } }], { declines: 'result-text-not-text' })
    add('text-missing-declines', [{ json: { results: [{}] } }], { declines: 'result-text-not-text' })
    add('results-missing-throws', [{ json: { other: 1 } }], { declines: 'thrown' })
    add('results-empty-throws', [{ json: { results: [] } }], { declines: 'thrown' })
    add('json-null-throws', [{ json: null }], { declines: 'thrown' })
    add('http-error-object', [{ status: 500, json: { detail: { msg: 'boom', type: 'server' } } }])
    add('http-error-string', [{ status: 503, json: 'Server busy' }])
    add('http-error-array', [{ status: 422, json: [1, 'two'] }])
    add('http-error-not-json', [{ status: 502, text: 'Bad gateway' }])
    add('ok-not-json', [{ status: 200, text: 'not json' }])
    add('ok-empty-body', [{ status: 200, text: '' }])
    add('transport-error', [{ throws: 'fetch failed' }])
    write('responses', cases)
    return cases.length
}

console.log(`requests ${writeRequests()}, responses ${writeResponses()}`)
