// Writes the synthetic case file of the template glue oracle (tests/fixtures/rust-core/templates/glue.cases.json). Add
// cases here, run `node benchmarks/rust-core-oracles/templates-glue.cases.mjs`, then regenerate the expected file with
// RUST_CORE_ORACLE_WRITE=1.
//
// `builtins` records `chatTemplates` and `templateEffect`. `chat` runs `applyChatTemplate(messages, arg)` with `arg`
// holding only the `type` and `custom` keys a case sets; `db` holds `instructChatTemplate` and `JinjaTemplate` when set,
// the current character has `name` only when `charName` is set, and `getUserName()` answers `userName`. `check` runs
// `templateCheck({ promptTemplate })`, with the key left out when the case has none. `decline` names the `Unsupported` the
// Rust port must answer instead of a result.
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/templates/', import.meta.url))
mkdirSync(dir, { recursive: true })

const cases = []
const ids = new Set()
function add(entry) {
    if (ids.has(entry.id)) throw new Error(`duplicate ${entry.id}`)
    ids.add(entry.id)
    cases.push(entry)
}
const chat = (id, messages, settings, extra = {}) => add({ id, kind: 'chat', messages, settings: { userName: 'Synthetic User', charName: 'Synthetic Character', ...settings }, ...extra })
const check = (id, promptTemplate, extra = {}) => add({ id, kind: 'check', ...(promptTemplate === undefined ? {} : { promptTemplate }), ...extra })

add({ id: 'builtins', kind: 'builtins' })

const m = (role, content) => ({ role, content })
const sets = {
    standard: [m('system', 'Synthetic system'), m('user', '  합성🐿️  '), m('assistant', 'Synthetic reply'), m('user', 'Follow-up')],
    alternating: [m('user', 'Q1'), m('assistant', 'A1'), m('user', 'Q2')],
    'assistant-first': [m('assistant', 'Greeting'), m('user', 'Hi'), m('assistant', 'Hello')],
    'double-user': [m('user', 'one'), m('user', 'two'), m('assistant', 'reply'), m('assistant', 'more'), m('user', 'three')],
    'system-middle': [m('user', 'u1'), m('system', 'mid system'), m('assistant', 'a1'), m('system', 'late')],
    'with-function': [m('user', 'u'), m('function', 'tool output'), m('assistant', 'a')],
    'only-system': [m('system', 'sys only')],
    empty: [],
    multiline: [m('user', 'line one\nline two\n'), m('assistant', '  padded  \n'), m('user', 'tab\tand "quotes" and \\ backslash')],
}
for (const type of ['llama3', 'llama2', 'chatml', 'gpt2', 'gemma', 'mistral', 'vicuna', 'alpaca']) {
    for (const [name, messages] of Object.entries(sets)) chat(`${type}-${name}`, messages, { type })
}
chat('db-type', sets.standard, { instructChatTemplate: 'chatml' })
chat('type-overrides-db', sets.standard, { type: 'vicuna', instructChatTemplate: 'chatml' })
chat('no-type', sets.standard, {})
chat('empty-type', sets.standard, { type: '' })
chat('empty-type-falls-to-db', sets.standard, { type: '', instructChatTemplate: 'chatml' })
chat('unknown-type', sets.standard, { type: 'llama4' })
chat('prototype-type', sets.standard, { type: 'toString' })
chat('jinja-custom', sets.standard, { type: 'jinja', custom: '{{ risu_char }}|{{ risu_user }}\n{% for message in messages %}[{{ loop.index }}:{{ message.role }}]{{ message.content | trim }}\n{% endfor %}{% if add_generation_prompt %}assistant:{% endif %}' })
chat('jinja-db-template', sets.alternating, { instructChatTemplate: 'jinja', JinjaTemplate: '{% for m in messages %}<{{ m.role }}>{{ m.content }}{% endfor %}[{{ eos_token }}][{{ bos_token }}]' })
chat('jinja-custom-overrides-db', sets.alternating, { type: 'jinja', custom: 'custom', JinjaTemplate: 'db' })
chat('jinja-empty-custom', sets.alternating, { type: 'jinja', custom: '', JinjaTemplate: 'db' })
chat('jinja-missing-template', sets.alternating, { type: 'jinja' })
chat('jinja-invalid', sets.alternating, { type: 'jinja', custom: '{% if %}' })
chat('jinja-raise', sets.alternating, { type: 'jinja', custom: "{% if messages|length > 2 %}{{ raise_exception('Too many: ' ~ messages|length) }}{% endif %}" })
chat('jinja-no-char-name', sets.alternating, { type: 'jinja', custom: '[{{ risu_char }}][{{ risu_char is defined }}]', charName: undefined })
chat('jinja-function-skipped', sets['with-function'], { type: 'jinja', custom: '{{ messages|length }}{{ messages|map(attribute="role")|join(",") }}' })
chat('jinja-message-keys', [{ role: 'user', content: 'x', name: 'n', memo: 'm' }], { type: 'jinja', custom: '{{ messages[0]|tojson }}' })
chat('jinja-context-keys', sets.empty, { type: 'jinja', custom: '{% for m in messages %}{% endfor %}{{ add_generation_prompt }} {{ risu_user|length }}' })
chat('jinja-unicode-names', sets.alternating, { type: 'jinja', custom: '{{ risu_char }}/{{ risu_user }}', charName: '캐릭터 😀', userName: '사용자' })
// The app passes no clock to the port; `%%` keeps the recorded JS result independent of the date.
chat('jinja-strftime', sets.alternating, { type: 'jinja', custom: '{{ strftime_now("%%") }}' }, { decline: 'strftime-now' })
chat('jinja-tool-style', [m('system', 'S'), m('user', 'u'), m('assistant', 'a')], { type: 'jinja', custom: "{%- for message in messages %}{{- '<|im_start|>' + message.role + '\\n' + message.content + '<|im_end|>\\n' }}{%- endfor %}{%- if add_generation_prompt %}{{- '<|im_start|>assistant\\n' }}{%- endif %}" })

// templateCheck
const card = (type, extra = {}) => ({ type, ...extra })
const standardTemplate = [
    card('plain', { type2: 'main', text: 'main', role: 'system' }),
    card('description'),
    card('lorebook'),
    card('chat', { rangeStart: 0, rangeEnd: -4 }),
    card('jailbreak', { type2: 'globalNote', text: 'note', role: 'system' }),
    card('chat', { rangeStart: -4, rangeEnd: 'end' }),
]
check('check-absent', undefined)
check('check-null', null)
check('check-empty-string', '')
check('check-empty', [])
check('check-standard', standardTemplate)
check('check-no-range-split', [card('plain', { type2: 'main' }), card('jailbreak', { type2: 'globalNote' }), card('description'), card('lorebook'), card('chat', { rangeStart: 0, rangeEnd: 'end' })])
check('check-multiple', [card('plain', { type2: 'main' }), card('jailbreak', { type2: 'main' }), card('plain', { type2: 'globalNote' }), card('plain', { type2: 'globalNote' }), card('chat', { rangeStart: 0, rangeEnd: 'end' })])
check('check-unresolved', [card('chat', { rangeStart: 0, rangeEnd: -10 }), card('chat', { rangeStart: -8, rangeEnd: -2 }), card('chat', { rangeStart: -2, rangeEnd: 'end' }), card('chat', { rangeStart: 1.5, rangeEnd: 3 })])
check('check-disabled-range', [card('chat', { rangeStart: -1000, rangeEnd: 7 }), card('chat', { rangeStart: 0, rangeEnd: 'end' })])
check('check-negative-zero', [card('chat', { rangeStart: JSON.parse('-0'), rangeEnd: 'end' })])
check('check-missing-bounds', [card('chat'), card('chat', { rangeStart: 2 })])
check('check-mixed-bounds', [card('chat', { rangeStart: '2', rangeEnd: 2 }), card('chat', { rangeStart: null, rangeEnd: true }), card('chat', { rangeStart: false, rangeEnd: '2' })])
check('check-other-cards', [card('persona'), card('authornote'), card('postEverything'), card('cot'), card('chatML'), card('memory'), card('cache'), card('plain', { type2: 'normal' }), card(5), {}])
check('check-primitive-cards', ['plain', 3, true, [card('description')]])
check('check-null-card', [card('description'), null])
check('check-object-template', { 0: card('description'), length: 1 }, { decline: 'promptTemplate' })
check('check-object-bound', [card('chat', { rangeStart: [1], rangeEnd: 'end' })], { decline: 'promptTemplate.range' })
check('check-type-case', [card('Plain', { type2: 'main' }), card('chat', { rangeStart: 0, rangeEnd: 'END' })])

writeFileSync(join(dir, 'glue.cases.json'), `{"schema": 1, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`)
console.log('glue', cases.length)
