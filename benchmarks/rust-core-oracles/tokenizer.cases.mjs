// Writes the synthetic case files of the tokenizer oracle (tests/fixtures/rust-core/tokenizer/*.cases.json). Add
// cases here, run `node benchmarks/rust-core-oracles/tokenizer.cases.mjs`, then regenerate the expected files with
// RUST_CORE_ORACLE_WRITE=1.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/tokenizer/', import.meta.url))
mkdirSync(dir, { recursive: true })

// String encoding: a plain JSON string when well-formed, otherwise its UTF-16 units.
const str = (value) => value.isWellFormed() ? value : { u: [...Array(value.length).keys()].map(i => value.charCodeAt(i)) }

function write(set, cases) {
    const ids = new Set()
    for (const entry of cases) { if (ids.has(entry.id)) throw new Error(`duplicate ${set} ${entry.id}`); ids.add(entry.id) }
    const text = `{"schema": 1, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`
    if (text.includes('\r')) throw new Error('CR in cases')
    writeFileSync(join(dir, `${set}.cases.json`), text)
    console.log(set, cases.length)
}

// The tokenizerList names `encode` switches on, each with the artifact it fetches.
const named = ['mistral', 'llama', 'novelai', 'claude', 'novellist', 'llama3', 'gemma', 'cohere', 'deepseek', 'deepseek-v4', 'glm4', 'glm5']

// --- selection: which tokenizer `encode` runs ---
const probe = 'Hello, 세계! 🙂 tokens'
const selection = []
const select = (id, settings, extra = {}) => selection.push({ id, text: probe, settings, ...extra })
for (const name of named) select(`openrouter-${name}`, { aiModel: 'openrouter', customTokenizer: name })
for (const name of ['tik', 'o200k_base', 'cl100k_base', 'gpt2', 'Claude']) select(`openrouter-${name}`, { aiModel: 'openrouter', customTokenizer: name })
select('openrouter-number', { aiModel: 'openrouter', customTokenizer: 5 })
select('reverse-proxy-glm5', { aiModel: 'reverse_proxy', customTokenizer: 'glm5' })
select('reverse-proxy-tik', { aiModel: 'reverse_proxy', customTokenizer: 'tik' })
select('custom-no-provider', { aiModel: 'custom', customTokenizer: 'claude' })
select('custom-plugin-function', { aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: 'custom', reply: 'code-points' } })
select('custom-plugin-no-function', { aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: 'custom' } })
select('custom-plugin-returns-nothing', { aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: 'custom', reply: 'none' } })
select('custom-plugin-function-only', { aiModel: 'custom', customTokenizer: 'tik', plugin: { reply: 'code-points' } })
select('custom-plugin-null', { aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: null, reply: 'code-points' } })
for (const name of ['cl100k_base', 'o200k_base', 'tik', 'claude', 'deepseek-v4', 'none']) {
    select(`custom-plugin-${name}`, { aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: name, reply: 'code-points' } })
}
select('custom-plugin-empty', { aiModel: 'custom', customTokenizer: 'claude', plugin: { tokenizer: '', reply: 'code-points' } })
// Custom models carry any LLMTokenizer value, so they reach every fallback branch.
for (let tokenizer = 0; tokenizer <= 16; tokenizer++) {
    select(`model-${tokenizer}`, { aiModel: `xcustom:::selection-${tokenizer}`, customTokenizer: 'cohere', model: { tokenizer } })
}
select('model-string', { aiModel: 'xcustom:::selection-string', customTokenizer: 'tik', model: { tokenizer: '6' } })
select('model-missing', { aiModel: 'xcustom:::selection-missing', customTokenizer: 'tik', model: {} })
select('model-fraction', { aiModel: 'xcustom:::selection-fraction', customTokenizer: 'tik', model: { tokenizer: 6.5 } })
select('model-google-off', { aiModel: 'xcustom:::selection-google-off', customTokenizer: 'tik', googleClaudeTokenizing: false, model: { tokenizer: 10 } })
select('model-google-200', { aiModel: 'xcustom:::selection-google-200', customTokenizer: 'tik', googleClaudeTokenizing: true, model: { tokenizer: 10 } },
    { google: { status: 200, totalTokens: 7 } })
select('model-google-503', { aiModel: 'xcustom:::selection-google-503', customTokenizer: 'tik', googleClaudeTokenizing: true, model: { tokenizer: 10 } },
    { google: { status: 503 } })
select('model-google-no-total', { aiModel: 'xcustom:::selection-google-no-total', customTokenizer: 'tik', googleClaudeTokenizing: true, model: { tokenizer: 10 } },
    { google: { status: 200 } })
select('model-gemma-google-on', { aiModel: 'xcustom:::selection-gemma-google-on', customTokenizer: 'tik', googleClaudeTokenizing: true, model: { tokenizer: 9 } })
select('model-local-outside-hf', { aiModel: 'xcustom:::selection-local', customTokenizer: 'tik', model: { tokenizer: 12 } })
for (const aiModel of ['anthropic.claude-opus-4-6-v1', 'cohere-command-r', 'novelai', 'gpt4_turbo_20240409', 'synthetic-unlisted-model']) {
    select(`catalog-${aiModel}`, { aiModel, customTokenizer: 'claude' })
}
for (const [index, name] of ['deepseek', 'deepseek-v4', 'claude', 'deepseek-v4', 'deepseek'].entries()) {
    select(`hf-alternating-${index}-${name}`, { aiModel: 'openrouter', customTokenizer: name },
        { text: '<｜begin▁of▁repo▁name｜>cache<EOT><｜end▁of▁sentence｜>' })
}
for (const [index, name] of ['llama3', 'glm4', 'glm5', 'claude', 'glm5', 'glm4', 'llama3'].entries()) {
    select(`llama-glm-alternating-${index}-${name}`, { aiModel: 'openrouter', customTokenizer: name },
        { text: '[gMASK]<|begin_of_text|>cache<|assistant|><|eot_id|>' })
}
const llamaRouteProbe = 'Synthetic tokenizer route: Ελληνικά 한국어.'
select('llama3-model-fallback', { aiModel: 'xcustom:::llama3-model-fallback', customTokenizer: 'llama3', model: { tokenizer: 8 } }, { text: llamaRouteProbe })
select('llama3-named-route', { aiModel: 'openrouter', customTokenizer: 'llama3' }, { text: llamaRouteProbe })
for (const [index, name] of ['gemma', 'cohere', 'claude', 'gemma', 'cohere', 'gemma'].entries()) {
    select(`gemma-cohere-alternating-${index}-${name}`, { aiModel: 'openrouter', customTokenizer: name },
        { text: '<|turn>user é ▁<BOS_TOKEN><turn|>' })
}
for (const name of ['gemma', 'cohere']) {
    select(`custom-plugin-${name}`, { aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: name } })
    select(`reverse-proxy-${name}`, { aiModel: 'reverse_proxy', customTokenizer: name })
}
write('selection', selection)

// --- accounting: tokenize, tokenizeNum, ChatTokenizer, getChatToken, getCharToken, tokenizeAccurate ---
const image = (width, height) => ({ type: 'image', base64: 'data:,', ...(width === undefined ? {} : { width }), ...(height === undefined ? {} : { height }) })
const accounting = []
const account = (id, call, fields) => accounting.push({ id, call, tokenizer: 'cl100k_base', ...fields })
for (const tokenizer of ['cl100k_base', 'o200k_base']) {
    account(`${tokenizer}-tokenize`, 'tokenize', { tokenizer, text: 'Synthetic tokens, 한국어 텍스트와 🙂.' })
    account(`${tokenizer}-tokenize-num`, 'tokenizeNum', { tokenizer, text: 'Synthetic tokens, 한국어 텍스트와 🙂.' })
}
account('tokenize-lone-surrogate', 'tokenize', { text: str('a\ud800b') })
account('tokenize-special', 'tokenize', { text: 'before <|endoftext|> after' })
const named1 = { role: 'user', content: 'A synthetic message.', name: 'Speaker' }
for (const useName of ['name', 'noName']) {
    account(`chat-${useName}`, 'tokenizeChat', { additional: 3, useName, message: named1 })
}
account('chat-empty-name', 'tokenizeChat', { additional: 3, useName: 'name', message: { role: 'user', content: 'x', name: '' } })
account('chat-thoughts-ignored', 'tokenizeChat', { additional: 1, useName: 'name', message: { role: 'assistant', content: 'Answer.', thoughts: ['first thought', 'second'] } })
const pictured = { role: 'user', content: 'Look.', multimodals: [image(1024, 1024), image(2000, 1000), image(1000, 2000), image(), image(300, 700), image(768, 768)] }
account('chat-images-inlay-high', 'tokenizeChat', { additional: 2, useName: 'name', inlay: true, visionQuality: 'high', message: pictured })
account('chat-images-inlay-low', 'tokenizeChat', { additional: 2, useName: 'name', inlay: true, visionQuality: 'low', message: pictured })
account('chat-images-no-inlay', 'tokenizeChat', { additional: 2, useName: 'name', inlay: false, visionQuality: 'high', message: pictured })
account('chat-images-fractional', 'tokenizeChat', { additional: 0, useName: 'name', inlay: true, visionQuality: 'high',
    message: { role: 'user', content: '', multimodals: [image(512.5, 100.25), image(1e-9, 0)] } })
const conversation = [
    { role: 'system', content: 'System text.' },
    { role: 'user', content: '안녕하세요', name: 'User' },
    { role: 'assistant', content: 'Hi there! 🙂', name: 'Bot', multimodals: [image(640, 480)] },
    { role: 'user', content: '' },
]
for (const caching of [false, true]) {
    account(`chats-${caching ? 'cached' : 'uncached'}`, 'tokenizeChats', { additional: 4, useName: 'name', inlay: true, visionQuality: 'high', caching, messages: conversation })
}
account('chats-o200k', 'tokenizeChats', { tokenizer: 'o200k_base', additional: 4, useName: 'noName', inlay: true, visionQuality: 'low', messages: conversation })
account('chat-token', 'getChatToken', { chat: ['first message', '두 번째 메시지', '', 'last'] })
const character = {
    name: 'Ari$&',
    desc: '{{char}} is a synthetic character. <char> waves. {{CHAR}} stays.',
    personality: 'Calm; says $$ and $` and $\'.',
    scenario: 'A {{char}}{{char}} test.',
    globalLore: [
        { content: '@@depth 4\nFirst line about {{char}}\n\n@@@ triple\nlast line\n' },
        { content: '' },
        { content: '\r\nwindows line\r\n@@decorator' },
    ],
}
account('char-token', 'getCharToken', { character })
account('char-token-missing-fields', 'getCharToken', { character: { name: 'Nemo', desc: 'Only <char>.', globalLore: [] } })
account('char-token-dollar-patterns', 'getCharToken', { character: { name: "[$`|$'|$$|$1|$<x>|$]", desc: 'L {{char}} M <char> R', globalLore: [] } })
for (const [id, text] of [['plain', 'No slot here.'], ['slot', 'Before {{slot}} after.'], ['slot-start', '{{slot}}text'],
    ['slot-only', '{{slot}}'], ['slot-upper', 'Upper {SLOT} braces']]) {
    account(`accurate-${id}`, 'tokenizeAccurate', { text, consistantChar: id === 'slot' })
}
// Plugin tokenizers: the synthetic tokenizerFunc answers by the named reply rule, and the oracle records every request.
const plugin = { tokenizer: 'plugin', reply: 'code-points' }
account('plugin-tokenize', 'tokenize', { ...plugin, text: 'plugin text' })
account('plugin-tokenize-num', 'tokenizeNum', { ...plugin, reply: 'half-length', text: 'odd' })
account('plugin-chats-uncached', 'tokenizeChats', { ...plugin, additional: 2, useName: 'name', caching: false, inlay: true, visionQuality: 'high',
    messages: [{ role: 'user', content: 'same', name: 'same' }, { role: 'assistant', content: 'same' }, { role: 'user', content: 'other', name: 'n' }] })
account('plugin-chats-cached', 'tokenizeChats', { ...plugin, additional: 2, useName: 'name', caching: true,
    messages: [{ role: 'user', content: 'same', name: 'same' }, { role: 'assistant', content: 'same' }, { role: 'user', content: 'other', name: 'n' }] })
account('plugin-returns-nothing', 'tokenizeChat', { ...plugin, reply: 'none', additional: 1, useName: 'name', message: named1 })
account('plugin-returns-empty', 'tokenizeChat', { ...plugin, reply: 'empty', additional: 1, useName: 'name', message: named1 })
account('plugin-char-token', 'getCharToken', { ...plugin, character })
account('plugin-chat-token-cached', 'getChatToken', { ...plugin, caching: true, chat: ['repeat', 'repeat', 'other'] })
account('plugin-missing-chats', 'tokenizeChats', { tokenizer: 'plugin_missing', additional: 1, useName: 'name', messages: conversation })
account('plugin-missing-num', 'tokenizeNum', { tokenizer: 'plugin_missing', text: 'anything' })
account('claude-chat', 'tokenizeChat', { tokenizer: 'claude', additional: 4, useName: 'name', inlay: true, visionQuality: 'high',
    message: { role: 'user', content: '<SOS>Ｆｕｌｌ ﬁ é 안녕🙂', name: '<META>', multimodals: [image(640, 480)] } })
account('claude-chats', 'tokenizeChats', { tokenizer: 'claude', additional: 3, useName: 'noName', messages: conversation })
account('claude-ids', 'tokenizeNum', { tokenizer: 'claude', text: str('a\ud800B<EOT>') })
for (const tokenizer of ['deepseek', 'deepseek-v4']) {
    account(`${tokenizer}-chat`, 'tokenizeChat', { tokenizer, additional: 4, useName: 'name', inlay: true, visionQuality: 'high',
        message: { role: 'user', content: '<｜begin▁of▁repo▁name｜>１２３ é 中文🙂', name: '<｜end▁of▁sentence｜>', multimodals: [image(640, 480)] } })
    account(`${tokenizer}-chats`, 'tokenizeChats', { tokenizer, additional: 3, useName: 'noName', caching: true, messages: conversation })
    account(`${tokenizer}-ids`, 'tokenizeNum', { tokenizer, text: str('a\ud800B<｜end▁of▁sentence｜>') })
}
for (const tokenizer of ['llama3', 'glm4', 'glm5']) {
    account(`${tokenizer}-chat`, 'tokenizeChat', { tokenizer, additional: 4, useName: 'name', inlay: true, visionQuality: 'high',
        message: { role: 'user', content: '[gMASK]<|begin_of_text|>１２３ é 中文🙂', name: '<|assistant|>', multimodals: [image(640, 480)] } })
    account(`${tokenizer}-chats`, 'tokenizeChats', { tokenizer, additional: 3, useName: 'noName', caching: true, messages: conversation })
    account(`${tokenizer}-ids`, 'tokenizeNum', { tokenizer, text: str('a\ud800B<|begin_of_text|>[gMASK]') })
}
for (const tokenizer of ['gemma', 'cohere']) {
    account(`${tokenizer}-chat`, 'tokenizeChat', { tokenizer, additional: 4, useName: 'name', inlay: true, visionQuality: 'high',
        message: { role: 'user', content: '<|turn>１２３ é 中文🙂', name: '<BOS_TOKEN>', multimodals: [image(640, 480)] } })
    account(`${tokenizer}-chats`, 'tokenizeChats', { tokenizer, additional: 3, useName: 'noName', caching: true, messages: conversation })
    account(`${tokenizer}-ids`, 'tokenizeNum', { tokenizer, text: str('a\ud800B<|turn><BOS_TOKEN>') })
}
write('accounting', accounting)

// --- strong-ban: strongBan(data, bias) and its localStorage cache ---
const bans = []
const ban = (id, data, fields = {}) => bans.push({ id, tokenizer: 'cl100k_base', data: str(data), bias: '{}', ...fields })
ban('hello', 'Hello')
ban('o200k-spaced', ' Hello world ', { tokenizer: 'o200k_base' })
ban('existing-bias', 'test', { bias: '{"100":5,"-1":3,"abc":1,"13":-100}' })
ban('sharp-s', 'straße')
ban('final-sigma', 'ΟΔΟΣ')
ban('titlecase', 'ǅungla')
ban('astral-first', '🙂smile')
ban('lone-surrogate', 'a\ud800b')
ban('cache-hit', 'cached', { cache: '{"5":-100,"x":1}' })
ban('cache-empty', 'cached', { cache: '' })
ban('cache-invalid', 'cached', { cache: 'not json' })
ban('cache-number', 'cached', { cache: '7' })
ban('empty-data', '')
ban('empty-data-cached', '', { cache: '{"1":-100}' })
ban('special-token', '<|endoftext|>')
ban('plugin-code-points', 'Hi', { tokenizer: 'plugin', reply: 'code-points' })
ban('plugin-half-length-cached', 'Ab ', { tokenizer: 'plugin', reply: 'half-length', caching: true })
ban('plugin-empty', 'Hi', { tokenizer: 'plugin', reply: 'empty', bias: '{"9":1}' })
ban('plugin-missing', 'Hi', { tokenizer: 'plugin_missing', bias: '{"9":1}' })
ban('claude-special', '<EOT>', { tokenizer: 'claude' })
ban('claude-normalization', 'Ｆｕｌｌ ﬁ', { tokenizer: 'claude' })
ban('claude-surrogate', 'a\ud800B', { tokenizer: 'claude' })
for (const tokenizer of ['deepseek', 'deepseek-v4']) {
    ban(`${tokenizer}-special`, '<｜begin▁of▁repo▁name｜>', { tokenizer })
    ban(`${tokenizer}-unicode`, 'Straße 中文 é', { tokenizer, bias: '{"9":1}' })
    ban(`${tokenizer}-surrogate`, 'a\ud800B', { tokenizer })
}
for (const tokenizer of ['llama3', 'glm4', 'glm5']) {
    ban(`${tokenizer}-special`, '[gMASK]<|begin_of_text|>', { tokenizer })
    ban(`${tokenizer}-unicode`, 'Straße 中文 é', { tokenizer, bias: '{"9":1}' })
    ban(`${tokenizer}-surrogate`, 'a\ud800B', { tokenizer })
}
for (const tokenizer of ['gemma', 'cohere']) {
    ban(`${tokenizer}-special`, '<|turn><BOS_TOKEN>', { tokenizer })
    ban(`${tokenizer}-unicode`, 'Straße 中文 é', { tokenizer, bias: '{"9":1}' })
    ban(`${tokenizer}-surrogate`, 'a\ud800B', { tokenizer })
}
write('strong-ban', bans)

// --- encode: the checked-in tiktoken corpus through encodeWithTokenizer ---
const corpus = JSON.parse(readFileSync(new URL('../tokenizer/native-tokenizer-corpus.json', import.meta.url), 'utf8'))
const encode = []
const listName = { cl100k_base: 'tik', o200k_base: 'o200k_base' }
for (const entry of corpus.cases) {
    const input = entry.input.kind === 'text' ? str(entry.input.value)
        : entry.input.kind === 'utf16-code-units' ? { u: entry.input.codeUnits }
            : { repeat: str(entry.input.value), count: entry.input.count }
    encode.push({ id: entry.name, tokenizer: listName[entry.tokenizerId], input })
}
encode.push({ id: 'unlisted-name', tokenizer: 'gpt2', input: 'Unlisted names encode with cl100k.' })
encode.push({ id: 'case-variant-name', tokenizer: 'Claude', input: probe })
// Named tokenizers load their artifact; the oracle withholds those still declined by the native encoder.
for (const name of named) encode.push({ id: `named-${name}`, tokenizer: name, input: probe })
for (const [index, tokenizer] of ['deepseek', 'deepseek-v4', 'claude', 'deepseek-v4', 'deepseek'].entries()) {
    encode.push({ id: `hf-alternating-${index}-${tokenizer}`, tokenizer, input: '<｜begin▁of▁repo▁name｜>cache<EOT><｜end▁of▁sentence｜>' })
}
for (const [index, tokenizer] of ['llama3', 'glm4', 'glm5', 'claude', 'glm5', 'glm4', 'llama3'].entries()) {
    encode.push({ id: `llama-glm-alternating-${index}-${tokenizer}`, tokenizer, input: '[gMASK]<|begin_of_text|>cache<|assistant|><|eot_id|>' })
}
for (const [index, tokenizer] of ['gemma', 'cohere', 'claude', 'gemma', 'cohere', 'gemma'].entries()) {
    encode.push({ id: `gemma-cohere-alternating-${index}-${tokenizer}`, tokenizer, input: '<|turn>user é ▁<BOS_TOKEN><turn|>' })
}
write('encode', encode)

// --- web-encode: every web tokenizer over its real artifact, through encodeWithTokenizer ---
// The distinct corpus inputs, then inputs that pin special tokens, normalization, byte fallback and whitespace handling.
const webInputs = []
const corpusInputs = new Set()
for (const entry of corpus.cases) {
    const input = entry.input.kind === 'text' ? str(entry.input.value)
        : entry.input.kind === 'utf16-code-units' ? { u: entry.input.codeUnits }
            : { repeat: str(entry.input.value), count: entry.input.count }
    const key = JSON.stringify(input)
    if (corpusInputs.has(key)) continue
    corpusInputs.add(key)
    webInputs.push([entry.name.slice(entry.name.indexOf(':') + 1), input])
}
for (const [name, input] of Object.entries({
    'special-bos-eos-unk': '<s> </s> <unk> <pad> <mask>',
    'special-sentence-boundaries': '<s>Hello</s>',
    'special-llama3': '<|begin_of_text|><|start_header_id|>user<|end_header_id|>\n\nHi<|eot_id|>',
    'special-chatml': '<|im_start|>user\nHi<|im_end|>',
    'special-mistral-inst': '[INST] Hi [/INST]',
    'special-gemma-turn': '<start_of_turn>user\nHi<end_of_turn>',
    'special-cohere-turn': '<|START_OF_TURN_TOKEN|><|USER_TOKEN|>Hi<|END_OF_TURN_TOKEN|>',
    'special-deepseek': '<｜begin▁of▁sentence｜>Hi<｜end▁of▁sentence｜>',
    'special-glm': '[gMASK]<sop><|user|>\nHi<|assistant|>',
    'whitespace-only': '   ',
    'leading-space': ' leading',
    'trailing-space': 'trailing ',
    'inner-space-runs': 'a  b   c    d',
    'tabs-and-newlines': '\t\tindent\n\n\nblank lines\r\r',
    'metaspace-literal': '▁already ▁marked',
    'digits-and-dates': '2026-10-26 3.14159 1,000,000',
    'nfkc-compatibility': 'ﬁ ① Ｆｕｌｌ ｗｉｄｔｈ ㌔ ℃ Ⅻ ½',
    'byte-fallback': '𓀀 ᚠᛇᚻ 𠀀 \u{10fffd}',
    'repeated-letters': 'a'.repeat(40),
    'repeated-punctuation': '!'.repeat(25) + '?'.repeat(25),
    'case-mapping': 'ǅungla ΟΔΟΣ straße İstanbul',
    'combining-marks': 'é ä 각',
    'zero-width': 'a​b‌c‍d﻿e',
    'long-word': 'Pneumonoultramicroscopicsilicovolcanoconiosis',
})) webInputs.push([name, str(input)])
// tokenizerList order without tik, so each tokenizer loads its artifact once, at its first case.
const webTokenizers = ['mistral', 'novelai', 'claude', 'llama', 'llama3', 'novellist', 'gemma', 'cohere', 'deepseek', 'deepseek-v4', 'glm4', 'glm5']
// Unigram inputs where the novellist vocabulary scores two segmentations equally, so the first one Viterbi reaches
// stays, and inputs where comparing a double-precision candidate with the single-precision best changes the result.
const viterbi = {
    'viterbi-tie-exclamation': '!'.repeat(8), 'viterbi-tie-plus': '+++', 'viterbi-tie-period': '.'.repeat(7),
    'viterbi-tie-em-dash': '—'.repeat(5), 'viterbi-tie-ellipsis': '⋯'.repeat(3),
    'viterbi-precision-exclamation': '!'.repeat(7), 'viterbi-precision-quote': '"'.repeat(3), 'viterbi-precision-hyphen': '-'.repeat(6),
    'viterbi-precision-hiragana': 'あ'.repeat(11), 'viterbi-precision-heart': '♥'.repeat(3),
}
const claudeInputs = {
    'special-reserved': '<EOT><META><META_START><META_END><SOS>',
    'special-adjacent': 'left<EOT>right<META_START>slot<META_END>',
    'special-normalized': '＜ＥＯＴ＞ <eot> <EOT >',
    'nul-prefix': '\0hello',
    'nul-inner': 'before\0after',
    'nul-suffix': 'hello\0',
    'surrogate-high-before-nul': '\ud800\0x',
    'surrogate-low-before-ascii': '\udc00AB',
    'surrogate-two-high': '\ud800\ud800x',
    'surrogate-two-low': '\udfff\udfff',
    'nfkc-expansion': 'ﷺ ㍿ ﬃ ㎏ Å 가',
    'bytelevel-contractions': "I'm we're I'M WE'RE isn't ISN'T\n\tend  ",
}
// Replay both added-token inventories on each family, including spellings absent from the older model.
const deepseekAdded = [...new Set(['../../public/token/deepseek/tokenizer.json', '../../public/token/deepseek/v4/tokenizer.json']
    .flatMap(path => JSON.parse(readFileSync(new URL(path, import.meta.url), 'utf8')).added_tokens.map(token => token.content)))]
const deepseekInputs = {
    'digit-groups': '1 12 123 1234 12345 123456 1234567 000123456789',
    'cjk-kana-boundaries': '中文abcかなカナ123漢字かなABC',
    'combining-boundaries': 'éä각 !Latin?中文',
    'space-newline-boundaries': '  alpha\t\tbeta\n\n中文\r\nend  ',
    'special-adjacent': '<｜begin▁of▁repo▁name｜><｜end▁of▁repo▁name｜><｜end▁of▁sentence｜>',
    'special-prefix-near-miss': '<｜begin▁of▁repo <｜begin▁of▁repo▁name｜x <｜begin▁of▁repo▁name｜>',
    'special-overlap': '<｜begin▁of▁file｜><｜begin▁of▁file▁name｜>x<｜end▁of▁file▁name｜><｜end▁of▁file｜>',
    'nul-prefix': '\0hello', 'nul-inner': 'before\0after', 'nul-suffix': 'hello\0',
    'surrogate-high-before-nul': '\ud800\0x', 'surrogate-low-before-ascii': '\udc00AB',
    'surrogate-two-high': '\ud800\ud800x', 'surrogate-two-low': '\udfff\udfff',
}
const deepseekCases = ['deepseek', 'deepseek-v4'].flatMap(tokenizer => [
    ...deepseekAdded.map((input, index) => ({ id: `${tokenizer}:added-${index}`, tokenizer, input })),
    ...Object.entries(deepseekInputs).map(([name, input]) => ({ id: `${tokenizer}:${name}`, tokenizer, input: str(input) })),
])
const llamaGlmAdded = [...new Set(['../../public/token/llama/llama3.json', '../../public/token/glm4/tokenizer.json', '../../public/token/glm5/tokenizer.json']
    .flatMap(path => JSON.parse(readFileSync(new URL(path, import.meta.url), 'utf8')).added_tokens.map(token => token.content)))].filter(input => !webInputs.some(([, known]) => input === known))
const llamaGlmInputs = {
    'postprocessor-space': ' prompt ',
    'special-empty-turn': '<|begin_of_text|><|end_of_text|>',
    'llama-header': '<|start_header_id|>assistant<|end_header_id|>\n\nAnswer<|eot_id|>',
    'glm-role': '[gMASK]<sop><|assistant|>\nAnswer<eop>',
    'special-prefix-adjacent': '[gMASK][gMAS]<|start_header_id|x<|start_header_id|>',
    'digit-groups': '1 12 123 1234 12345 123456 1234567 000123456789',
    'contractions': "I'm I'M we're WE'RE isn't ISN'T\nEnd",
    'cjk-kana-boundaries': '中文abcかなカナ123漢字かなABC',
    'combining-boundaries': 'éä각 !Latin?中文',
    'punctuation-latin': '!Hello?WORLD...Mixed_case Γειά',
    'space-newline-boundaries': '  alpha\t\tbeta\n\n中文\r\nend  ',
    'nul-after-llama': '<|begin_of_text|>\0[gMASK]',
    'nul-after-glm': '[gMASK]\0<|begin_of_text|>',
    'surrogate-high-before-nul': '\ud800\0<|begin_of_text|>',
    'surrogate-low-before-ascii': '\udc00AB[gMASK]',
    'surrogate-runs': '\ud800\ud800x\udfff\udfff<|begin_of_text|>',
    // Complete vocabulary words whose ranked merges can select a different segmentation.
    'ignore-merges-llama': 'mektedir lardan iyordu ecektir',
    'ignore-merges-glm': 'irsiniz kohol fahrt ramientas',
}
const llamaGlmCases = ['llama3', 'glm4', 'glm5'].flatMap(tokenizer => [
    ...llamaGlmAdded.map((input, index) => ({ id: `${tokenizer}:added-${index}`, tokenizer, input })),
    ...Object.entries(llamaGlmInputs).map(([name, input]) => ({ id: `${tokenizer}:${name}`, tokenizer, input: str(input) })),
])
const gemmaReference = JSON.parse(readFileSync(new URL('../../tests/fixtures/tokenizer/gemma4.json', import.meta.url), 'utf8'))
const gemmaCohereInputs = {
    'normalization-distinction': 'é café 가 가 Å Å ﬁ Ｆｕｌｌ',
    'spaces-and-metaspace': '  space ▁space▁  ',
    'tabs-and-line-breaks': '\tA  \n\nB\r\n\t',
    'individual-digit-groups': '0123456789 123456 2026-10-27 1,234.567',
    'bytelevel-contractions': "I'm I'M we're WE'RE isn't ISN'T\nEnd",
    'plain-no-bos': 'No automatic BOS here.',
    'mixed-adjacent-specials': '<|turn><|channel>thought<channel|><turn|><|START_OF_TURN_TOKEN|><|USER_TOKEN|>Hi<|END_OF_TURN_TOKEN|>',
    'prefix-and-case-near-misses': '<|turn <|TURN> ＜｜turn＞ <BOS_TOKEN <bos_token> <|START_OF_TURN_TOKEN|x',
    'modern-and-legacy-turns': '<|turn>assistant<turn|><start_of_turn>assistant<end_of_turn>',
    'adjacent-uncommon-scalars': 'x\u{10ffff}\u{10fffd}𓀀y',
    'nul-after-special': '<|turn>\0<BOS_TOKEN>',
    'surrogate-high-before-nul': '\ud800\0<|turn>',
    'surrogate-low-before-ascii': '\udc00AB<BOS_TOKEN>',
    'surrogate-runs': '\ud800\ud800x\udfff\udfff<|turn>',
}
const gemmaCohereCases = [
    ...['gemma', 'cohere'].flatMap(tokenizer => {
        const file = tokenizer === 'gemma' ? '../../public/token/gemma/gemma4.json' : '../../public/token/cohere/tokenizer.json'
        const added = JSON.parse(readFileSync(new URL(file, import.meta.url), 'utf8')).added_tokens
        return [
            ...added.filter(token => !webInputs.some(([, input]) => input === token.content))
                .map(token => ({ id: `${tokenizer}:added-${token.id}`, tokenizer, input: token.content })),
            ...Object.entries(gemmaCohereInputs).map(([name, input]) => ({ id: `${tokenizer}:${name}`, tokenizer, input: str(input) })),
        ]
    }),
    ...gemmaReference.cases.filter(entry => !webInputs.some(([, input]) => input === entry.text))
        .map(entry => ({ id: `gemma:reference-${entry.name}`, tokenizer: 'gemma', input: str(entry.text) })),
    ...['gemma', 'cohere', 'claude', 'gemma', 'cohere', 'gemma'].map((tokenizer, index) => ({
        id: `gemma-cohere-alternating-${index}-${tokenizer}`, tokenizer, input: '<|turn>user é ▁<BOS_TOKEN><turn|>',
    })),
]
// Code points by the Unicode version that assigned them, in contexts where a pre-tokenizer pattern splits by category.
// The web-tokenizers regex tables stop at Unicode 14.0.
const symbolIn = (x) => `I love ${x} you ${x}${x}!${x}`
const letterIn = (x) => `a${x}b ${x}c t${x}${x}ab ${x}123 ${x}`
const markIn = (base, x) => `${base}${x} a${x}b ${x}`
const numberIn = (x) => `${x}123 1${x}23 a${x}b ${x}${x}`
const unicodeInputs = {
    'unicode-14.0-symbol': symbolIn('\u{1fae0}'), 'unicode-14.0-letter': letterIn('\u{870}'),
    'unicode-14.0-mark': markIn('\u{628}', '\u{898}'), 'unicode-14.0-number': numberIn('\u{16ac0}'),
    'unicode-15.0-symbol': symbolIn('\u{1fa77}'), 'unicode-15.0-letter': letterIn('\u{11f04}'),
    'unicode-15.0-mark': markIn('\u{c95}', '\u{cf3}'), 'unicode-15.0-number': numberIn('\u{11f50}'),
    // Cohere's IDs for this text change once U+11F31 counts as a letter.
    'unicode-15.0-letter-merged': ' \u{11f31}c',
    'unicode-15.1-symbol': symbolIn('\u{2ffc}'), 'unicode-15.1-letter': letterIn('\u{2ebf0}'),
    'unicode-16.0-symbol': symbolIn('\u{2427}'), 'unicode-16.0-letter': letterIn('\u{1c89}'),
    'unicode-16.0-mark': markIn('\u{628}', '\u{897}'), 'unicode-16.0-number': numberIn('\u{1ccf0}'),
    // Rust's char::is_numeric, which the Digits pre-tokenizer calls, follows the toolchain's Unicode version.
    'unicode-17.0-number': numberIn('\u{11de0}'),
}
const unicodeCases = ['claude', 'llama3', 'gemma', 'cohere', 'deepseek', 'deepseek-v4', 'glm4', 'glm5'].flatMap(tokenizer =>
    Object.entries(unicodeInputs).map(([name, input]) => ({ id: `${tokenizer}:${name}`, tokenizer, input })))
// A whitespace run and a run of a format character (U+200D, two per DeepSeek token) at the longest the native encoder
// takes (crates/core/tokenizer/src/unicode.rs), and one code point longer. Then, for each pattern, a run of one class
// it matches whole at the longest the native encoder takes (crates/core/tokenizer/src/id/huggingface.rs).
const limitCases = [
    ...[['whitespace', ' ', 100000], ['other', '‍', 10000]].flatMap(([kind, repeat, count]) => [
        { id: `deepseek:${kind}-run-at-limit`, tokenizer: 'deepseek', input: { repeat, count } },
        { id: `deepseek:${kind}-run-over-limit`, tokenizer: 'deepseek', input: { repeat, count: count + 1 } },
    ]),
    ...[['claude', 'letters', 'a'], ['cohere', 'letters', 'a'], ['llama3', 'letters', 'a'], ['deepseek', 'punctuation', '!']]
        .map(([tokenizer, kind, repeat]) => ({ id: `${tokenizer}:${kind}-run-at-limit`, tokenizer, input: { repeat, count: 100000 } })),
]
write('web-encode', [
    ...webTokenizers.flatMap((tokenizer) => webInputs.map(([name, input]) => ({ id: `${tokenizer}:${name}`, tokenizer, input }))),
    ...deepseekCases,
    ...gemmaCohereCases,
    ...llamaGlmCases,
    ...['llama3', 'glm4', 'glm5', 'claude', 'glm5', 'glm4', 'llama3'].map((tokenizer, index) => ({
        id: `llama-glm-alternating-${index}-${tokenizer}`, tokenizer, input: '[gMASK]<|begin_of_text|>cache<|assistant|><|eot_id|>',
    })),
    ...['deepseek', 'deepseek-v4', 'claude', 'deepseek-v4', 'deepseek'].map((tokenizer, index) => ({
        id: `hf-alternating-${index}-${tokenizer}`, tokenizer, input: '<｜begin▁of▁repo▁name｜>cache<EOT><｜end▁of▁sentence｜>',
    })),
    ...Object.entries(claudeInputs).map(([name, input]) => ({ id: `claude:${name}`, tokenizer: 'claude', input: str(input) })),
    ...Object.entries(viterbi).map(([name, input]) => ({ id: `novellist:${name}`, tokenizer: 'novellist', input })),
    ...unicodeCases,
    ...limitCases,
])

// --- google-cloud: tokenizeGoogleCloud's countTokens requests, how it reads each answer, and its memo ---
// Each case runs its steps in order against one TS session, so the memo carries from step to step. A step names the
// model (an index into `models`, the internalIDs) and the call; the API answers each request with the next entry of
// `responses`. Every case has its own internalIDs, none a suffix of another case's, so cases never share memo keys.
const googleCloud = []
const gcText = 'Count these synthetic words.'
const answer = (body, status = 200) => ({ status, body })
const gcCase = (id, steps, responses, extra = {}) => googleCloud.push({
    id, models: [`gc-${id}`], token: 'synthetic-google-key', steps, responses, ...extra,
})
const gcEncode = (text = gcText, fields = {}) => ({ call: 'encode', model: 0, text: str(text), ...fields })
for (const [id, body] of Object.entries({
    'total-number': '{"totalTokens":6}', 'total-fraction': '{"totalTokens":5.5}', 'total-negative-fraction': '{"totalTokens":-0.5}',
    'total-negative': '{"totalTokens":-1}', 'total-overflow': '{"totalTokens":1e400}', 'total-zero': '{"totalTokens":0}',
    'total-string': '{"totalTokens":"5"}', 'total-string-spaced': '{"totalTokens":" 7 "}', 'total-string-hex': '{"totalTokens":"0x10"}',
    'total-string-word': '{"totalTokens":"many"}', 'total-true': '{"totalTokens":true}', 'total-null': '{"totalTokens":null}',
    'total-missing': '{}', 'total-duplicate-key': '{"totalTokens":1,"totalTokens":4}',
    'total-with-details': '{"totalTokens":9,"totalBillableCharacters":30,"promptTokensDetails":[{"modality":"TEXT","tokenCount":9}]}',
    'body-array': '[1,2]', 'body-number': '12', 'body-string': '"12"', 'body-null': 'null', 'body-not-json': 'not json', 'body-empty': '',
})) gcCase(id, [gcEncode()], [answer(body)])
gcCase('body-bom', [gcEncode()], [{ status: 200, bytes: [0xef, 0xbb, 0xbf, ...Buffer.from('{"totalTokens":3}')] }])
gcCase('body-invalid-utf8', [gcEncode()], [{ status: 200, bytes: [...Buffer.from('{"totalTokens":2,"note":"'), 0xff, ...Buffer.from('"}')] }])
// TS copies an array-like totalTokens into the typed array; the native stage declines it.
gcCase('total-list', [gcEncode(gcText, { decline: 'google-cloud-token-list' })], [answer('{"totalTokens":[1,2]}')])
gcCase('status-404', [gcEncode()], [answer('{"error":{"code":404}}', 404)])
gcCase('status-201', [gcEncode()], [answer('{"totalTokens":6}', 201)])
gcCase('fetch-throws', [gcEncode()], [{ throws: 'synthetic network failure' }])
gcCase('token-missing', [gcEncode()], [answer('{"totalTokens":2}')], { token: null })
gcCase('text-escapes', [gcEncode('quote " backslash \\ tab \t newline \n bell \u0007 separator   emoji 🙂 é')], [answer('{"totalTokens":11}')])
gcCase('text-isolated-surrogate', [gcEncode('lone \ud800 high, lone \udc00 low')], [answer('{"totalTokens":4}')])
gcCase('text-empty', [gcEncode('')], [answer('{"totalTokens":0}')])
gcCase('model-id-characters', [gcEncode()], [answer('{"totalTokens":3}')], { models: ['gc-model-id-characters models/é ?x=1#y'] })
gcCase('chat-content-and-name', [{ call: 'chat', model: 0, text: str(gcText), name: 'Speaker' }],
    [answer('{"totalTokens":6}'), answer('{"totalTokens":2}')])
gcCase('memo-repeat', [gcEncode(), gcEncode()], [answer('{"totalTokens":6}')])
gcCase('memo-chat-then-encode', [{ call: 'chat', model: 0, text: str(gcText), name: 'Speaker' }, gcEncode('Speaker'), gcEncode()],
    [answer('{"totalTokens":6}'), answer('{"totalTokens":2}')])
gcCase('memo-encode-then-chat', [gcEncode(), { call: 'chat', model: 0, text: str(gcText), name: 'Speaker' }],
    [answer('{"totalTokens":6}'), answer('{"totalTokens":2}')])
gcCase('memo-missing-total', [gcEncode(), gcEncode()], [answer('{}')])
gcCase('memo-not-after-status', [gcEncode(), gcEncode()], [answer('{}', 503), answer('{"totalTokens":6}')])
gcCase('memo-not-after-syntax', [gcEncode(), gcEncode()], [answer('not json'), answer('{"totalTokens":6}')])
gcCase('memo-not-after-throw', [gcEncode(), gcEncode()], [{ throws: 'synthetic network failure' }, answer('{"totalTokens":6}')])
gcCase('memo-invalid-length', [gcEncode(), gcEncode()], [answer('{"totalTokens":-1}')])
gcCase('memo-per-model', [gcEncode(), { ...gcEncode(), model: 1 }], [answer('{"totalTokens":6}'), answer('{"totalTokens":8}')],
    { models: ['gc-memo-per-model-a', 'gc-memo-per-model-b'] })
// TS keys the memo by `text + internalID`, so a text ending in the start of another model's internalID shares its entry.
gcCase('memo-key-concatenation', [gcEncode('helloX'), { ...gcEncode('hello'), model: 1 }], [answer('{"totalTokens":4}')],
    { models: ['gc-memo-key-concatenation', 'Xgc-memo-key-concatenation'] })
const fallbackText = '<|turn>user\nFallback<turn|>'
gcCase('fallback-bad-json', [gcEncode(fallbackText)], [answer('not JSON', 503)])
gcCase('fallback-invalid-utf8', [gcEncode(fallbackText)], [{ status: 503, bytes: [0xff, 0xfe, 0] }])
gcCase('fallback-repeat-not-cached', [gcEncode(fallbackText), gcEncode(fallbackText)], [answer('bad', 503), answer('bad again', 500)])
gcCase('fallback-then-api-success-cache', [gcEncode(fallbackText), gcEncode(fallbackText), gcEncode(fallbackText)],
    [answer('bad', 503), answer('{"totalTokens":4}')])
gcCase('chat-content-fallback-name-api', [{ call: 'chat', model: 0, text: fallbackText, name: 'API name', additional: 3 }],
    [answer('bad', 503), answer('{"totalTokens":5}')])
gcCase('chat-content-api-name-fallback', [{ call: 'chat', model: 0, text: fallbackText, name: '<|channel>name', additional: 3 }],
    [answer('{"totalTokens":7}'), answer('bad', 503)])
gcCase('chat-same-content-name-fallback', [{ call: 'chat', model: 0, text: fallbackText, name: fallbackText, additional: 2 }],
    [answer('bad', 503), answer('bad again', 500)])
gcCase('chat-same-content-name-api-cache', [{ call: 'chat', model: 0, text: fallbackText, name: fallbackText, additional: 3 }],
    [answer('{"totalTokens":6}')])
gcCase('fallback-after-fetch-error', [gcEncode(fallbackText), gcEncode(fallbackText), gcEncode(fallbackText)],
    [{ throws: 'synthetic fallback retry failure' }, answer('bad', 503), answer('{"totalTokens":2}')])
gcCase('fallback-after-malformed-200', [gcEncode(fallbackText), gcEncode(fallbackText), gcEncode(fallbackText)],
    [answer('not JSON'), answer('bad', 503), answer('{"totalTokens":8}')])
gcCase('fallback-original-utf16', [gcEncode('a\ud800B\0after')], [answer('bad', 503)])
gcCase('chat-fallback-cross-call-memo', [
    { call: 'chat', model: 0, text: fallbackText, name: 'cached name', additional: 4 }, gcEncode(fallbackText),
    { call: 'chat', model: 0, text: fallbackText, name: 'cached name', additional: 4 },
], [answer('bad', 503), answer('{"totalTokens":5}'), answer('{"totalTokens":2}')])
write('google-cloud', googleCloud)

// --- artifacts: the bytes behind every tokenizer (a contract; no product code runs) ---
const artifacts = [['cl100k_base', 'node_modules/@dqbd/tiktoken/encoders/cl100k_base.json'], ['o200k_base', 'src/etc/o200k_base.json'],
    ['mistral', 'public/token/mistral/tokenizer.model'], ['llama', 'public/token/llama/llama.model'], ['novelai', 'public/token/nai/nerdstash_v2.model'],
    ['claude', 'public/token/claude/claude.json'], ['novellist', 'public/token/trin/spiece.model'], ['llama3', 'public/token/llama/llama3.json'],
    ['gemma', 'public/token/gemma/gemma4.json'], ['cohere', 'public/token/cohere/tokenizer.json'], ['deepseek', 'public/token/deepseek/tokenizer.json'],
    ['deepseek-v4', 'public/token/deepseek/v4/tokenizer.json'], ['glm4', 'public/token/glm4/tokenizer.json'], ['glm5', 'public/token/glm5/tokenizer.json']]
write('artifacts', artifacts.map(([tokenizer, file]) => ({ id: tokenizer, tokenizer, file })))
