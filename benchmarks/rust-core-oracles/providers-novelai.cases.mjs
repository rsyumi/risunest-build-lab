// Writes the case files the NovelAI oracle (providers-novelai.oracle.test.ts) reads: requests and responses of
// `novelai` and `novelai_kayra` under tests/fixtures/rust-core/providers/novelai. The cases are deterministic and use
// synthetic values only; the token table stands in for the tokenizer.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/novelai')
mkdirSync(out, { recursive: true })

const sys = (text, extra = {}) => ({ role: 'system', content: text, ...extra })
const user = (text, extra = {}) => ({ role: 'user', content: text, ...extra })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const fn = text => ({ role: 'function', content: text })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const NAIsettings = {
    topK: 12, topP: 0.85, topA: 0.1, tailFreeSampling: 0.915, repetitionPenalty: 2.8, repetitionPenaltyRange: 2048, repetitionPenaltySlope: 0.02,
    repostitionPenaltyPresence: 0, seperator: '', frequencyPenalty: 0.03, presencePenalty: 0, typicalp: 1, starter: '', cfg_scale: 1, mirostat_tau: 0, mirostat_lr: 1,
}

const baseDb = {
    usePlainFetch: true,
    novelai: { token: 'sk-synthetic-novelai', model: '' },
    username: 'Mina',
    NAIsettings,
    NAIadventure: false,
    NAIappendName: true,
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
    forceReplaceUrl: 'https://proxy.example.test/api',
    customProxyRequestModel: 'my-proxy-model',
    customAPIFormat: 9,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

const settings = extra => ({ NAIsettings: { ...NAIsettings, ...extra } })

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: 'novelai', messages: chat, ...entry })
    add('basic', {})
    add('kayra', { model: 'novelai_kayra' })
    add('multi-turn', { messages: talk })
    add('continued', { continue: true, messages: talk })
    add('no-append-name', { db: { NAIappendName: false }, messages: talk })
    add('adventure', { db: { NAIadventure: true }, messages: talk })
    add('adventure-without-names', { db: { NAIadventure: true, NAIappendName: false }, messages: talk })
    add('named-messages', { messages: [sys('Be brief.'), user('Hi', { name: 'Traveler' }), bot('Hey', { name: 'Guide' }), bot('Plain'), fn('tool output'), user('Again')] })
    add('new-chat-markers', { messages: [sys('Be brief.'), sys('Example', { memo: 'NewChatExample' }), sys('Chat', { memo: 'NewChat' }), sys('[Start a new chat]'), sys('Other', { memo: 'NewChatX' }), user('Hi')] })
    add('separator-and-starter', { db: settings({ seperator: '\\n***\\n', starter: '\\n---' }), messages: [sys('[Start a new chat]'), user('Hi')] })
    add('separator-plain', { db: settings({ seperator: ' | ' }) })
    add('separator-number-declines', { db: settings({ seperator: 5 }), declines: 'nai-settings-text' })
    add('persona-user-name', { persona: 'Hero', messages: talk })
    add('unicode-and-escapes', { messages: [sys('日本語 \u{1F600}'), user('"quoted" \\ back\nline')] })
    add('empty-messages', { messages: [] })
    add('max-tokens-argument', { maxTokens: 77 })
    add('temperature-zero', { db: { temperature: 0 } })
    add('settings-values', { db: settings({ topK: 0, topP: 1, topA: 0, tailFreeSampling: 1, repetitionPenalty: 1, typicalp: 0.9, frequencyPenalty: 0, presencePenalty: 0.5 }) })
    add('settings-optional-defaults', { db: { NAIsettings: { ...NAIsettings, cfg_scale: undefined, mirostat_tau: undefined, mirostat_lr: undefined } } })
    add('settings-optional-null', { db: { NAIsettings: { ...NAIsettings, cfg_scale: null, mirostat_tau: null, mirostat_lr: null } } })
    add('settings-optional-values', { db: settings({ cfg_scale: 1.5, mirostat_tau: 5, mirostat_lr: 0.5 }) })
    add('bias-strings', { biasString: [['dragon', 2], [' the', -1.5], ['none', 1]], tokens: { dragon: [101, 102], ' the': [7] } })
    const extra = pairs => ({ db: { additionalParams: pairs, applyAdditionalParamsToAll: true } })
    add('additional-number', extra([['seed', '7']]))
    add('additional-nested', extra([['parameters.min_length', '5'], ['parameters.prefix', '"custom"']]))
    add('additional-removes-input', extra([['input', '{{none}}']]))
    add('additional-header', extra([['header::Authorization', 'Bearer other'], ['header::X-Test', 'on']]))
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' } })
    add('reverse-proxy-key', { model: 'reverse_proxy' })
    add('custom-model-key', {
        model: 'xcustom:::nai', customModels: [{ id: 'xcustom:::nai', name: 'N', internalId: 'n', url: 'https://ignored.example.test/', key: 'sk-synthetic-custom', format: 9, flags: [6], tokenizer: 0, params: '' }],
    })
    write('requests', cases)
    return cases.length
}

function writeResponses() {
    const cases = []
    const add = (id, replies, entry = {}) => cases.push({ id, model: 'novelai_kayra', messages: talk, replies, ...entry })
    add('output', [{ json: { output: ' She nods.' } }])
    add('output-cut-at-user', [{ json: { output: ' She nods.\nMina: what?' } }])
    add('output-cut-at-char', [{ json: { output: 'Steps in.\nAria： again' } }])
    add('output-cut-at-message-name', [{ json: { output: 'Ok. Guide: next' } }], { messages: [user('Hi'), bot('Hey', { name: 'Guide' })] })
    add('output-persona', [{ json: { output: 'Fine. Hero: no' } }], { persona: 'Hero' })
    add('output-untrimmed', [{ json: { output: '  spaced  ' } }])
    add('output-empty-fails', [{ json: { output: '' } }])
    add('output-missing-fails', [{ json: { other: 1 } }])
    add('output-number-declines', [{ json: { output: 5 } }], { declines: 'result-text-not-text' })
    add('json-null-throws', [{ json: null }], { declines: 'thrown' })
    add('http-error-object', [{ status: 401, json: { statusCode: 401, message: 'Unauthorized' } }])
    add('http-error-with-output', [{ status: 500, json: { output: 'partial' } }])
    add('http-error-not-json', [{ status: 502, text: 'Bad gateway' }])
    add('ok-not-json', [{ status: 200, text: 'nope' }])
    add('transport-error', [{ throws: 'fetch failed' }])
    write('responses', cases)
    return cases.length
}

console.log(`requests ${writeRequests()}, responses ${writeResponses()}`)
