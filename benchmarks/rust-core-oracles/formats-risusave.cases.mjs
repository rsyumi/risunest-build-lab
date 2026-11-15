// Writes the synthetic case file of the RisuSave import oracle (tests/fixtures/rust-core/formats/risusave.cases.json).
// Add cases here, run `node benchmarks/rust-core-oracles/formats-risusave.cases.mjs`, then regenerate the expected file
// with RUST_CORE_ORACLE_WRITE=1.
//
// `constants` records the mirror maps and protected preset groups. `external` runs `prepareExternalDatabaseImport`,
// `upstream` runs `prepareUpstreamImport`, `assign-ids` runs `assignIds` and `char-order` runs `checkCharOrder` on
// `database`. `route` imports `database`, written as a compressed legacy save, through the web `.risudat` import route
// and keeps the database it hands the persistent store. `chain` keeps the database the web runtime then prepares from
// it with `prepareDatabaseForPersistence`, with `Date.now()` at `now`. `decline` names the `Unsupported` the Rust port
// must answer instead of a result. The shapes follow the pinned-reference contract fixtures in tests/compatibleExport.node-test.mjs
// (group and `§temp` characters, `characterOrder` folders, loadouts, personas, plugin storage, `formatversion: 5`).
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
const kind = name => (id, database, extra = {}) => add({ id, kind: name, database, ...extra })
const external = kind('external')
const upstream = kind('upstream')
const route = kind('route')
const NOW = 1_700_000_000_000
const DAY = 1000 * 60 * 60 * 24
const chain = (id, database, extra = {}) => add({ id, kind: 'chain', database, now: NOW, ...extra })
const assignIds = kind('assign-ids')
const charOrder = kind('char-order')
// An object with an own `__proto__` key, which only JSON.parse creates.
const protoObject = JSON.parse('{"__proto__": {"polluted": true}, "plain": 1}')

add({ id: 'constants', kind: 'constants' })

const character = (chaId, extra = {}) => ({ chaId, type: 'character', name: `Character ${chaId}`, chats: [], ...extra })
// The multi-character fixture of the pinned-reference contract test, with synthetic text.
const compat = {
    characters: [
        character('synthetic-a', {
            chats: [
                { id: 'synthetic-chat-1', name: 'Chat', message: [{ role: 'user', data: '합성 질문' }, { role: 'char', data: '합성 답변' }] },
                { id: 'synthetic-chat-2', name: 'Empty', message: [] },
            ],
        }),
        character('synthetic-b'),
    ],
    characterOrder: ['synthetic-a', 'synthetic-b'],
    pluginCustomStorage: { syntheticPlugin: { arbitrary: null, nested: ['한글', 0.125, Number.MAX_SAFE_INTEGER], text: 'do not rewrite assets/synthetic.png substring' } },
    personas: [{ id: 'synthetic-persona', name: 'Synthetic Persona', personaPrompt: 'Synthetic prompt', icon: '' }],
    modules: [],
    botPresets: [],
    adaptiveThinkingEffort: 'high',
    formatversion: 5,
}
// An upstream database with every part the import steps touch.
const full = {
    formatversion: 5,
    username: 'Root user', userIcon: 'assets/user.png', personaPrompt: 'Root persona prompt', userNote: 'Root note',
    temperature: 55, mainPrompt: 'Root main prompt', NAIsettings: { topK: 3 }, presetRegex: [{ in: 'a', out: 'b' }],
    reasoningEffort: 1, promptTemplate: [{ type: 'plain', text: 'root', role: 'system' }],
    groupTemplate: '{{char}}: ', groupOtherBotRole: 'user',
    globalChatVariables: { toggle_x: '1', score: '3' },
    doNotChangeSeperateModels: true, seperateModels: { memory: 'm' },
    characters: [
        character('char-a', { chats: [{ id: 'chat-a', message: [] }] }),
        { chaId: 'group-1', type: 'group', name: 'Group', characters: ['char-a'] },
        character('§temp'),
        character('char-b', { trashTime: 1 }),
        character('char-c'),
    ],
    characterOrder: ['char-a', 'group-1', '§temp', { id: 'folder-1', name: 'Folder', data: ['group-1', 'char-c'] }, { id: 'folder-2', data: ['group-1'] }, 'char-b'],
    loadouts: [{ id: 'loadout-1', name: 'Loadout', characterIds: ['char-a', 'group-1', '§temp'] }, { id: 'loadout-1', name: 'Repeated id' }, { name: 'No id', characterIds: null }],
    botPresets: [
        { name: 'First', id: 'preset-1', groupTemplate: 'first: ' },
        { name: 'Selected', temperature: 10, regex: [], groupOtherBotRole: 'bot' },
        { name: 'Repeated', id: 'preset-1' },
    ],
    botPresetsId: 1,
    personas: [{ name: 'Persona', icon: '', embeddedModule: { name: 'Embedded' } }, { id: 'persona-2', name: 'Second', embeddedModule: { id: 'kept', name: 'Kept' } }],
    selectedPersona: 0,
    modules: [{ name: 'Module', id: '' }, { name: 'Module 2', id: 'module-2' }, { name: 'Module 3', id: 'module-2' }],
    customModels: [{ name: 'Custom', id: 7 }],
    protectedPresetValues: { groupTemplate: 'protected: ' },
}

// prepareExternalDatabaseImport
external('external-minimal', { characters: [] })
external('external-full', full)
external('external-compat', compat)
external('external-empty-folder-dropped', { characters: [{ type: 'group', chaId: 'g' }], characterOrder: [{ id: 'f', data: ['g'] }, { id: 'kept', data: [] }, 'g', 'other'] })
external('external-group-without-id', { characters: [{ type: 'group' }, character('a')], characterOrder: ['a', 'undefined'] })
external('external-group-number-id', { characters: [{ type: 'group', chaId: 5 }], characterOrder: [{ data: [5, '5'] }] })
external('external-temp-number-type', { characters: [{ type: 7, chaId: '§temp' }], loadouts: [{ characterIds: ['§temp', 'x'] }] })
external('external-order-not-array', { characters: [], characterOrder: 'not a list' })
external('external-loadout-ids-absent', { characters: [], loadouts: [{ name: 'Absent' }, { name: 'Null', characterIds: null }] })
external('external-loadouts-null', { characters: [], loadouts: null, botPresets: null })
external('external-message-names-null', { characters: [], groupTemplate: null })
external('external-message-names-overwrite', { messageNameTemplate: 'local', characters: [], groupTemplate: 'upstream', namedMessageRole: 'bot' })
external('external-protected-string', { characters: [], protectedPresetValues: 'truthy text' })
external('external-protected-falsy', { characters: [], protectedPresetValues: 0, groupOtherBotRole: 'user' })
external('external-preset-primitives', { characters: [], botPresets: [5, 'text', [], false, { groupTemplate: 'object: ' }] })
external('external-primitive-characters', { characters: [5, 'text', [], true, { type: 'group', chaId: 'g' }], characterOrder: ['g', 'text'] })
// Index-like keys come first in JS property order; the rename appends after them and the other keys.
external('external-protected-index-keys', { characters: [], protectedPresetValues: { 10: 'ten', groupTemplate: 'g: ', 0: 'zero', zeta: 'z' } })
external('external-unicode', { characters: [character('캐릭터 🐿️ \ud800'), { type: 'group', chaId: '그룹' }], characterOrder: ['캐릭터 🐿️ \ud800', '그룹'], groupTemplate: '이름: ' })
external('external-not-object', null, { decline: 'database' })
external('external-array', [], { decline: 'database' })
external('external-characters-missing', {}, { decline: 'characters' })
external('external-characters-object', { characters: {} }, { decline: 'characters' })
external('external-characters-null-entry', { characters: [null] }, { decline: 'characters' })
external('external-order-null-entry', { characters: [], characterOrder: [null] }, { decline: 'characterOrder' })
external('external-order-number-entry', { characters: [], characterOrder: [3] }, { decline: 'characterOrder' })
external('external-order-array-entry', { characters: [], characterOrder: [['a']] }, { decline: 'characterOrder' })
external('external-order-folder-no-data', { characters: [], characterOrder: [{ id: 'f' }] }, { decline: 'characterOrder' })
external('external-order-folder-string-data', { characters: [], characterOrder: [{ data: 'abc' }] }, { decline: 'characterOrder' })
external('external-loadouts-object', { characters: [], loadouts: {} }, { decline: 'loadouts' })
external('external-loadouts-string', { characters: [], loadouts: 'ab' }, { decline: 'loadouts' })
external('external-loadout-null', { characters: [], loadouts: [null] }, { decline: 'loadouts' })
external('external-loadout-ids-string', { characters: [], loadouts: [{ characterIds: 'abc' }] }, { decline: 'characterIds' })
external('external-presets-object', { characters: [], botPresets: {} }, { decline: 'botPresets' })
external('external-presets-null-entry', { characters: [], botPresets: [null] }, { decline: 'botPresets' })
external('external-proto-key', { characters: [], extra: protoObject }, { decline: 'protoKey' })

// prepareUpstreamImport
upstream('upstream-minimal', {})
upstream('upstream-full', full)
upstream('upstream-compat', compat)
upstream('upstream-ids', {
    botPresets: [{ name: 'a' }, { id: '', name: 'b' }, { id: 'p', name: 'c' }, { id: 'p', name: 'd' }, { id: 3, name: 'e' }],
    personas: [{ id: 'p', name: 'same id as a preset' }, { name: 'no id', embeddedModule: { id: '' } }, { id: 'x', embeddedModule: null }, { id: 'y', embeddedModule: 0 }, { id: 'z', embeddedModule: { id: 0 } }],
    modules: [{ id: 'm' }, { id: 'm' }, {}], loadouts: [{ id: 'm' }], customModels: [{ id: null }, { id: 'c' }],
})
upstream('upstream-preset-mirror-new-fields', { botPresets: [{ name: 'only' }], botPresetsId: 0, temperature: 1, NAIsettings: { a: 1 }, presetRegex: null, reasoningEffort: 2, verbosity: 0, dynamicOutput: { x: true } })
upstream('upstream-preset-mirror-string-index', { botPresets: [{ id: 'a' }, { id: 'b', temperature: 5 }], botPresetsId: '1', temperature: 9 })
upstream('upstream-preset-index-out-of-range', { botPresets: [{ id: 'a' }], botPresetsId: 4, temperature: 9 })
upstream('upstream-preset-index-fraction', { botPresets: [{ id: 'a' }, { id: 'b' }], botPresetsId: 0.5, temperature: 9 })
upstream('upstream-preset-index-negative', { botPresets: [{ id: 'a' }], botPresetsId: -1, temperature: 9 })
upstream('upstream-preset-index-null', { botPresets: [{ id: 'a' }], botPresetsId: null, temperature: 9 })
upstream('upstream-preset-index-bool', { botPresets: [{ id: 'a' }], botPresetsId: true, temperature: 9 })
upstream('upstream-preset-index-missing', { botPresets: [{ id: 'a' }], mainPrompt: 'root' })
upstream('upstream-no-presets-string-index', { botPresetsId: 'length', temperature: 9 })
upstream('upstream-persona-mirror', { personas: [{ id: 'a' }, { id: 'b', name: 'old', note: 'old note', extra: 1 }], selectedPersona: 1, username: 'New', userIcon: '', personaPrompt: null, userNote: 'note' })
upstream('upstream-global-vars-existing-explicit', { explicitGlobalChatVariables: { old: '1' }, name: 'x', globalChatVariables: { 1: 'index', b: '2', a: '3' } })
upstream('upstream-global-vars-null', { globalChatVariables: null })
upstream('upstream-global-vars-number', { globalChatVariables: 5 })
upstream('upstream-protected-absent', { doNotChangeFallbackModels: 1, fallbackModels: { a: ['b'] }, fallbackWhenBlankResponse: false })
upstream('upstream-protected-null', { protectedPresetValues: null, after: 1, disableSeperateParameterChangeOnPresetChange: 'yes', seperateParameters: { memory: {} } })
upstream('upstream-protected-existing', { protectedPresetValues: { seperateModels: 'old', other: 1 }, doNotChangeSeperateModels: true, seperateModelsForAxModels: true })
upstream('upstream-protected-flag-off', { protectedPresetValues: 'kept text', doNotChangeSeperateModels: false, seperateModels: 'x' })
upstream('upstream-global-vars-index-keys', { globalChatVariables: { b: '1', 10: 'ten', 0: 'zero', a: '2', '01': 'not an index', 4294967295: 'not an index' } })
upstream('upstream-protected-index-keys', { protectedPresetValues: { 10: 'ten', zeta: 'z', 0: 'zero' }, doNotChangeSeperateModels: true, seperateModels: { memory: 'm' } })
upstream('upstream-unicode', { personas: [{ name: '페르소나 🐿️ \ud800' }], selectedPersona: 0, username: '사용자', globalChatVariables: { 변수: '값' } })
upstream('upstream-not-object', null, { decline: 'database' })
upstream('upstream-array', [{}], { decline: 'database' })
upstream('upstream-proto-key', { extra: protoObject }, { decline: 'protoKey' })
upstream('upstream-presets-object', { botPresets: {} }, { decline: 'botPresets' })
upstream('upstream-presets-primitive', { botPresets: ['a'] }, { decline: 'botPresets' })
upstream('upstream-personas-null-entry', { personas: [null] }, { decline: 'personas' })
upstream('upstream-modules-array-entry', { modules: [[]] }, { decline: 'modules' })
upstream('upstream-custom-models-string', { customModels: 'ab' }, { decline: 'customModels' })
upstream('upstream-preset-index-length', { botPresets: [{ id: 'a' }], botPresetsId: 'length' }, { decline: 'botPresetsId' })
upstream('upstream-preset-index-array', { botPresets: [{ id: 'a' }], botPresetsId: [0], temperature: 1 }, { decline: 'botPresetsId' })
upstream('upstream-persona-index-object', { personas: [{ id: 'a' }], selectedPersona: {}, username: 'x' }, { decline: 'selectedPersona' })
upstream('upstream-global-vars-string', { globalChatVariables: 'ab' }, { decline: 'globalChatVariables' })
upstream('upstream-global-vars-array', { globalChatVariables: ['a'] }, { decline: 'globalChatVariables' })
upstream('upstream-protected-string', { protectedPresetValues: 'text', doNotChangeFallbackModels: true }, { decline: 'protectedPresetValues' })
upstream('upstream-protected-array', { protectedPresetValues: [], doNotChangeFallbackModels: true }, { decline: 'protectedPresetValues' })
upstream('upstream-embedded-string', { personas: [{ id: 'a', embeddedModule: 'text' }] }, { decline: 'embeddedModule' })
upstream('upstream-embedded-array', { personas: [{ id: 'a', embeddedModule: [] }] }, { decline: 'embeddedModule' })

// The web `.risudat` import route.
route('route-minimal', { characters: [] })
route('route-full', full)
route('route-compat', compat)
route('route-groups-only', { characters: [{ type: 'group', chaId: 'g1' }, character('§temp')], characterOrder: [{ id: 'f', data: ['g1'] }, 'g1'], loadouts: [{ id: 'l', characterIds: ['g1'] }] })
route('route-unicode', { characters: [character('캐릭터 🐿️')], username: '사용자', groupTemplate: '이름: ', personas: [{ name: '페르소나' }], selectedPersona: 0 })
route('route-characters-missing', {}, { decline: 'characters' })

// The whole web import: the route, then the persistence preparation.
chain('chain-minimal', { characters: [] })
chain('chain-full', full)
chain('chain-compat', compat)
chain('chain-groups-only', { characters: [{ type: 'group', chaId: 'g1' }, character('§temp')], characterOrder: [{ id: 'f', data: ['g1'] }, 'g1'], loadouts: [{ id: 'l', characterIds: ['g1'] }] })
chain('chain-unicode', { characters: [character('캐릭터 🐿️')], username: '사용자', groupTemplate: '이름: ', personas: [{ name: '페르소나' }], selectedPersona: 0 })
chain('chain-index-keys', {
    characters: [], globalChatVariables: { b: '1', 10: 'ten', 0: 'zero', a: '2', '01': 'not an index' },
    protectedPresetValues: { 10: 'ten', zeta: 'z', 0: 'zero' }, doNotChangeSeperateModels: true, seperateModels: { memory: 'm' },
})
chain('chain-old-format', {
    customBackground: 'C:\\x\\assets\\bg.png', loreBookToken: 100, groupTemplate: 'g: ',
    characters: [
        character('a', { image: 'C:\\y\\assets\\a.png', chats: [{ message: [] }, { id: 'dup', message: [] }] }),
        character('a', { chats: [{ id: 'dup' }] }),
        character('gone', { trashTime: NOW - 4 * DAY }),
        character('trashed', { trashTime: NOW - DAY }),
        { type: 'group', chaId: 'g' },
    ],
    characterOrder: ['gone', { id: 'folder', data: ['a', 'gone', 'g'] }, { data: [] }, 'missing'],
    personas: [{ name: 'p' }, { id: 'kept', name: 'q' }],
    modules: [{ id: 'm', name: 'mod', lorebook: [{ content: '<char> @@end' }] }],
})
chain('chain-plugin-storage-array', { characters: [], pluginCustomStorage: [] })
chain('chain-cold-plugin-index', { characters: [], pluginCustomStorage: { _coldpluginIndex: 'index', kept: 1 } }, { decline: 'coldPluginStorage' })
chain('chain-cold-plugin-legacy', { characters: [], pluginCustomStorage: { _coldplugin: { a: 'key' } } }, { decline: 'coldPluginStorage' })
chain('chain-characters-missing', {}, { decline: 'characters' })

// assignIds
assignIds('assign-ids-fresh', { characters: [character(''), character('a', { chats: [{ id: '' }, { id: 'c' }, { id: 'c' }, {}] }), character('a'), character(null)] })
assignIds('assign-ids-collision', { characters: [character('00000000-0000-4000-8000-000000000001'), character(''), character('b', { chats: [{ id: 'b' }] })] })
assignIds('assign-ids-falsy', { characters: [character(0), character(false, { chats: [{ id: null }, { id: 0 }] })] })
assignIds('assign-ids-key-order', { characters: [{ name: 'no id yet', chats: [{ message: [] }] }] })
assignIds('assign-ids-compat', compat)
assignIds('assign-ids-unicode', { characters: [character('캐릭터'), character('캐릭터', { chats: [{ id: '대화' }, { id: '대화' }] })] })
assignIds('assign-ids-number-cha-id', { characters: [character(5)] }, { decline: 'chaId' })
assignIds('assign-ids-number-chat-id', { characters: [character('a', { chats: [{ id: 3 }] })] }, { decline: 'chatId' })
assignIds('assign-ids-chats-missing', { characters: [{ chaId: 'a' }] }, { decline: 'chats' })
assignIds('assign-ids-chat-null', { characters: [character('a', { chats: [null] })] }, { decline: 'chats' })
assignIds('assign-ids-characters-object', { characters: {} }, { decline: 'characters' })
assignIds('assign-ids-character-null', { characters: [null] }, { decline: 'characters' })
assignIds('assign-ids-proto-key', { characters: [], extra: protoObject }, { decline: 'protoKey' })

// checkCharOrder
charOrder('char-order-missing', { characters: [character('a'), character('b')] })
charOrder('char-order-null', { characterOrder: null, characters: [character('a')] })
charOrder('char-order-full', {
    characters: [character('a'), character('b', { trashTime: 5 }), character('§playground'), character('c'), character('d'), character('e')],
    characterOrder: [
        'a', 'b', 'gone', null, 0, false, '',
        { id: 'empty', data: [] }, { id: 'emptied', data: ['gone', 'b'] }, { id: 'kept', data: ['gone', 'c', 'gone', 5, null] }, 'e',
    ],
})
charOrder('char-order-duplicate-ids', { characters: [character('x'), character('x')], characterOrder: [] })
charOrder('char-order-trashed-zero', { characters: [character('a', { trashTime: 0 }), character('b', { trashTime: '' })], characterOrder: ['b'] })
charOrder('char-order-compat', compat)
charOrder('char-order-unicode', { characters: [character('캐릭터 🐿️ \ud800')], characterOrder: ['없음', { data: ['캐릭터 🐿️ \ud800'] }] })
charOrder('char-order-string', { characters: [], characterOrder: 'abc' }, { decline: 'characterOrder' })
charOrder('char-order-object', { characters: [], characterOrder: {} }, { decline: 'characterOrder' })
charOrder('char-order-number-entry', { characters: [], characterOrder: [1] }, { decline: 'characterOrder' })
charOrder('char-order-array-entry', { characters: [], characterOrder: [['a']] }, { decline: 'characterOrder' })
charOrder('char-order-folder-no-data', { characters: [], characterOrder: [{ id: 'f' }] }, { decline: 'characterOrder' })
charOrder('char-order-folder-string-data', { characters: [], characterOrder: [{ data: 'ab' }] }, { decline: 'characterOrder' })
charOrder('char-order-cha-id-missing', { characters: [{ name: 'no id' }] }, { decline: 'chaId' })
charOrder('char-order-characters-missing', { characterOrder: [] }, { decline: 'characters' })
charOrder('char-order-character-null', { characters: [null] }, { decline: 'characters' })
charOrder('char-order-proto-key', { characters: [], extra: protoObject }, { decline: 'protoKey' })

writeFileSync(`${dir}risusave.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
console.log(`wrote ${cases.length} cases`)
