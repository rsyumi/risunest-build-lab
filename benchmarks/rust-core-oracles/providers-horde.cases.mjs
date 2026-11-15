// Writes the case files the Horde oracle (providers-horde.oracle.test.ts) reads: requests and poll runs of `horde:::`
// models under tests/fixtures/rust-core/providers/horde. The cases are deterministic and use synthetic values only.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/horde')
mkdirSync(out, { recursive: true })

const sys = text => ({ role: 'system', content: text })
const user = text => ({ role: 'user', content: text })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const baseDb = {
    usePlainFetch: true,
    hordeConfig: { apiKey: '', model: '', softPrompt: '' },
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
    customAPIFormat: 16,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

const queued = (body = { id: 'job-1', kudos: 10 }) => ({ status: 202, json: body })
const finished = text => ({ json: { is_possible: true, done: true, generations: [{ text }] } })
const waiting = { json: { is_possible: true, done: false } }
const model = 'horde:::koboldcpp/Synth-7B'

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model, messages: chat, replies: [queued(), finished('ok')], ...entry })
    add('basic', {})
    add('multi-turn', { messages: talk })
    add('auto-model', { model: 'horde:::auto' })
    add('auto-spaced-keeps-models', { model: 'horde::: auto' })
    add('spaced-model', { model: 'horde:::  Synth Model  ' })
    add('extra-separator', { model: 'horde:::first:::second' })
    add('empty-model-name', { model: 'horde:::' })
    add('api-key', { db: { hordeConfig: { apiKey: 'synthetic-horde-key', model: '', softPrompt: '' } } })
    add('api-key-two-units', { db: { hordeConfig: { apiKey: 'ab', model: '', softPrompt: '' } } })
    add('api-key-three-units', { db: { hordeConfig: { apiKey: 'abc', model: '', softPrompt: '' } } })
    add('settings-values', { db: { maxContext: 8192, maxResponse: 512, temperature: 0, top_k: 0, top_p: 1 } })
    add('settings-fractions', { db: { maxContext: 1000.5, temperature: 75, top_p: 0.95 } })
    add('max-tokens-argument-ignored', { maxTokens: 77 })
    add('unicode-and-escapes', { messages: [sys('日本語 \u{1F600}'), user('"quoted" \\ back\nline')] })
    add('empty-messages', { messages: [] })
    const extra = pairs => ({ db: { additionalParams: pairs, applyAdditionalParamsToAll: true } })
    add('additional-number', extra([['seed', '7']]))
    add('additional-nested', extra([['params.n', '2'], ['params.singleline', 'true']]))
    add('additional-removes-prompt', extra([['prompt', '{{none}}']]))
    add('additional-header', extra([['header::apikey', 'override'], ['header::X-Test', 'on']]))
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' } })
    add('chat-template-llama3', { db: { instructChatTemplate: 'llama3' }, messages: talk })
    add('submodel-mode', { mode: 'submodel' })
    add('reverse-proxy-throws', { model: 'reverse_proxy' })
    add('custom-model', {
        model: 'xcustom:::synth', customModels: [{ id: 'xcustom:::synth', name: 'H', internalId: 'h', url: 'https://ignored.example.test/', key: 'sk-synthetic-custom', format: 16, flags: [], tokenizer: 0, params: '' }],
    })
    write('requests', cases)
    return cases.length
}

function writeResponses() {
    const cases = []
    const add = (id, replies, entry = {}) => cases.push({ id, model, messages: talk, replies, ...entry })
    add('done-first-poll', [queued(), finished(' She nods.')])
    add('waits-then-done', [queued(), waiting, { json: { is_possible: true, done: false, generations: [] } }, finished('Later.')])
    add('done-empty-generations-polls', [queued(), { json: { is_possible: true, done: true, generations: [] } }, finished('Then.')])
    add('done-generations-not-array-polls', [queued(), { json: { is_possible: true, done: true, generations: 'x' } }, finished('Then.')])
    add('generations-before-done-polls', [queued(), { json: { is_possible: true, done: false, generations: [{ text: 'early' }] } }, finished('Final.')])
    add('output-cut-at-user', [queued(), finished(' She nods.\nMina: what?')])
    add('output-cut-at-char', [queued(), finished('Steps in.\nAria: again')])
    add('output-cut-at-message-name', [queued(), finished('Ok. Guide: next')], { messages: [user('Hi'), bot('Hey', { name: 'Guide' })] })
    add('output-persona', [queued(), finished('Fine. Hero: no')], { persona: 'Hero' })
    add('text-missing', [queued(), { json: { is_possible: true, done: true, generations: [{}] } }])
    add('text-null', [queued(), { json: { is_possible: true, done: true, generations: [{ text: null }] } }])
    add('generation-number', [queued(), { json: { is_possible: true, done: true, generations: [5] } }])
    add('generation-null-throws', [queued(), { json: { is_possible: true, done: true, generations: [null] } }])
    add('text-number-declines', [queued(), { json: { is_possible: true, done: true, generations: [{ text: 5 }] } }], { declines: 'result-text-not-text' })
    add('not-possible', [queued(), { json: { is_possible: false } }])
    add('not-possible-with-message', [queued({ id: 'job-2', message: 'queued behind 3' }), { json: { is_possible: false } }])
    add('message-number', [queued({ id: 'job-3', message: 5 }), { json: { is_possible: false } }])
    add('message-empty', [queued({ id: 'job-4', message: '' }), { json: { is_possible: false } }])
    add('is-possible-missing', [queued(), { json: {} }])
    add('possible-checked-before-done', [queued(), { json: { is_possible: false, done: true, generations: [{ text: 'x' }] } }])
    add('status-http-error-ignored', [queued(), { status: 500, json: { is_possible: true, done: true, generations: [{ text: 'ok' }] } }])
    add('create-rejected', [{ status: 401, text: '{"message":"Unauthorized"}' }])
    add('create-200-rejected', [{ status: 200, json: { id: 'x' } }])
    add('create-not-json-throws', [{ status: 202, text: 'nope' }])
    add('create-empty-body-throws', [{ status: 202, text: '' }])
    add('create-null-throws', [{ status: 202, json: null }])
    add('create-transport-throws', [{ throws: 'fetch failed' }])
    add('status-not-json-throws', [queued(), { text: 'Bad gateway' }])
    add('status-null-throws', [queued(), { json: null }])
    add('status-transport-throws', [queued(), { throws: 'fetch failed' }])
    add('id-missing', [queued({ kudos: 1 }), finished('ok')])
    add('id-number', [queued({ id: 42 }), finished('ok')])
    add('create-string-root', [{ status: 202, json: 'queued' }, finished('ok')])
    add('id-form-declines', [queued({ id: 'a/b?c' }), finished('ok')], { declines: 'id-form' })
    add('id-object-declines', [queued({ id: {} }), finished('ok')], { declines: 'id-form' })
    write('responses', cases)
    return cases.length
}

console.log(`requests ${writeRequests()}, responses ${writeResponses()}`)
