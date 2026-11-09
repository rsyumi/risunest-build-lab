// Writes the synthetic case file of the card mapping oracle (tests/fixtures/rust-core/formats/cards.cases.json). Add
// cases here, run `node benchmarks/rust-core-oracles/formats-cards.cases.mjs`, then regenerate the expected file with
// RUST_CORE_ORACLE_WRITE=1.
//
// `import` runs `mapPreparedNativeCharacterCard({ card, assets, portraitLogicalId, module })` with only the keys a case
// sets. `export` runs `createBaseV3(character)` with `Date.now()` answering `now`, `export-v2` runs `createBaseV2` through
// the JSON branch of `exportCharacterCard(character, 'json', { spec: 'v2' })`, and `charx` runs the CharX projection
// closure `exportChar` hands the native route. `roundtrip` imports `card` and exports the character it returns. `chain`
// exports `character` and imports the card as its container hands it back (`via`: `png-v3`, `png-v2` or `charx`).
// `decline` names the `Unsupported` the Rust port must answer instead of a result; a case TS throws on without one
// must be an `Invalid` error in Rust.
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
const imported = (id, card, extra = {}) => add({ id, kind: 'import', card, ...extra })
const exported = (id, character, extra = {}) => add({ id, kind: 'export', character, now: 1767225600123, ...extra })
const roundtrip = (id, card, extra = {}) => add({ id, kind: 'roundtrip', card, now: 1767225600999, ...extra })
const exportedV2 = (id, character, extra = {}) => add({ id, kind: 'export-v2', character, ...extra })
const charx = (id, character, extra = {}) => add({ id, kind: 'charx', character, now: 1767225601234, ...extra })
const chain = (id, via, character, extra = {}) => add({ id, kind: 'chain', via, character, now: 1767225601777, ...extra })
const asset = (token, logicalId = `stored-${token}`) => ({ token, logicalId })
// An object with an own `__proto__` key, which only JSON.parse creates.
const protoObject = JSON.parse('{"__proto__": {"polluted": true}, "plain": 1}')

const v3 = (data, extensions = {}) => ({ spec: 'chara_card_v3', spec_version: '3.0', data: { ...data, extensions } })
const v2 = (data, extensions = {}) => ({ spec: 'chara_card_v2', spec_version: '2.0', data: { ...data, extensions } })
const base = { name: 'Synthetic Card', description: 'A synthetic description.', first_mes: 'Hello there.' }

const onboarding = {
    enabled: true, mode: 'form', title: 'Welcome', description: 'Pick one', html: 7, floatingButton: 'yes',
    questions: [
        { id: 'q-kept', title: 'Color', type: 'multiple', varName: 'color', required: true, placeholder: 'any',
            options: [{ label: 'Red', value: 'red' }, null, 5, { label: 3, value: 'blue' }, ['array option']] },
        { id: '', title: 'Name', type: 'text' },
        null,
        'skipped string',
        { title: 'No id', type: 'radio', required: 'true', options: 'not an array' },
        ['array question'],
    ],
}
const risuaiFull = {
    bias: [['token', 5], ['other', -3]], viewScreen: 'emotion', customScripts: [{ comment: 'regex', in: 'a', out: 'b', type: 'editinput' }],
    utilityBot: true, sdData: [['always', 'custom']], backgroundHTML: '<div>bg</div>', license: 'CC0', triggerscript: [{ comment: 'trigger', type: 'start', conditions: [], effect: [] }],
    private: true, additionalText: 'extra', largePortrait: false, lorePlus: true, inlayViewScreen: true,
    newGenData: { prompt: 'p', negative: 'n', instructions: 'i', emotionInstructions: 'e' }, source: ['risu-source'],
    defaultVariables: 'a=1', prebuiltAssetCommand: 'cmd', prebuiltAssetExclude: ['x'], prebuiltAssetStyle: 'style',
    toggles: 'toggle=1', moduleNamespace: 'ns', hideChatIcon: true, onboarding, lowLevelAccess: false,
}
const fullData = {
    ...base, personality: 'Calm', scenario: 'A quiet room', mes_example: '<START>\n{{user}}: hi', creator_notes: 'notes',
    system_prompt: 'system', post_history_instructions: 'post history', alternate_greetings: ['Alt one', 'Alt two'], tags: ['tag-a', 'tag-b'],
    creator: 'Synthetic Creator', character_version: '1.2', nickname: 'Synth', source: ['card-source'], creation_date: 1700000000,
    modification_date: 1700000500, group_only_greetings: ['group'],
}

// Lore entries covering each SillyTavern extension rewrite.
const entry = (overrides = {}) => ({ keys: ['alpha', 'beta'], content: 'Lore content.', extensions: {}, enabled: true, insertion_order: 100, ...overrides })
const decoratedEntries = [
    entry({ name: 'probability', extensions: { useProbability: true, probability: 40 } }),
    entry({ comment: 'probability 100', extensions: { useProbability: true, probability: 100 } }),
    entry({ extensions: { useProbability: true, probability: null } }),
    entry({ extensions: { useProbability: false, probability: 10 } }),
    entry({ extensions: { position: 4, depth: 3, role: 1, other: 'kept' } }),
    entry({ extensions: { position: 4, depth: 2.5, role: 5 } }),
    entry({ extensions: { position: 4, depth: '3', role: 1 } }),
    entry({ extensions: { position: 1, depth: 3, role: 0 } }),
    entry({ secondary_keys: ['s1', 's2'], selective: true, extensions: { selectiveLogic: 0 } }),
    entry({ secondary_keys: ['s1', null, 7], selective: true, extensions: { selectiveLogic: 1 } }),
    entry({ secondary_keys: ['x1', 'x2'], selective: true, extensions: { selectiveLogic: 2 } }),
    entry({ secondary_keys: ['a1', { nested: true }, ['p', 'q']], extensions: { selectiveLogic: 3 } }),
    entry({ secondary_keys: [], selective: true, extensions: { selectiveLogic: 1 } }),
    entry({ secondary_keys: ['k'], extensions: { selectiveLogic: 9 } }),
    entry({ extensions: { delay: 2 } }),
    entry({ extensions: { delay: 0 } }),
    entry({ extensions: { match_whole_words: true } }),
    entry({ extensions: { match_whole_words: false } }),
    entry({ secondary_keys: ['z'], extensions: { useProbability: true, probability: 25, position: 4, depth: 1, role: 2, selectiveLogic: 2, delay: 1, match_whole_words: true, risu_activationPercent: 50, risu_loreCache: { key: 'k', data: ['d'] } } }),
    entry({ keys: ['/regex/i', 'b'], use_regex: true }),
    entry({ keys: ['plain'], use_regex: true }),
    entry({ keys: [], use_regex: true }),
    entry({ keys: [null, 'b'], use_regex: 1 }),
    entry({ keys: ['a', null, 5, true, ['x', 'y']] }),
    entry({ name: null, comment: 'comment only', mode: 'folder', constant: true, case_sensitive: true, folder: 'f1', extensions: { risu_activationPercent: 0 } }),
    { keys: ['no extensions'], content: 'c' },
    { keys: ['no content'], extensions: { delay: 4 } },
    { keys: ['numeric content'], content: 12, extensions: { match_whole_words: false } },
    { keys: ['no content, no prefix'], extensions: null },
]
const book = (entries, extra = {}) => ({ name: 'Book', scan_depth: 5, token_budget: 1024, recursive_scanning: true, extensions: { risu_fullWordMatching: true, custom: 'book ext' }, entries, ...extra })

// CCv3 cards.
imported('v3-minimal', v3(base), { portraitLogicalId: 'portrait-key' })
imported('v3-minimal-no-portrait', v3({ name: 'Bare' }))
imported('v3-full', v3({
    ...fullData,
    character_book: book(decoratedEntries),
    assets: [
        { type: 'icon', uri: 'ccdefault:', name: 'main', ext: 'png' },
        { type: 'emotion', uri: '__asset:e1', name: 'smile', ext: 'png' },
        { type: 'x-risu-asset', uri: 'embeded://assets/other/x.webp', name: 'bg', ext: 'webp' },
        { type: 'background', uri: '__asset:b1', name: 'bgimg', ext: 'jpg' },
        { type: 'other', uri: 'https://example.invalid/remote.png', name: 'remote' },
        { type: 'x-risu-asset', uri: '__asset:noext', name: '' },
        { uri: '__asset:notype', name: 'untyped' },
    ],
}, { risuai: risuaiFull, depth_prompt: { depth: 4, prompt: 'depth prompt' }, custom_ext: { a: 1 }, 5: 'index key', zeta: null }), {
    portraitLogicalId: 'portrait-key',
    assets: [asset('e1'), asset('assets/other/x.webp'), asset('b1'), asset('noext'), asset('notype')],
})
imported('v3-icon-replaces-portrait', v3({
    ...base,
    assets: [
        { type: 'icon', uri: '__asset:icon', name: 'main', ext: 'png' },
        { type: 'emotion', uri: 'ccdefault:', name: 'uses icon' },
        { type: 'icon', uri: '__asset:icon2', name: 'secondary', ext: 'png' },
    ],
}), { portraitLogicalId: 'portrait-key', assets: [asset('icon'), asset('icon2')] })
imported('v3-unknown-schemes-skipped', v3({ ...base, assets: [{ type: 'emotion', uri: '', name: 'empty' }, { type: 'emotion', uri: 'file:///x', name: 'file' }] }))
imported('v3-wrong-types', v3({
    name: 42, description: null, first_mes: ['array greeting'], tags: 'tag-string', alternate_greetings: null, creator: false,
    character_version: 3, nickname: null, source: null, creation_date: null, personality: 0, scenario: '',
}, { risuai: { bias: null, viewScreen: 0, utilityBot: null, lowLevelAccess: 0, largePortrait: null, toggles: null, onboarding: [], private: '' } }))
imported('v3-character-version-object', v3({ ...base, character_version: { major: 1 } }))
imported('v3-character-version-array', v3({ ...base, character_version: [1, null, [2, 3]] }))
imported('v3-character-version-to-string', v3({ ...base, character_version: { toString: 'not callable' } }), { decline: 'toPrimitive' })
imported('v3-unicode', v3({
    name: '합성 캐릭터 🐿️', description: 'line one\nline two   tab\t "quotes" \\ backslash', first_mes: 'lone \ud800 surrogate',
    character_book: book([entry({ keys: ['한글', '　full width　'], content: '내용 😀', name: '이름' })]),
}, { risuai: { additionalText: '추가 \udfff 텍스트' } }))
imported('v3-low-level-access', v3(base, { risuai: { lowLevelAccess: true } }), { decline: 'lowLevelAccess' })
imported('v3-data-uri', v3({ ...base, assets: [{ type: 'emotion', uri: 'data:image/png;base64,AAAA', name: 'inline' }] }))
imported('v3-missing-asset', v3({ ...base, assets: [{ type: 'emotion', uri: '__asset:absent', name: 'x' }] }), { assets: [asset('present')] })
// The preflight runs before the module is read, so its error wins over a module the port declines.
imported('v3-data-uri-before-module', v3({ ...base, assets: [{ type: 'emotion', uri: 'data:image/png;base64,AAAA', name: 'inline' }] }), { module: { trigger: 'abc' } })
imported('v3-empty-asset-token', v3({ ...base, assets: [{ type: 'emotion', uri: 'embeded://', name: 'x' }] }))
imported('v3-ccdefault-without-portrait', v3({ ...base, assets: [{ type: 'icon', uri: 'ccdefault:', name: 'main' }] }))
imported('v3-asset-uri-number', v3({ ...base, assets: [{ type: 'emotion', uri: 5, name: 'x' }] }), { decline: 'asset' })
imported('v3-asset-null', v3({ ...base, assets: [null] }), { decline: 'asset' })
imported('v3-assets-object', v3({ ...base, assets: { 0: { uri: '__asset:a' } } }), { decline: 'assets' })
imported('v3-no-extensions', { spec: 'chara_card_v3', spec_version: '3.0', data: { ...base } }, { decline: 'extensions' })
imported('v3-data-string', { spec: 'chara_card_v3', spec_version: '3.0', data: 'string data' }, { decline: 'cardData' })
imported('v3-risuai-string', v3(base, { risuai: 'risu' }), { decline: 'risuai' })
imported('v3-risuai-null', v3(base, { risuai: null, depth_prompt: { depth: 1, prompt: 'p' } }))
imported('v3-proto-key', v3(base, { custom: protoObject }), { decline: 'protoKey' })

// The asset dictionary and portrait checks.
imported('assets-duplicate-token', v3(base), { assets: [asset('a'), asset('a', 'other')] })
imported('assets-blank-token', v3(base), { assets: [asset('  ')] })
imported('assets-blank-logical-id', v3(base), { assets: [asset('a', '\t')] })
imported('portrait-blank', v3(base), { portraitLogicalId: '   ' })

// Lore books.
imported('book-settings-partial', v3({ ...base, character_book: { scan_depth: 3, entries: [entry()], extensions: { risu_fullWordMatching: true } } }))
imported('book-settings-null-extensions', v3({ ...base, character_book: { scan_depth: 0, token_budget: 0, recursive_scanning: false, entries: [] } }))
imported('book-extensions-string', v3({ ...base, character_book: { scan_depth: 1, token_budget: 2, recursive_scanning: true, extensions: 'text', entries: [] } }))
imported('book-large', v3({ ...base, character_book: book(Array.from({ length: 400 }, (_, index) => entry({
    keys: [`key-${index}`, `alt-${index % 7}`], content: `Entry ${index} ${'body '.repeat(index % 5)}`, insertion_order: index,
    secondary_keys: index % 3 === 0 ? [`sec-${index}`] : undefined, selective: index % 3 === 0,
    extensions: index % 4 === 0 ? { useProbability: true, probability: index % 100 } : index % 4 === 1 ? { delay: index % 9 } : {},
}))) }))
imported('book-falsy', v3({ ...base, character_book: 0 }))
imported('book-string', v3({ ...base, character_book: 'book' }), { decline: 'characterBook' })
imported('book-entries-missing', v3({ ...base, character_book: { scan_depth: 1 } }), { decline: 'characterBook' })
imported('book-entry-null', v3({ ...base, character_book: book([null]) }), { decline: 'lorebookEntry' })
imported('book-entry-keys-string', v3({ ...base, character_book: book([entry({ keys: 'a,b' })]) }), { decline: 'lorebookEntry' })
imported('book-entry-secondary-string', v3({ ...base, character_book: book([entry({ secondary_keys: 's1' })]) }), { decline: 'lorebookEntry' })
imported('book-entry-extensions-array', v3({ ...base, character_book: book([entry({ extensions: ['x'] })]) }), { decline: 'lorebookEntry' })
imported('book-entry-regex-number-key', v3({ ...base, character_book: book([entry({ keys: [5], use_regex: true })]) }), { decline: 'lorebookEntry' })
imported('book-entry-probability-to-string', v3({ ...base, character_book: book([entry({ extensions: { useProbability: true, probability: { toString: 1 } } })]) }), { decline: 'toPrimitive' })

// Charx modules.
const module = {
    trigger: [{ comment: 'module trigger', type: 'manual', conditions: [], effect: [] }],
    regex: [{ comment: 'module regex', in: 'x', out: 'y', type: 'editoutput' }],
    lorebook: [{ key: 'module key', secondkey: '', insertorder: 1, comment: 'module lore', content: 'module content', mode: 'normal', alwaysActive: false, selective: false }],
}
imported('module-replaces-scripts-and-lore', v3({ ...base, character_book: book([entry()]) }, { risuai: { customScripts: [{ comment: 'card regex' }], triggerscript: [{ comment: 'card trigger' }], viewScreen: 'imggen' } }), { module })
imported('module-without-lorebook', v3({ ...base, character_book: book([entry()]) }, { other: 1 }), { module: { trigger: null } })
imported('module-creates-risuai', v3(base, { first: 1 }), { module: {} })
imported('module-lorebook-null', v3(base), { module: { lorebook: null }, decline: 'module' })
imported('module-trigger-string', v3(base), { module: { trigger: 'abc' }, decline: 'module' })
imported('module-falsy', v3(base), { module: false })

// CCv2 cards.
imported('v2-full', v2({
    ...fullData, character_version: '2.0.1',
    character_book: book(decoratedEntries.slice(0, 8)),
}, {
    risuai: {
        ...risuaiFull,
        emotions: [['happy', '__asset:em1'], [{ label: 'object label' }, '__asset:em2', 'extra']],
        additionalAssets: [['bg', '__asset:ad1', 'bg.png'], ['noext', '__asset:ad2'], ['numeric', '__asset:ad1', 9]],
        vits: { 'model.onnx': '__asset:v1', 'config.json': '__asset:v2', 3: '__asset:v1' },
    },
    depth_prompt: { depth: 2, prompt: 'v2 depth' },
    sillytavern: { flag: true },
}), { assets: [asset('em1'), asset('em2'), asset('ad1'), asset('ad2'), asset('v1'), asset('v2')], portraitLogicalId: 'v2-portrait' })
imported('v2-no-risuai', v2({ ...base, character_version: 7 }))
imported('v2-empty-vits', v2(base, { risuai: { vits: {}, emotions: [], additionalAssets: [] } }))
imported('v2-emotion-inline', v2(base, { risuai: { emotions: [['happy', 'iVBORw0KGgo=']] } }), { assets: [asset('x')] })
imported('v2-emotion-missing-token', v2(base, { risuai: { emotions: [['happy', '__asset:']] } }))
imported('v2-missing-token-before-module', v2(base, { risuai: { emotions: [['happy', '__asset:absent']] } }), { module: { trigger: 'abc' } })
imported('v2-emotion-number', v2(base, { risuai: { emotions: [['happy', 5]] } }), { decline: 'emotions' })
imported('v2-emotion-short', v2(base, { risuai: { emotions: [['happy']] } }), { decline: 'emotions' })
imported('v2-additional-object', v2(base, { risuai: { additionalAssets: { a: 1 } } }), { decline: 'additionalAssets' })
imported('v2-vits-inline', v2(base, { risuai: { vits: { 'model.onnx': 'AAAA' } } }))
imported('v2-vits-number', v2(base, { risuai: { vits: { 'model.onnx': 5 } } }), { decline: 'vits' })
imported('v2-vits-array', v2(base, { risuai: { vits: ['__asset:a'] } }), { assets: [asset('a')], decline: 'vits' })
imported('v2-module-scripts', v2(base, { risuai: { emotions: [['e', '__asset:em']] } }), { assets: [asset('em')], module: { trigger: [{ comment: 't' }], regex: [] } })
imported('v2-low-level-access', v2(base, { risuai: { lowLevelAccess: 'yes' } }), { decline: 'lowLevelAccess' })
imported('v2-no-extensions', { spec: 'chara_card_v2', spec_version: '2.0', data: { ...base } }, { decline: 'extensions' })

// Cards without a CCv2 or CCv3 spec.
const tavern = { name: 'Old Tavern', description: 'old desc', personality: 'old personality', scenario: 'old scenario', first_mes: 'old greeting',
    mes_example: 'old example', avatar: 'none', chat: 'chat', create_date: '2023-1-1', talkativeness: '0.5' }
imported('off-spec-tavern', tavern, { portraitLogicalId: 'tavern-portrait', assets: [asset('ignored')] })
imported('off-spec-empty', {})
imported('off-spec-v1', { spec: 'chara_card_v1', name: 'V1', first_mes: null, mes_example: null }, { module })
imported('off-spec-v2-data', { spec_version: '2.0', data: { ...fullData, character_book: book(decoratedEntries.slice(8, 14)) } })
imported('off-spec-v2-no-book', { spec_version: '2.0', data: { name: 'Data only', system_prompt: null, post_history_instructions: 'post' } })
imported('off-spec-v2-data-string', { spec_version: '2.0', data: 'x' }, { decline: 'cardData' })
imported('off-spec-v2-book-string', { spec_version: '2.0', data: { character_book: 'x' } }, { decline: 'characterBook' })

// Stored characters exported as CCv3.
const lore = (overrides = {}) => ({ key: 'alpha, beta ,gamma', secondkey: '', insertorder: 100, comment: 'Lore', content: 'Lore content', mode: 'normal', alwaysActive: false, selective: false, ...overrides })
const character = {
    name: 'Stored Character', desc: 'stored desc', personality: 'p', scenario: 's', firstMessage: 'first', exampleMessage: 'example',
    creatorNotes: 'creator notes', systemPrompt: 'system', replaceGlobalNote: 'replace note', alternateGreetings: ['alt'],
    globalLore: [
        lore(),
        lore({ selective: true, secondkey: ' s1,s2 , ', extentions: { risu_case_sensitive: true, custom: 1 }, activationPercent: 30, loreCache: { key: 'k', data: ['d'] }, useRegex: true, folder: 'folder-1' }),
        lore({ key: '', mode: undefined, comment: undefined, extentions: null }),
        lore({ key: '　wide　, nbsp', content: 12 }),
    ],
    loreSettings: { tokenBudget: 800, scanDepth: 4, recursiveScanning: true, fullWordMatching: true },
    loreExt: { existing: 'ext', risu_fullWordMatching: 'replaced' },
    tags: ['t1'], additionalData: { tag: ['t1'], creator: 'Creator', character_version: '3.1' },
    bias: [['b', 1]], viewScreen: 'none', customscript: [{ comment: 'regex' }], utilityBot: false, sdData: [['always', 'x']],
    backgroundHTML: '<b>bg</b>', license: 'CC-BY', triggerscript: [{ comment: 'trigger' }], additionalText: 'more', largePortrait: true,
    lorePlus: false, inlayViewScreen: false, newGenData: { prompt: 'np' }, lowLevelAccess: false, defaultVariables: 'v=1',
    prebuiltAssetCommand: 'c', prebuiltAssetExclude: ['e'], prebuiltAssetStyle: 'st', customModuleToggle: 'toggle', moduleNamespace: 'mns',
    hideChatIcon: false, onboarding: { enabled: true, mode: 'form', questions: [] }, depth_prompt: { depth: 3, prompt: 'dp' },
    nickname: 'nick', source: ['src'], creation_date: 1700000001, modification_date: 1,
    ccAssets: [{ type: 'background', uri: 'stored-bg', name: 'bg', ext: 'jpg' }],
    additionalAssets: [['asset one', 'stored-a1', 'webp'], ['asset two', 'stored-a2'], ['asset three', 'stored-a3', '']],
    emotionImages: [['joy', 'stored-joy'], ['sad', 'stored-sad']],
    extentions: { risuai: { ignored: true }, depth_prompt: 'ignored', custom: { nested: [1, 2] }, 3: 'index', sillytavern: null },
    chats: [{ message: [{ role: 'user', data: 'not exported' }] }], chaId: 'stored-id', image: 'stored-portrait',
}
exported('export-full', character)
exported('export-minimal', { name: 'Minimal', globalLore: [] })
exported('export-empty-emotions', { name: 'Emotions', globalLore: [], emotionImages: [], additionalAssets: [] })
exported('export-wrong-types', { name: 7, desc: null, tags: 'tags', globalLore: [lore({ selective: 1, secondkey: 'one,two' })], loreSettings: 'none', additionalData: 'none', loreExt: null, extentions: 0, ccAssets: null })
exported('export-version-number', { name: 'v', globalLore: [], additionalData: { character_version: 2.5, creator: null } })
exported('export-unicode', { name: '내보내기 🐿️', desc: 'lone \udc00 low', globalLore: [lore({ key: '키, 열쇠 ,🔑', content: '내용' })] })
exported('export-large', { name: 'Large', globalLore: Array.from({ length: 300 }, (_, index) => lore({ key: `k${index}, j${index % 11}`, insertorder: index, selective: index % 2 === 0, secondkey: `s${index}`, content: `c${index}` })) })
exported('export-no-global-lore', { name: 'No lore' }, { decline: 'globalLore' })
exported('export-lore-key-number', { name: 'x', globalLore: [lore({ key: 5 })] }, { decline: 'globalLore' })
exported('export-lore-secondkey-missing', { name: 'x', globalLore: [lore({ selective: true, secondkey: undefined })] }, { decline: 'globalLore' })
exported('export-lore-extentions-string', { name: 'x', globalLore: [lore({ extentions: 'x' })] }, { decline: 'globalLore' })
exported('export-lore-null', { name: 'x', globalLore: [null] }, { decline: 'globalLore' })
exported('export-lore-ext-string', { name: 'x', globalLore: [], loreExt: 'x' }, { decline: 'loreExt' })
exported('export-extentions-string', { name: 'x', globalLore: [], extentions: 'ab' }, { decline: 'extentions' })
exported('export-cc-assets-object', { name: 'x', globalLore: [], ccAssets: {} }, { decline: 'ccAssets' })
exported('export-additional-string', { name: 'x', globalLore: [], additionalAssets: ['ab'] }, { decline: 'additionalAssets' })
exported('export-emotions-object', { name: 'x', globalLore: [], emotionImages: { a: 1 } }, { decline: 'emotionImages' })
exported('export-proto-key', { name: 'x', globalLore: [], extentions: protoObject }, { decline: 'protoKey' })
exported('export-version-to-string', { name: 'x', globalLore: [], additionalData: { character_version: { toString: [] } } }, { decline: 'toPrimitive' })

// Imported cards exported again.
roundtrip('roundtrip-v3-full', cases.find(entry => entry.id === 'v3-full').card, { portraitLogicalId: 'portrait-key', assets: cases.find(entry => entry.id === 'v3-full').assets })
roundtrip('roundtrip-v2-full', cases.find(entry => entry.id === 'v2-full').card, { portraitLogicalId: 'v2-portrait', assets: cases.find(entry => entry.id === 'v2-full').assets })
roundtrip('roundtrip-off-spec', tavern)

// Stored characters exported as CCv2. `createBaseV2` never reads the asset lists, so shapes CCv3 declines export here.
exportedV2('export-v2-full', character)
exportedV2('export-v2-minimal', { name: 'Minimal', globalLore: [] })
exportedV2('export-v2-ignores-assets', { name: 'Assets', globalLore: [], ccAssets: {}, emotionImages: { a: 1 }, additionalAssets: ['ab'] })
exportedV2('export-v2-wrong-types', {
    name: 'Wrong', desc: null, tags: 'tags', globalLore: [lore({ selective: 1, secondkey: 'one,two', useRegex: true })], loreSettings: 'none',
    additionalData: 'none', loreExt: null, extentions: 0, lowLevelAccess: true, customModuleToggle: 'x', nickname: 'n', source: ['s'], creation_date: 5,
})
exportedV2('export-v2-version-number', { name: 'v', globalLore: [], additionalData: { character_version: 2.5, creator: null } })
exportedV2('export-v2-unicode', { name: '내보내기 🐿️', desc: 'lone \udc00 low', globalLore: [lore({ key: '키, 열쇠 ,🔑', content: '내용' })] })
exportedV2('export-v2-large', { name: 'Large', globalLore: Array.from({ length: 300 }, (_, index) => lore({ key: `k${index}, j${index % 11}`, insertorder: index, selective: index % 2 === 0, secondkey: `s${index}`, content: `c${index}` })) })
exportedV2('export-v2-no-global-lore', { name: 'No lore' }, { decline: 'globalLore' })
exportedV2('export-v2-lore-key-number', { name: 'x', globalLore: [lore({ key: 5 })] }, { decline: 'globalLore' })
exportedV2('export-v2-lore-ext-string', { name: 'x', globalLore: [], loreExt: 'x' }, { decline: 'loreExt' })
exportedV2('export-v2-extentions-string', { name: 'x', globalLore: [], extentions: 'ab' }, { decline: 'extentions' })
exportedV2('export-v2-proto-key', { name: 'x', globalLore: [], extentions: protoObject }, { decline: 'protoKey' })
exportedV2('export-v2-version-to-string', { name: 'x', globalLore: [], additionalData: { character_version: { toString: [] } } }, { decline: 'toPrimitive' })

// The CharX projection: the CCv3 card without its scripts, and the module that carries them with the stored lore.
charx('charx-full', character)
charx('charx-minimal', { name: 'Minimal', globalLore: [] })
charx('charx-null-scripts', { name: 'Null scripts', globalLore: [lore()], triggerscript: null, customscript: null })
charx('charx-scripts-not-arrays', { name: 'Odd scripts', globalLore: [], triggerscript: 'not an array', customscript: 0 })
charx('charx-name-number', { name: 7, globalLore: [] })
charx('charx-name-missing', { globalLore: [] })
charx('charx-name-object', { name: { first: 'a' }, globalLore: [] })
charx('charx-name-array', { name: ['a', null, ['b', 'c']], globalLore: [] })
charx('charx-name-unicode', { name: '모듈 🐿️ \ud800', globalLore: [] })
charx('charx-name-to-string', { name: { toString: 'x' }, globalLore: [] }, { decline: 'toPrimitive' })
charx('charx-no-global-lore', { name: 'x' }, { decline: 'globalLore' })
charx('charx-proto-key', { name: 'x', globalLore: [], extentions: protoObject }, { decline: 'protoKey' })

// Exported cards fed back into import. A character with only the usual fields exports `toggles: ''`,
// `character_version: 'undefined'`, `vits: {}`, `group_only_greetings: []` and lore without `risu_activationPercent`.
const plainCharacter = {
    name: 'Chain Character', desc: 'Chain description', firstMessage: 'Chain greeting', personality: 'steady',
    globalLore: [lore(), lore({ key: 'second, third', selective: true, secondkey: 'x, y', extentions: { custom: 'kept' }, comment: 'Second lore' })],
    loreSettings: { tokenBudget: 600, scanDepth: 3, recursiveScanning: false },
    emotionImages: [['joy', 'stored-joy']],
    additionalAssets: [['bg', 'stored-bg', 'webp']],
    ccAssets: [{ type: 'background', uri: 'https://example.invalid/remote.png', name: 'remote', ext: 'png' }],
    triggerscript: [{ comment: 'chain trigger', type: 'start', conditions: [], effect: [] }],
    customscript: [{ comment: 'chain regex', in: 'a', out: 'b', type: 'editinput' }],
    tags: ['chain'], viewScreen: 'emotion', utilityBot: false,
}
chain('chain-charx-plain', 'charx', plainCharacter)
chain('chain-charx-full', 'charx', character)
chain('chain-png-v3-plain', 'png-v3', plainCharacter, { portraitLogicalId: 'png-portrait' })
chain('chain-png-v3-full', 'png-v3', character, { portraitLogicalId: 'png-portrait' })
chain('chain-png-v2-plain', 'png-v2', plainCharacter, { portraitLogicalId: 'png-portrait' })
chain('chain-png-v2-full', 'png-v2', character, { portraitLogicalId: 'png-portrait' })
// The PNG export skips the asset URIs `isKnownUri` names, so only stored keys become `chara-ext-asset_` chunks.
const knownUriCharacter = {
    ...plainCharacter, name: 'Known URIs',
    ccAssets: [
        { type: 'emotion', uri: 'ccdefault:x', name: 'prefixed default', ext: 'png' },
        { type: 'icon', uri: 'ccdefault:', name: 'main', ext: 'png' },
        { type: 'background', uri: 'http://example.invalid/a.png', name: 'plain http', ext: 'png' },
        { type: 'background', uri: 'stored-key', name: 'stored', ext: 'png' },
    ],
}
chain('chain-png-v3-known-uris', 'png-v3', knownUriCharacter, { portraitLogicalId: 'png-portrait' })
// An `embeded://` URI stays in the card, which the import then rejects for its missing payload.
chain('chain-png-v3-embedded-uri', 'png-v3', { ...knownUriCharacter, ccAssets: [{ type: 'background', uri: 'embeded://assets/other/x.webp', name: 'embedded', ext: 'webp' }] }, { portraitLogicalId: 'png-portrait', rejected: true })
// Integer-like extension keys: array indices sort ahead of `risuai`, the others keep their place.
const integerKeyCharacter = { ...plainCharacter, name: 'Integer keys', extentions: { custom: 1, '01': 'zero one', 4294967295: 'not an index', 7: 'seven', 4294967294: 'last index' } }
chain('chain-png-v3-integer-keys', 'png-v3', integerKeyCharacter, { portraitLogicalId: 'png-portrait' })
chain('chain-charx-integer-keys', 'charx', integerKeyCharacter)

writeFileSync(`${dir}cards.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
console.log(`wrote ${cases.length} cases`)
