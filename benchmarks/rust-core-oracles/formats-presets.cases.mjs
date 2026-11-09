// Writes the synthetic case file of the preset mapping oracle (tests/fixtures/rust-core/formats/presets.cases.json). Add
// cases here, run `node benchmarks/rust-core-oracles/formats-presets.cases.mjs`, then regenerate the expected file
// with RUST_CORE_ORACLE_WRITE=1.
//
// `constants` records the preset template and the NAI preset the import builds on. `import` imports `data` as a
// `.json` preset file through `importPreset`, `export` downloads the stored `preset` through `downloadPreset` as JSON,
// `chain` downloads the stored `preset` (as JSON or as a `.risup` file, by `via`) and imports the file it wrote, and
// `risup` imports a `.risup` or `.risupreset` file (by `extension`) whose msgpack container is `container`, with each
// `{"$encrypted": body}` value written as the encrypted msgpack of `body`.
// `decline` names the `Unsupported` the Rust port must answer instead of a result.
import { mkdirSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/formats/', import.meta.url))
mkdirSync(dir, { recursive: true })

const cases = []
const ids = new Set()
function add(entry) {
    if (ids.has(entry.id)) throw new Error(`duplicate ${entry.id}`)
    ids.add(entry.id)
    cases.push(entry)
}
const importCase = (id, data, extra = {}) => add({ id, kind: 'import', data, ...extra })
const exportCase = (id, preset, extra = {}) => add({ id, kind: 'export', preset, ...extra })
const chain = (id, via, preset) => add({ id, kind: 'chain', via, preset })
const risup = (id, container, extra = {}) => add({ id, kind: 'risup', extension: 'risup', container, ...extra })
const encrypted = body => ({ $encrypted: body })
// An object with an own `__proto__` key, which only JSON.parse creates.
const protoObject = JSON.parse('{"__proto__": {"polluted": true}, "plain": 1}')

add({ id: 'constants', kind: 'constants' })

const promptTemplate = [
    { type: 'plain', type2: 'main', text: 'main', role: 'assistant' },
    { type: 'jailbreak', type2: 'normal', text: 'jb' },
    { type: 'cot', type2: 'normal', text: 'cot', role: 'char' },
    { type: 'plain', type2: 'normal', text: 'user', role: 'user' },
    { type: 'plain', type2: 'normal', text: 'odd', role: 'narrator' },
    { type: 'persona', innerFormat: '{{slot}}', role2: 'assistant' },
    { type: 'description', role2: null },
    { type: 'authornote', role2: 'weird' },
    { type: 'memory', role2: 'user' },
    { type: 'lorebook' },
    { type: 'cache', name: 'c', depth: 1, role: 'bot' },
    { type: 'cache', name: 'd', depth: 2 },
    { type: 'cache', name: 'e', depth: 3, role: 'system' },
    { type: 'chat', rangeStart: 0, rangeEnd: 'end' },
    null, 'text item', ['array', 'item'], 7,
]

// Plain preset files: the template under the file's own fields.
importCase('import-minimal', { name: 'Mine' })
importCase('import-empty-object', {})
importCase('import-full', {
    name: 'Full', apiType: 'openai', temperature: 55, maxContext: 16000, mainPrompt: 'Be brief.', promptTemplate,
    7: 'index key', customField: { nested: [1, 2, { deep: true }] }, bias: [['a', 1]], image: 'preset-image',
    fallbackModels: { model: ['x'] }, verbosity: 0, top_p: 0.5,
})
importCase('import-message-name-settings', { name: 'Names', groupTemplate: '{{char}}: ', groupOtherBotRole: 'user' })
importCase('import-message-name-null', { groupTemplate: null })
importCase('import-message-name-overwrite', { messageNameTemplate: 'local', name: 'Both', groupTemplate: 'upstream', namedMessageRole: 'bot' })
importCase('import-name-null', { name: null, temperature: 1 })
importCase('import-prompt-template-string', { name: 'String template', promptTemplate: 'not a list' })
importCase('import-prompt-template-null', { name: 'Null template', promptTemplate: null })
importCase('import-prompt-template-empty', { promptTemplate: [] })
importCase('import-template-key-order', { verbosity: 2, zeta: 'new', name: 'Order', apiType: 'claude', alpha: 'second new' })
importCase('import-id-present', { id: 'stored-id', name: 'Has id' })
importCase('import-unicode', { name: '프리셋 🐿️ \ud800', mainPrompt: 'line\nbreak\u0000nul' })
importCase('import-large', { name: 'Large', mainPrompt: 'x'.repeat(200000), bias: Array.from({ length: 2000 }, (_, i) => [`b${i}`, i]) })
importCase('import-null', null)
importCase('import-number', 5)
importCase('import-true', true)
importCase('import-string', 'ab', { decline: 'preset' })
importCase('import-array', [{ name: 'in array' }], { decline: 'preset' })
importCase('import-proto-key', { name: 'Proto', extra: protoObject }, { decline: 'protoKey' })
importCase('import-preset-version-2', { presetVersion: 2, name: 'Version 2' })
importCase('import-preset-version-0', { presetVersion: 0, name: 'Version 0' })
importCase('import-preset-version-true', { presetVersion: true, name: 'Version true' })
importCase('import-preset-version-string', { presetVersion: '3', parameters: {} }, { decline: 'presetVersion' })
importCase('import-preset-version-object', { presetVersion: { v: 3 }, name: 'Version object' }, { decline: 'presetVersion' })

// NovelAI presets: version 3 and up, read from `parameters` into the NAI preset.
const parameters = {
    temperature: 1.25, max_length: 150, top_k: 10, top_p: 0.9, top_a: 0.05, typical_p: 0.95, tail_free_sampling: 0.9,
    repetition_penalty: 1.1, repetition_penalty_range: 1024, repetition_penalty_slope: 0.5,
    repetition_penalty_frequency: 0.01, repetition_penalty_presence: 0.02, cfg_scale: 1.2, mirostat_lr: 0.5, mirostat_tau: 2,
}
importCase('import-nai', { presetVersion: 3, name: 'NAI one', parameters })
importCase('import-nai-version-5', { presetVersion: 5, name: 'NAI five', parameters: { ...parameters, temperature: 0.7, extra: 'ignored' } })
importCase('import-nai-no-name', { presetVersion: 4, parameters: {} })
importCase('import-nai-null-parameters', { presetVersion: 3, name: null, parameters: Object.fromEntries(Object.keys(parameters).map(key => [key, null])) })
importCase('import-nai-bool-parameters', { presetVersion: 3, parameters: { temperature: true, repetition_penalty_presence: false, top_k: 'kept as is' } })
importCase('import-nai-no-parameters', { presetVersion: 3, name: 'No parameters' }, { decline: 'parameters' })
importCase('import-nai-parameters-array', { presetVersion: 3, parameters: [1] }, { decline: 'parameters' })
importCase('import-nai-string-temperature', { presetVersion: 3, parameters: { temperature: '1' } }, { decline: 'parameters' })
importCase('import-nai-object-presence', { presetVersion: 3, parameters: { repetition_penalty_presence: {} } }, { decline: 'parameters' })

// SillyTavern presets: prompts in `prompt_order[0].order`, looked up in `prompts`.
const stPrompts = [
    { identifier: 'main', content: 'Main prompt', role: 'system' },
    { identifier: 'jailbreak', content: 'Jailbreak', role: 'assistant' },
    { identifier: 'nsfw', content: 'NSFW' },
    { identifier: 'dialogueExamples', content: 'ignored' },
    { identifier: 'charPersonality', content: 'ignored' },
    { identifier: 'scenario', content: 'ignored' },
    { identifier: 'chatHistory' },
    { identifier: 'worldInfoBefore' },
    { identifier: 'worldInfoAfter' },
    { identifier: 'charDescription' },
    { identifier: 'personaDescription' },
    { identifier: 'custom', content: 'Custom', role: 'user' },
    { identifier: 'custom-char', content: 'Char role', role: 'char' },
    { identifier: 'custom-odd', role: 'narrator' },
    { identifier: 'custom-null', content: null, role: null },
    { identifier: 'disabled', content: 'never' },
    'stray string', ['stray array'], 42,
]
const stOrder = [
    ...stPrompts.slice(0, 15).map(prompt => ({ identifier: prompt.identifier, enabled: true })),
    { identifier: 'disabled', enabled: false },
    { identifier: 'missing', enabled: true },
    null, 'string order', { enabled: 1, identifier: 'custom' }, { identifier: 'main' },
]
importCase('import-st-full', {
    name: 'ignored name', temperature: 0.9, frequency_penalty: 0.4, presence_penalty: 0.3, top_p: 0.95,
    assistant_prefill: 'Sure,', prompt_order: [{ character_id: 1, order: stOrder }, { order: [] }], prompts: stPrompts,
})
importCase('import-st-minimal', { prompt_order: [{ order: [] }], prompts: [] })
importCase('import-st-null-sampling', { temperature: null, frequency_penalty: null, presence_penalty: null, top_p: null, prompt_order: [{ order: [] }], prompts: [] })
importCase('import-st-object-container', { prompt_order: { 0: { order: [{ identifier: 'main', enabled: true }] } }, prompts: [{ identifier: 'main', content: 'from object' }] })
importCase('import-st-prefill-object', { prompt_order: [{ order: [] }], prompts: [], assistant_prefill: { a: 1 } })
importCase('import-st-prefill-array', { prompt_order: [{ order: [] }], prompts: [], assistant_prefill: ['a', 'b'] })
importCase('import-st-number-identifier', { prompt_order: [{ order: [{ identifier: 5, enabled: true }, { identifier: '5', enabled: true }] }], prompts: [{ identifier: 5, content: 'five' }] })
importCase('import-st-empty-identifier', { prompt_order: [{ order: [{ enabled: true }, { identifier: null, enabled: true }] }], prompts: [{ identifier: '', content: 'empty' }] })
importCase('import-st-object-content', { prompt_order: [{ order: [{ identifier: 'x', enabled: true }] }], prompts: [{ identifier: 'x', content: { rich: true }, role: 'assistant' }] })
importCase('import-st-prompts-not-array', { name: 'Not ST', prompt_order: [{ order: [] }], prompts: {} })
importCase('import-st-order-not-array', { name: 'Not ST either', prompt_order: [{ order: 'x' }], prompts: [] })
importCase('import-st-order-string', { name: 'String order', prompt_order: 'abc', prompts: [] })
importCase('import-st-null-prompt-before-match', { prompt_order: [{ order: [{ identifier: 'main', enabled: true }] }], prompts: [null, { identifier: 'main' }] }, { decline: 'prompts' })
importCase('import-st-null-prompt-after-match', { prompt_order: [{ order: [{ identifier: 'main', enabled: true }] }], prompts: [{ identifier: 'main', content: 'first' }, null] })
importCase('import-st-string-temperature', { temperature: '0.5', prompt_order: [{ order: [] }], prompts: [] }, { decline: 'temperature' })
importCase('import-st-nai-version', { presetVersion: 3, parameters, prompt_order: [{ order: [] }], prompts: [] })

// Stored presets downloaded as JSON: the secrets blanked and the message name settings under their upstream names.
const stored = {
    name: 'Stored', id: 'stored-preset-id', apiType: 'openai', openAIKey: 'sk-synthetic', temperature: 70,
    forceReplaceUrl: 'https://proxy.invalid/a', forceReplaceUrl2: 'https://proxy.invalid/b', proxyKey: 'proxy-synthetic',
    textgenWebUIStreamURL: 'ws://local.invalid', textgenWebUIBlockingURL: 'http://local.invalid', mainPrompt: 'Main',
    messageNameTemplate: '{{char}}: ', namedMessageRole: 'user', promptTemplate, image: 'stored-image', 3: 'index key',
}
exportCase('export-full', stored)
exportCase('export-no-secrets', { name: 'No secrets', temperature: 1 })
exportCase('export-message-name-null', { name: 'Null names', messageNameTemplate: null })
exportCase('export-message-name-both', { groupTemplate: 'old', name: 'Both', messageNameTemplate: 'new', groupOtherBotRole: 'bot', namedMessageRole: 'user' })
exportCase('export-unicode', { name: '내보내기 🐿️ \ud800', openAIKey: '키' })
exportCase('export-large', { name: 'Large', mainPrompt: 'y'.repeat(200000), promptTemplate: Array.from({ length: 2000 }, (_, i) => ({ type: 'plain', text: `${i}`, role: 'user' })) })
exportCase('export-proto-key', { name: 'Proto', extra: protoObject }, { decline: 'protoKey' })

// Downloaded files imported again.
chain('chain-json-full', 'json', stored)
chain('chain-risup-full', 'risup', stored)
chain('chain-risup-minimal', 'risup', { name: 'Minimal' })
chain('chain-risup-message-names', 'risup', { name: 'Names', groupTemplate: 'old', messageNameTemplate: 'new' })
chain('chain-json-unicode', 'json', { name: '체인 🐿️', promptTemplate: [{ type: 'cache', role: 'char' }] })

// `.risup` and `.risupreset` containers: version 0 or 2, type 'preset', the body under `preset` or the older `pres`.
const body = { name: 'Container body', temperature: 42, groupTemplate: '{{char}}: ', mainPrompt: 'From the container' }
risup('risup-v2-preset', { presetVersion: 2, type: 'preset', preset: encrypted(body) })
risup('risup-v0-preset', { presetVersion: 0, type: 'preset', preset: encrypted(body) })
risup('risup-v0-pres', { presetVersion: 0, type: 'preset', pres: encrypted({ name: 'Old field' }) })
risup('risup-v2-pres', { presetVersion: 2, type: 'preset', pres: encrypted({ name: 'Old field, new version' }) })
risup('risup-preset-over-pres', { presetVersion: 2, type: 'preset', preset: encrypted({ name: 'Preset field' }), pres: encrypted({ name: 'Pres field' }) })
risup('risup-null-preset-falls-back', { presetVersion: 2, type: 'preset', preset: null, pres: encrypted({ name: 'Pres after null' }) })
risup('risup-key-order', { pres: encrypted({ name: 'Ordered' }), type: 'preset', presetVersion: 0, extra: 'ignored' })
risup('risupreset-v2', { presetVersion: 2, type: 'preset', preset: encrypted({ name: 'Legacy extension' }) }, { extension: 'risupreset' })
risup('risupreset-v0-pres', { presetVersion: 0, type: 'preset', pres: encrypted({ name: 'Legacy pres' }) }, { extension: 'risupreset' })
risup('risup-body-nai', { presetVersion: 2, type: 'preset', preset: encrypted({ presetVersion: 3, name: 'NAI inside', parameters }) })
risup('risup-body-st', { presetVersion: 2, type: 'preset', preset: encrypted({ prompt_order: [{ order: [{ identifier: 'main', enabled: true }] }], prompts: [{ identifier: 'main', content: 'ST inside' }] }) })
risup('risup-body-template', { presetVersion: 2, type: 'preset', preset: encrypted({ name: 'Template', promptTemplate }) })
risup('risup-body-unicode', { presetVersion: 2, type: 'preset', preset: encrypted({ name: '컨테이너 🐿️', mainPrompt: 'line\nbreak\u0000nul' }) })
risup('risup-body-null', { presetVersion: 2, type: 'preset', preset: encrypted(null) })
risup('risup-body-number', { presetVersion: 2, type: 'preset', preset: encrypted(7) })
risup('risup-body-array', { presetVersion: 2, type: 'preset', preset: encrypted([{ name: 'in array' }]) }, { decline: 'preset' })
risup('risup-body-string', { presetVersion: 2, type: 'preset', preset: encrypted('ab') }, { decline: 'preset' })
risup('risup-version-1', { presetVersion: 1, type: 'preset', preset: encrypted(body) }, { decline: 'risupContainer' })
risup('risup-version-3', { presetVersion: 3, type: 'preset', preset: encrypted(body) }, { decline: 'risupContainer' })
risup('risup-version-string', { presetVersion: '2', type: 'preset', preset: encrypted(body) }, { decline: 'risupContainer' })
risup('risup-version-missing', { type: 'preset', preset: encrypted(body) }, { decline: 'risupContainer' })
risup('risup-type-module', { presetVersion: 2, type: 'module', preset: encrypted(body) }, { decline: 'risupContainer' })
risup('risup-type-missing', { presetVersion: 0, pres: encrypted(body) }, { decline: 'risupContainer' })
risup('risup-container-array', [2, 'preset'], { decline: 'risupContainer' })
risup('risup-container-null', null, { decline: 'risupContainer' })
risup('risup-container-number', 2, { decline: 'risupContainer' })
risup('risup-body-string-field', { presetVersion: 2, type: 'preset', preset: 'not binary' }, { decline: 'risupBody' })
risup('risup-body-missing', { presetVersion: 2, type: 'preset' }, { decline: 'risupBody' })
risup('risup-body-false', { presetVersion: 0, type: 'preset', preset: false, pres: encrypted(body) }, { decline: 'risupBody' })

writeFileSync(`${dir}presets.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
console.log(`wrote ${cases.length} cases`)
