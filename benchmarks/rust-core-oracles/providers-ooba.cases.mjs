// Writes the case files the Ooba oracle (providers-ooba.oracle.test.ts) reads: requests and responses of `ooba`
// (LLMFormat.Ooba) and `mancer` (LLMFormat.OobaLegacy) under tests/fixtures/rust-core/providers/ooba. The cases are
// deterministic and use synthetic values only.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/ooba')
mkdirSync(out, { recursive: true })

const sys = text => ({ role: 'system', content: text })
const user = (text, extra = {}) => ({ role: 'user', content: text, ...extra })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const ooba = {
    max_new_tokens: 180, do_sample: true, temperature: 0.7, top_p: 0.9, typical_p: 1, repetition_penalty: 1.15, encoder_repetition_penalty: 1,
    top_k: 20, min_length: 0, no_repeat_ngram_size: 0, num_beams: 1, penalty_alpha: 0, length_penalty: 1, early_stopping: false, seed: -1,
    add_bos_token: true, truncation_length: 4096, ban_eos_token: false, skip_special_tokens: true, top_a: 0, tfs: 1, epsilon_cutoff: 0, eta_cutoff: 0,
    formating: { header: 'Header.', systemPrefix: '### Instruction:', userPrefix: '### Input:', assistantPrefix: '### Response:', seperator: '', useName: false },
}

const baseDb = {
    usePlainFetch: true,
    textgenWebUIBlockingURL: 'https://ooba.example.test/api/',
    textgenWebUIStreamURL: 'wss://ooba.example.test/api/',
    mancerHeader: 'sk-synthetic-mancer',
    instructChatTemplate: 'chatml',
    username: 'Mina',
    ooba,
    reverseProxyOobaArgs: { mode: 'instruct' },
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
    customAPIFormat: 13,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

function shared(add) {
    add('basic', {})
    add('multi-turn', { messages: talk })
    add('unicode-and-escapes', { messages: [sys('日本語 \u{1F600}'), user('"quoted" \\ back\nline  ')] })
    add('max-tokens-argument', { maxTokens: 77 })
    add('temperature-zero', { db: { temperature: 0 } })
    add('username-in-stop-strings', { db: { username: 'Ren' } })
    add('user-prefix-duplicate', { db: { ooba: { ...ooba, formating: { ...ooba.formating, userPrefix: '### user' } } } })
    add('user-prefix-empty', { db: { ooba: { ...ooba, formating: { ...ooba.formating, userPrefix: '' } } } })
    add('user-prefix-missing', { db: { ooba: { ...ooba, formating: { systemPrefix: '', assistantPrefix: '', seperator: '', useName: false } } } })
    add('local-stop-strings-empty', { db: { localStopStrings: [] } })
    add('local-stop-strings-declines', { declines: 'local-stop-strings', db: { localStopStrings: ['STOP\\n'] } })
    const extra = pairs => ({ db: { additionalParams: pairs, applyAdditionalParamsToAll: true } })
    add('additional-not-applied-without-flag', { db: { additionalParams: [['seed', '7']] } })
    add('additional-number', extra([['seed', '7']]))
    add('additional-json', extra([['grammar', 'json::{"root":"x"}']]))
    add('additional-removes-prompt', extra([['prompt', '{{none}}']]))
    add('additional-header', extra([['header::Authorization', 'Bearer synthetic'], ['header::X-Test', 'on']]))
    add('template-alpaca', { db: { instructChatTemplate: 'alpaca' }, messages: talk })
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' } })
}

function writeRequests() {
    const cases = []
    const O = (id, entry) => cases.push({ id: `ooba-${id}`, model: 'ooba', messages: chat, ...entry })
    shared(O)
    O('url-path-replaced', { db: { textgenWebUIBlockingURL: 'http://ooba.example.test:5000/some/deep/path' } })
    O('url-query-and-hash-kept', { db: { textgenWebUIBlockingURL: 'http://ooba.example.test/api?x=1#frag' } })
    O('url-default-port', { db: { textgenWebUIBlockingURL: 'HTTPS://Ooba.Example.Test:443/api/' } })
    O('url-invalid-declines', { declines: 'url-form', db: { textgenWebUIBlockingURL: 'localhost:5000' } })
    O('url-userinfo-declines', { declines: 'url-form', db: { textgenWebUIBlockingURL: 'http://a:b@ooba.example.test/api' } })
    O('penalties-zero-fall-back', { db: { PresensePenalty: 0, frequencyPenalty: 0 } })
    O('penalties-disabled-value', { db: { PresensePenalty: -1000, frequencyPenalty: -1000 } })
    O('ooba-args-copied', { db: { reverseProxyOobaArgs: { mode: 'instruct', min_p: 0.1, top_k: 0, seed: 12, custom_token_bans: '1,2', grammar_string: '' } } })
    O('ooba-args-null-deletes-truthy', { db: { reverseProxyOobaArgs: { mode: 'chat', temperature: null, top_p: null, max_tokens: null } } })
    O('ooba-args-unknown-deletes-truthy', { db: { reverseProxyOobaArgs: { prompt: 'x', stop: ['a'], logit_bias: { 1: 2 }, presence_penalty: 0.5 } } })
    O('ooba-args-falsy-kept', { db: { temperature: 0, reverseProxyOobaArgs: { temperature: null, frequency_penalty: null } } })
    O('ooba-args-array-value', { db: { reverseProxyOobaArgs: { sampler_priority: ['a'], do_sample: false, mirostat_mode: 2 } } })
    O('stream-flag-is-ignored', { stream: true })
    O('custom-model', {
        model: 'xcustom:::ooba', customModels: [{ id: 'xcustom:::ooba', name: 'O', internalId: 'o', url: 'http://ignored.example.test/', key: 'sk-synthetic-custom', format: 13, flags: [], tokenizer: 0, params: 'seed=9' }],
    })
    const L = (id, entry) => cases.push({ id: `legacy-${id}`, model: 'mancer', messages: chat, ...entry })
    shared(L)
    L('url-api-suffix-replaced', { db: { textgenWebUIBlockingURL: 'https://mancer.example.test/api/v2/generate?x=1' } })
    L('url-no-api-kept', { db: { textgenWebUIBlockingURL: 'https://mancer.example.test/v1/generate' } })
    L('url-first-api-wins', { db: { textgenWebUIBlockingURL: 'https://mancer.example.test/x/apis/api/y' } })
    L('url-invalid-declines', { declines: 'url-form', db: { textgenWebUIBlockingURL: 'mancer.example.test/api' } })
    L('mancer-header-empty', { db: { mancerHeader: '' } })
    L('ooba-settings', { db: { ooba: { ...ooba, do_sample: false, top_p: 0.5, typical_p: 0.95, top_k: 0, num_beams: 2, ban_eos_token: true, add_bos_token: false } } })
    L('ooba-settings-missing-fields', { db: { ooba: { formating: ooba.formating, top_k: 3 } } })
    L('stream-declines', { declines: 'websocket-stream', stream: true })
    L('custom-model', {
        model: 'xcustom:::mancer', customModels: [{ id: 'xcustom:::mancer', name: 'M', internalId: 'm', url: 'http://ignored.example.test/', key: 'sk-synthetic-custom', format: 11, flags: [], tokenizer: 0, params: 'seed=9' }],
    })
    write('requests', cases)
    return cases.length
}

function writeResponses() {
    const cases = []
    const O = (id, replies, entry = {}) => cases.push({ id: `ooba-${id}`, model: 'ooba', messages: chat, replies, ...entry })
    O('text', [{ json: { choices: [{ text: 'Hello there.' }] } }])
    O('text-markers-removed', [{ json: { choices: [{ text: 'a##\nb##\n##c\n' }] } }])
    O('text-null', [{ json: { choices: [{ text: null }] } }])
    O('text-missing', [{ json: { choices: [{}] } }])
    O('text-number-declines', [{ json: { choices: [{ text: 5 }] } }], { declines: 'result-text-not-text' })
    O('choices-missing-throws', [{ json: { results: [] } }], { declines: 'thrown' })
    O('http-error-object', [{ status: 500, json: { error: { message: 'boom' } } }])
    O('http-error-string', [{ status: 503, json: 'busy' }])
    O('http-error-not-json', [{ status: 502, text: 'Bad gateway' }])
    O('ok-not-json', [{ status: 200, text: 'nope' }])
    O('transport-error', [{ throws: 'fetch failed' }])
    const L = (id, replies, entry = {}) => cases.push({ id: `legacy-${id}`, model: 'mancer', messages: talk, replies, ...entry })
    L('text', [{ json: { results: [{ text: 'She smiles.' }] } }])
    L('text-cut-at-user-name', [{ json: { results: [{ text: ' She smiles. \nMina: hi\nAria: hey' }] } }])
    L('text-cut-at-char-name', [{ json: { results: [{ text: 'Waves.Aria： more' }] } }])
    L('text-cut-at-system', [{ json: { results: [{ text: 'Done.\nsystem note: x' }] } }])
    L('text-cut-at-earliest', [{ json: { results: [{ text: 'A system: B Aria: C' }] } }])
    L('text-cut-at-message-name', [{ json: { results: [{ text: 'Ok. Guide: next' }] } }], { messages: [sys('Be brief.'), user('Hi'), bot('Hello', { name: 'Guide' }), user('Again')] })
    L('text-persona-name', [{ json: { results: [{ text: 'Sure. Hero: wait. Mina: no' }] } }], { persona: 'Hero' })
    L('text-untrimmed-without-chunk', [{ json: { results: [{ text: '  spaced  ' }] } }])
    L('text-null', [{ json: { results: [{ text: null }] } }])
    L('text-number-declines', [{ json: { results: [{ text: 5 }] } }], { declines: 'result-text-not-text' })
    L('results-missing-declines', [{ json: { choices: [] } }], { declines: 'result-shape' })
    L('results-empty-declines', [{ json: { results: [] } }], { declines: 'result-shape' })
    L('http-error-object', [{ status: 500, json: { detail: 'boom' } }])
    L('http-error-not-json', [{ status: 502, text: 'Bad gateway' }])
    L('transport-error', [{ throws: 'fetch failed' }])
    write('responses', cases)
    return cases.length
}

console.log(`requests ${writeRequests()}, responses ${writeResponses()}`)
