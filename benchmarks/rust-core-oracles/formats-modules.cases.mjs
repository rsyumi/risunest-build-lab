// Writes the synthetic case file of the module mapping oracle (tests/fixtures/rust-core/formats/modules.cases.json). Add
// cases here, run `node benchmarks/rust-core-oracles/formats-modules.cases.mjs`, then regenerate the expected file
// with RUST_CORE_ORACLE_WRITE=1.
//
// `json` imports `data` as a `.json` module file through `importModuleData`. `risum` activates prepared RISUM content
// (`metadata`, one asset frame per `logicalIds` entry) and `charx` prepared charx content (`card`, `assets`,
// `portraitLogicalId`, `module`) through `activatePreparedNativeModuleContent`. `to-character` runs
// `convertModuleToCharacter(module)` with `root` as the database, `risum-export` writes `module` through
// `exportModuleLegacy` without saving it, and `module-charx` exports `module` through `exportModule`, whose fallback
// portrait the asset store saves as `noneImage`. `decline` names the `Unsupported` the Rust port must answer instead of
// a result; a case TS rejects without one must be an `Invalid` error in Rust.
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
const json = (id, data, extra = {}) => add({ id, kind: 'json', data, ...extra })
const risum = (id, metadata, logicalIds = [], extra = {}) => add({ id, kind: 'risum', metadata, logicalIds, ...extra })
const charx = (id, card, extra = {}) => add({ id, kind: 'charx', card, ...extra })
const toCharacter = (id, module, extra = {}) => add({ id, kind: 'to-character', module, ...extra })
const risumExport = (id, module, extra = {}) => add({ id, kind: 'risum-export', module, ...extra })
const moduleCharx = (id, module, extra = {}) => add({ id, kind: 'module-charx', module, noneImage: 'saved-none-image', now: 1767225602345, ...extra })
// An object with an own `__proto__` key, which only JSON.parse creates.
const protoObject = JSON.parse('{"__proto__": {"polluted": true}, "plain": 1}')

const lore = (overrides = {}) => ({ key: 'alpha, beta', secondkey: '', insertorder: 100, comment: 'Lore', content: 'Lore content', mode: 'normal', alwaysActive: false, selective: false, ...overrides })
const regex = [{ comment: 'regex', in: 'a', out: 'b', type: 'editinput', ableFlag: false }]
const trigger = [{ comment: 'trigger', type: 'start', conditions: [], effect: [{ type: 'v2Header', code: '', indent: 0 }] }]
const fullModule = {
    type: 'risuModule', name: 'Synthetic Module', description: 'A synthetic module.', id: 'stored-module-id',
    lorebook: [lore(), lore({ key: '', comment: 'second', content: 'other', mode: 'folder', folder: 'f' })],
    regex, trigger, lowLevelAccess: false, hideIcon: true, backgroundEmbedding: '<style>x</style>',
    assets: [['asset one', 'stored-a1', 'png'], ['asset two', 'stored-a2', 'webp']], namespace: 'ns', customModuleToggle: 'toggle=1',
    mcp: { url: 'https://example.invalid/mcp' }, icon: 'stored-icon', extra: { nested: [1, 2] }, 5: 'index key',
}

// JSON module files: a `risuModule`, a risu lorebook, an external lorebook, a regex list.
json('json-risu-module-full', fullModule)
json('json-risu-module-id-first', { id: 'first', type: 'risuModule', name: 'Id first' })
json('json-risu-module-no-id', { type: 'risuModule', name: 'No id' })
json('json-risu-module-empty-name', { type: 'risuModule', name: '', id: 'x' })
json('json-risu-module-number-name', { type: 'risuModule', name: 7, id: 3 })
json('json-risu-module-stdio', { type: 'risuModule', name: 'Stdio', id: 'x', mcp: { url: 'stdio:{"command":"node"}' } })
json('json-risu-module-stdio-before-identity', { type: 'risuModule', mcp: { url: 'stdio:x' } })
json('json-risu-module-mcp-url-number', { type: 'risuModule', name: 'Url number', id: 'x', mcp: { url: 5 } })
json('json-risu-module-mcp-string', { type: 'risuModule', name: 'Mcp string', id: 'x', mcp: 'stdio:x' })
json('json-risu-module-low-level', { type: 'risuModule', name: 'Low level', id: 'x', lowLevelAccess: true }, { decline: 'lowLevelAccess' })
json('json-risu-module-unicode', { type: 'risuModule', name: '모듈 🐿️ \ud800', id: 'x', description: '설명 \udfff', lorebook: [lore({ key: '키, 열쇠', content: '내용 😀' })] })
json('json-risu-module-large', { type: 'risuModule', name: 'Large', id: 'x', lorebook: Array.from({ length: 500 }, (_, index) => lore({ key: `k${index}`, content: `c${index} ${'body '.repeat(index % 4)}`, insertorder: index })) })
json('json-risu-lorebook', { type: 'risu', ver: 1, data: [lore(), lore({ comment: 'two' })] })
json('json-risu-lorebook-named', { type: 'risu', name: 'Named book', description: 'Described', data: [] })
json('json-risu-lorebook-falsy-name', { type: 'risu', name: 0, description: '', data: [lore()] })
json('json-risu-lorebook-object-name', { type: 'risu', name: { first: 'a' }, data: [] })
json('json-risu-object-data', { type: 'risu', data: { a: 1 } })
json('json-risu-object-data-entries', { type: 'risu', data: { a: 1 }, entries: { one: { key: ['k'], content: 'from entries' } } })
const external = {
    zero: { key: ['k1', 'k2'], keys: ['ignored'], order: 3, comment: 'by comment', content: 'by content', constant: true, secondary_keys: ['s1', 's2'], selective: true },
    keys: { keys: ['only keys'], priority: 9, name: 'by name', entry: 'by entry', forceActivation: true },
    keywords: { keywords: ['kw', null, 5, { x: 1 }, ['p', 'q']], contextConfig: { budgetPriority: 400 }, displayName: 'by display name', text: 'by text' },
    empty: {},
    falsy: { key: '', keys: 0, keywords: null, order: null, priority: null, contextConfig: 5, comment: '', name: 0, displayName: null, content: '', entry: 0, text: null, constant: null, forceActivation: 0, secondary_keys: '', selective: null },
    emptyArrays: { key: [], keys: ['skipped'], secondary_keys: [], order: 0, constant: false },
    numbers: { key: [1, 2.5, -0, 1e21], content: 12, comment: 5, order: 'string order' },
}
json('json-external-entries-object', { name: 'External', entries: external })
json('json-external-entries-array', { entries: Object.values(external), description: 'Array entries' })
json('json-external-entries-integer-keys', { entries: { x: { key: ['x'] }, 10: { key: ['ten'] }, 2: { key: ['two'] }, '01': { key: ['zero one'] } } })
json('json-external-entries-empty', { entries: {} })
json('json-external-key-string', { entries: { a: { key: 'a,b' } } }, { decline: 'externalLorebook' })
json('json-external-keys-object', { entries: { a: { keys: { 0: 'x' } } } }, { decline: 'externalLorebook' })
json('json-external-secondary-string', { entries: { a: { key: ['a'], secondary_keys: 's1' } } }, { decline: 'externalLorebook' })
json('json-external-entry-null', { entries: { a: null } }, { decline: 'externalLorebook' })
json('json-external-entry-array', { entries: [['a']] }, { decline: 'externalLorebook' })
json('json-external-entry-number', { entries: [5] }, { decline: 'externalLorebook' })
json('json-external-entries-string', { entries: 'ab' }, { decline: 'entries' })
json('json-external-to-string', { entries: { a: { key: [{ toString: 1 }] } } }, { decline: 'toPrimitive' })
json('json-regex', { type: 'regex', name: 'Regex list', data: regex })
json('json-regex-data-string', { type: 'regex', data: 'abc' })
json('json-regex-data-empty-array', { type: 'regex', data: [] })
json('json-regex-no-data', { type: 'regex', data: '' })
json('json-unknown', { type: 'something', data: [1] })
json('json-array', [{ type: 'risuModule', name: 'x', id: 'y' }], { decline: 'data' })
json('json-string', 'risuModule', { decline: 'data' })
json('json-null', null, { decline: 'data' })
json('json-proto-key', { type: 'risuModule', name: 'x', id: 'y', extra: protoObject }, { decline: 'protoKey' })

// Prepared RISUM content: the module the native job read, with asset keys blanked.
const blankAssets = [['asset one', '', 'png'], ['asset two', '', 'webp', 'extra slot']]
risum('risum-full', { ...fullModule, assets: blankAssets }, ['assets/aaaa.png', 'assets/bbbb.webp'])
risum('risum-no-assets', { name: 'No assets', description: 'd', id: 'old' })
risum('risum-no-id', { name: 'No id', assets: [['a', '', 'png']] }, ['assets/cccc.png'])
risum('risum-id-first', { id: 'first', name: 'Id first', assets: [] })
risum('risum-duplicate-logical-ids', { name: 'Duplicates', assets: [['a', '', 'png'], ['b', '', 'png']] }, ['assets/dddd.png', 'assets/dddd.png'])
risum('risum-assets-count-mismatch', { name: 'Mismatch', assets: [['a', '', 'png']] }, [])
risum('risum-assets-object', { name: 'Object assets', assets: {} })
risum('risum-assets-null', { name: 'Null assets', assets: null })
risum('risum-tuple-short', { name: 'Short', assets: [['a', '']] }, ['assets/eeee.png'])
risum('risum-tuple-object', { name: 'Object tuple', assets: [{ 0: 'a', 1: '', 2: 'png' }] }, ['assets/ffff.png'])
risum('risum-stdio', { name: 'Stdio', mcp: { url: 'stdio:x' }, assets: [['a', '', 'png']] }, ['assets/aaaa.png'])
risum('risum-stdio-before-assets', { name: 'Stdio', mcp: { url: 'stdio:x' }, assets: {} })
risum('risum-low-level', { name: 'Low level', lowLevelAccess: 1 }, [], { decline: 'lowLevelAccess' })
risum('risum-unicode', { name: '모듈 🐿️', description: '\ud800 lone', assets: [['이름 😀', '', 'png']] }, ['assets/9999.png'])
risum('risum-proto-key', { name: 'Proto', extra: protoObject }, [], { decline: 'protoKey' })

// Prepared charx content imported as a module: the card the native job read, its staged assets and module.
const v3 = (data, extensions = {}) => ({ spec: 'chara_card_v3', spec_version: '3.0', data: { ...data, extensions } })
const asset = (token, logicalId = `stored-${token}`) => ({ token, logicalId })
const book = { name: 'Book', scan_depth: 4, token_budget: 900, recursive_scanning: false, extensions: {}, entries: [{ keys: ['card'], content: 'card lore', extensions: {}, enabled: true, insertion_order: 1 }] }
charx('charx-full', v3({
    name: 'Charx Module', description: 'Character description', first_mes: 'Hello', alternate_greetings: ['Alt one', 'Alt two'],
    post_history_instructions: 'Replace the note', creator_notes: 'Creator notes', character_book: book,
    assets: [
        { type: 'icon', uri: 'ccdefault:', name: 'main', ext: 'png' },
        { type: 'x-risu-asset', uri: '__asset:a1', name: 'background', ext: 'webp' },
        { type: 'x-risu-asset', uri: 'embeded://assets/other/x.png', name: 'other', ext: 'png' },
        { type: 'emotion', uri: '__asset:e1', name: 'smile', ext: 'png' },
    ],
}, { risuai: { customScripts: regex, triggerscript: trigger, hideChatIcon: true, moduleNamespace: 'charx-ns', toggles: 'a=1', backgroundHTML: '<b>bg</b>' } }), {
    assets: [asset('a1'), asset('assets/other/x.png'), asset('e1')], portraitLogicalId: 'charx-portrait',
    module: { trigger, regex: [], lorebook: [lore({ comment: 'module lore' })] },
})
charx('charx-minimal', v3({ name: 'Minimal' }))
charx('charx-greetings-only', v3({ name: 'Greetings', first_mes: '', alternate_greetings: ['only alt', ''] }))
charx('charx-first-message-only', v3({ name: 'First', first_mes: 'just first' }), { portraitLogicalId: 'p' })
charx('charx-description-number', v3({ name: 'Number', description: 5, post_history_instructions: 0 }))
charx('charx-name-object', v3({ name: { first: 'a' }, description: 'd' }))
charx('charx-greeting-values', v3({ name: 'Values', first_mes: 'f', alternate_greetings: [null, 3, ['a', 'b'], { x: 1 }] }))
charx('charx-alternate-greetings-string', v3({ name: 'Alt string', first_mes: 'f', alternate_greetings: 'abc' }), { decline: 'alternateGreetings' })
charx('charx-low-level', v3({ name: 'Low level' }, { risuai: { lowLevelAccess: true } }), { decline: 'lowLevelAccess' })
charx('charx-unicode', v3({ name: '캐릭터 🐿️', description: '설명 \ud800', first_mes: '안녕 😀' }))

// Stored modules turned into the character `exportModule` writes.
const indicatorModule = {
    name: 'Indicators', description: 'Indicator module', id: 'm1',
    lorebook: [
        lore({ comment: 'kept first' }),
        lore({ content: '@@indicator replace_global_note\n\n  Global note replacement  ' }),
        lore({ content: '@@indicator character_desc\n\nThe description\n' }),
        lore({ content: '@@indicator character_first_message\n\n<FM>\nFirst message\n</FM>\n<FM_alt>\nAlt one\n</FM_alt>\n<FM_alt>\n  Alt two  \n</FM_alt>' }),
        lore({ comment: 'kept last', content: 'plain @@indicator character_desc' }),
    ],
    regex, trigger, lowLevelAccess: true, hideIcon: true, backgroundEmbedding: '<i>bg</i>', assets: [['a', 'stored-a', 'png']],
    namespace: 'ns', customModuleToggle: 'toggle=1', icon: 'stored-icon',
}
toCharacter('to-character-indicators', indicatorModule)
toCharacter('to-character-minimal', { name: 'Minimal', description: 'd', id: 'x' })
toCharacter('to-character-empty', {})
toCharacter('to-character-toggle-binding', { name: 'Toggles' }, { root: { defaultToggleValues: { b: '1', 10: 'ten', a: '' } } })
toCharacter('to-character-toggle-disabled', { name: 'Toggles' }, { root: { disableToggleBinding: true, defaultToggleValues: { a: '1' } } })
toCharacter('to-character-toggle-null', { name: 'Toggles' }, { root: { defaultToggleValues: null } })
toCharacter('to-character-toggle-string', { name: 'Toggles' }, { root: { defaultToggleValues: 'ab' }, decline: 'defaultToggleValues' })
toCharacter('to-character-phi', { lorebook: [lore({ content: '@@indicator phiXYZ 　' }), lore({ content: '@@indicator phi' })] })
toCharacter('to-character-indicator-order', { lorebook: [
    lore({ content: '@@indicator character_desc first' }), lore({ content: '@@indicator character_desc second﻿' }),
    lore({ content: '@@indicator character_first_message <FM>one</FM>' }), lore({ content: '@@indicator character_first_message <FM_alt>only alt</FM_alt>' }),
] })
toCharacter('to-character-first-message-forms', { lorebook: [lore({ content: '@@indicator character_first_message\n<FM>a<FM>b</FM>c</FM><FM_alt>x</FM_alt><FM_alt>y<FM_alt>z</FM_alt><FM_alt>open' })] })
toCharacter('to-character-first-message-unclosed', { lorebook: [lore({ content: '@@indicator character_first_message <FM>never closed' })] })
toCharacter('to-character-falsy-fields', { name: null, description: 0, regex: null, trigger: '', lowLevelAccess: 0, hideIcon: '', backgroundEmbedding: 0, assets: null, namespace: null, customModuleToggle: null, icon: 0 })
toCharacter('to-character-unicode', { name: '모듈 🐿️ \ud800', lorebook: [lore({ content: '@@indicator character_desc 설명 😀 \udfff' })] })
toCharacter('to-character-mcp', { name: 'Mcp', mcp: { url: 'https://example.invalid/mcp' } })
toCharacter('to-character-lore-content-number', { lorebook: [lore({ content: 5 })] }, { decline: 'lorebook' })
toCharacter('to-character-lore-null', { lorebook: [null] }, { decline: 'lorebook' })
toCharacter('to-character-lorebook-object', { lorebook: { 0: lore() } }, { decline: 'lorebook' })
toCharacter('to-character-proto-key', { name: 'x', extra: protoObject }, { decline: 'protoKey' })

// Stored modules written as RISUM metadata.
risumExport('risum-export-full', { ...fullModule, assets: [['asset one', 'stored-a1', 'png'], ['asset two', 'stored-a2', 'webp', 'extra']] })
risumExport('risum-export-no-assets', { name: 'No assets', id: 'x', description: 'd' })
risumExport('risum-export-assets-null', { name: 'Null assets', assets: null, id: 'x' })
risumExport('risum-export-short-tuples', { name: 'Short', assets: [['only'], []] }, { decline: 'assets' })
risumExport('risum-export-number-key', { name: 'Number key', assets: [['a', 7, 'png']] }, { decline: 'assets' })
risumExport('risum-export-key-order', { id: 'first', assets: [['a', 'k', 'png']], name: 'Order', 3: 'index' })
risumExport('risum-export-unicode', { name: '모듈 🐿️ \ud800', assets: [['이름', 'stored', 'png']] })
risumExport('risum-export-asset-string', { name: 'String asset', assets: ['abc'] }, { decline: 'assets' })
risumExport('risum-export-assets-object', { name: 'Object assets', assets: { 0: ['a', 'b', 'c'] } }, { decline: 'assets' })

// Stored modules exported as CharX through the character they become.
moduleCharx('module-charx-indicators', indicatorModule)
moduleCharx('module-charx-no-icon', { name: 'No icon', description: 'Uses none.webp', id: 'm2', lorebook: [lore()], assets: [['bg', 'stored-bg', 'webp'], ['remote', 'https://example.invalid/r.png', 'png']] })
moduleCharx('module-charx-minimal', { name: 'Minimal', id: 'm3', icon: 'stored-icon' })
moduleCharx('module-charx-toggle-binding', { name: 'Toggles', id: 'm4', icon: 'i', regex, trigger }, { root: { defaultToggleValues: { a: '1' } } })

writeFileSync(`${dir}modules.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
console.log(`wrote ${cases.length} cases`)
