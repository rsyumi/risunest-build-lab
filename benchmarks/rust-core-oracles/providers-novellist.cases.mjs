// Writes the case files the NovelList oracle (providers-novellist.oracle.test.ts) reads: requests and responses of
// `novellist` and `novellist_damsel` under tests/fixtures/rust-core/providers/novellist. The cases are deterministic and
// use synthetic values only.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const out = process.argv[2] ?? resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/novellist')
mkdirSync(out, { recursive: true })

const sys = text => ({ role: 'system', content: text })
const user = (text, extra = {}) => ({ role: 'user', content: text, ...extra })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const fn = text => ({ role: 'function', content: text })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('"Open" the door.'), bot('The door creaks. "Come in," she says.'), user('Walk in.')]

const ainconfig = { top_p: 0.7, rep_pen: 1.0625, top_a: 0.08, rep_pen_slope: 1.7, rep_pen_range: 1024, typical_p: 1, badwords: '', stoptokens: '', top_k: 140 }

const baseDb = {
    usePlainFetch: true,
    novellistAPI: 'sk-synthetic-novellist',
    username: 'Mina',
    ainconfig,
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
    customAPIFormat: 7,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
}

function write(name, cases) {
    writeFileSync(`${out}/${name}.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: 'novellist', messages: chat, ...entry })
    add('basic', {})
    add('damsel', { model: 'novellist_damsel' })
    add('multi-turn-quotes', { messages: talk })
    add('continued', { continue: true, messages: talk })
    add('quote-pairs-and-brackets', { messages: [user('Say "a" and "b" then 「c」 and "odd')] })
    add('bracket-across-newline', { messages: [bot('「line\nbreak」 then 「ok」 tail')] })
    add('nested-brackets', { messages: [bot('x「a「b」c」y')] })
    add('named-messages', { messages: [sys('Be brief.'), user('Hi', { name: 'Ignored' }), bot('「Hey」', { name: 'Guide' }), fn('result'), user('Again')] })
    add('new-chat-markers', { messages: [sys('Be brief.'), sys('[Start a new chat]'), user('Hi', { memo: 'newChatExample' }), user('Again', { memo: 'other' })] })
    add('function-message', { messages: [fn('tool output'), user('Next')] })
    add('persona-user-name', { persona: 'Hero', messages: talk })
    add('unicode-and-escapes', { messages: [sys('日本語 \u{1F600}'), user('back \\ slash sep')] })
    add('empty-messages', { messages: [] })
    add('max-tokens-argument', { maxTokens: 77 })
    add('bias-strings', { biasString: [['dragon', 2], ['「', -1.5], ['x', 0]] })
    add('ainconfig-values', { db: { ainconfig: { ...ainconfig, badwords: 'bad<<|>>words', stoptokens: '<<|>>」', top_k: 0 } } })
    add('ainconfig-missing-fields', { db: { ainconfig: { top_p: 0.5 } } })
    add('temperature-zero', { db: { temperature: 0 } })
    const extra = pairs => ({ db: { additionalParams: pairs, applyAdditionalParamsToAll: true } })
    add('additional-number', extra([['seed', '7']]))
    add('additional-removes-logit-bias-values', { ...extra([['logit_bias', '{{none}}'], ['typical_p', '"0.9"']]), biasString: [['dragon', 2]] })
    add('additional-header', extra([['header::Authorization', 'Bearer other'], ['header::X-Test', 'on']]))
    add('system-content-replacement', { db: { systemContentReplacement: 'Sys: {{slot}}', systemRoleReplacement: 'user' } })
    add('reverse-proxy-url', { model: 'reverse_proxy' })
    add('reverse-proxy-invalid-url-declines', { declines: 'url-form', model: 'reverse_proxy', db: { forceReplaceUrl: 'proxy' } })
    add('custom-model-url', {
        model: 'xcustom:::nl', customModels: [{ id: 'xcustom:::nl', name: 'N', internalId: 'n', url: 'https://custom.example.test/api', key: 'sk-synthetic-custom', format: 7, flags: [], tokenizer: 0, params: '' }],
    })
    write('requests', cases)
    return cases.length
}

function writeResponses() {
    const cases = []
    const add = (id, replies, entry = {}) => cases.push({ id, model: 'novellist', messages: talk, replies, ...entry })
    const data = text => [{ json: { data: [text] } }]
    add('narration-only', data('She nods.」'))
    add('quote-then-narration', data('Welcome.」 She smiles.'))
    add('user-line', data('Hi.」\n\nMina 「Hello?」 Aria 「Yes.」'))
    add('fullwidth-space-name', data('Hi.」Mina　「Wait」'))
    add('persona-user', data('Hi.」Hero 「Hey」'), { persona: 'Hero' })
    add('unclosed-quote', data('Hi.」 Aria 「unfinished'))
    add('trailing-name-trimmed', data('Done.」\n\nMina '))
    add('trailing-chunks-sequential', data('Done.」system note:Aria '))
    add('message-name-chunk', data('Hi.」Guide 「From the guide」'), { messages: [user('Hi'), bot('Hey', { name: 'Guide' })] })
    add('system-chunk', data('Hi.」system:「note」'))
    add('empty-text', data(''))
    add('extra-data-ignored', [{ json: { data: ['first', 'second'] } }])
    add('error-token-replaced', [{ json: { error: 'invalid token, token expired' } }])
    add('error-empty-string-ignored', [{ json: { error: '', data: ['ok'] } }])
    add('error-number-declines', [{ json: { error: 5 } }], { declines: 'error-not-text' })
    add('data-missing-throws', [{ json: { other: 1 } }], { declines: 'thrown' })
    add('text-number-coerced', [{ json: { data: [7] } }])
    add('text-missing-coerced', [{ json: { data: [] } }])
    add('json-null-throws', [{ json: null }], { declines: 'thrown' })
    add('http-error-object', [{ status: 401, json: { error: 'bad token' } }])
    add('http-error-string', [{ status: 503, json: 'busy' }])
    add('http-error-not-json', [{ status: 502, text: 'Bad gateway' }])
    add('ok-not-json', [{ status: 200, text: 'nope' }])
    add('transport-error', [{ throws: 'fetch failed' }])
    write('responses', cases)
    return cases.length
}

console.log(`requests ${writeRequests()}, responses ${writeResponses()}`)
