// Writes the synthetic case file of the database preparation oracle (tests/fixtures/rust-core/formats/prepare.cases.json).
// Add cases here, run `node benchmarks/rust-core-oracles/formats-prepare.cases.mjs`, then regenerate the expected file
// with RUST_CORE_ORACLE_WRITE=1.
//
// `defaults-constants` records the defaults the port reads (`normalizeDatabaseDefaults({})` and the constants it and
// `checkNewFormat` use). `defaults` runs `normalizeDatabaseDefaults` on `database`, on the web unless `tauri` is set.
// `new-format` runs `checkNewFormat(database, {now})`, and `persistence` runs `prepareDatabaseForPersistence(database)`
// with `Date.now()` at `now`. `oldPrompt` (`main` or `jailbreak`) sets `mainPrompt` to that old default prompt before
// the run, and `answers` answers the confirmations of a dialog in order. `decline` names the `Unsupported` the Rust port
// must answer instead of a result. `full` and `compat` are the databases of the RisuSave set's `external-full` and
// `external-compat` cases.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
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
const defaults = kind('defaults')
const NOW = 1_700_000_000_000
const DAY = 1000 * 60 * 60 * 24
const timed = name => (id, database, extra = {}) => add({ id, kind: name, database, now: NOW, ...extra })
const newFormat = timed('new-format')
const persistence = timed('persistence')
// An object with an own `__proto__` key, which only JSON.parse creates.
const protoObject = JSON.parse('{"__proto__": {"polluted": true}, "plain": 1}')
const risusave = JSON.parse(readFileSync(`${dir}risusave.cases.json`, 'utf8')).cases
const database = id => structuredClone(risusave.find(entry => entry.id === id).database)
const full = database('external-full')
const compat = database('external-compat')

add({ id: 'defaults-constants', kind: 'defaults-constants' })

// normalizeDatabaseDefaults
defaults('defaults-empty', {})
defaults('defaults-tauri-empty', {}, { tauri: true })
defaults('defaults-tauri-prompt-info', { promptInfoInsideChat: true }, { tauri: true })
defaults('defaults-web-prompt-info', { promptInfoInsideChat: true })
defaults('defaults-main-prompt-old', {}, { oldPrompt: 'main' })
defaults('defaults-nulls', {
    characters: null, apiType: null, botPresets: null, personas: null, hotkeys: null, NAIImgConfig: null, loreBook: null,
    loreBookPage: null, fallbackModels: null, hypaV3Presets: null, translatorPresets: null, chatLoadInitialPages: null,
    risunestInlayWebpQuality: null, customColorScheme: null, google: null, openrouterProvider: null,
    enableRisuaiProTools: null, hypaCustomSettings: null, localNetworkMode: null, top_p: null, globalChatVariables: null,
})
defaults('defaults-full', full)
defaults('defaults-compat', compat)
defaults('defaults-kept-values', {
    apiType: 'openai', temperature: 0, maxContext: 0, jailbreakToggle: true, username: '', userIcon: 'assets/me.png',
    botPresetsId: 2, selectedPersona: 1, colorSchemeName: 'dark', localNetworkMode: true, localNetworkTimeoutSec: 30,
    top_p: 0.5, enableScrollToActiveChar: true, sendWithEnter: false, globalChatVariables: { a: '1' }, plugins: [],
    modules: [{ id: 'm' }], loadouts: [], customModels: [], theme: 'waifu', zoomsize: 0, enableRisuaiProTools: false,
    keepSessionAlive: 'on', echoDelay: 0, reasoningEffort: -1, hypaV3PresetId: 3, botPresets: [], personas: [],
})
defaults('defaults-nai-img-config-legacy', { NAIImgConfig: { width: 640, height: 640, sampler: 'k_euler', autoSmea: true } })
defaults('defaults-nai-img-config-null-v4', { NAIImgConfig: { v4_prompt: null, width: 1, legacy_uc: true } })
defaults('defaults-nai-img-config-current', { NAIImgConfig: { v4_prompt: { use_order: false }, width: 2 } })
defaults('defaults-lorebook-page-reset', { loreBook: [{ name: 'a', data: [] }], loreBookPage: 5 })
defaults('defaults-lorebook-page-kept', { loreBook: [{}, {}], loreBookPage: 2 })
defaults('defaults-lorebook-page-fraction', { loreBook: [], loreBookPage: 0.5 })
defaults('defaults-lorebook-page-missing', { loreBook: [{ name: 'x', data: [] }] })
defaults('defaults-formating-order-main', { formatingOrder: ['description', 'main', 'chats'] })
defaults('defaults-formating-order-no-main', { formatingOrder: ['chats', 'jailbreak', 'lorebook'] })
defaults('defaults-formating-order-empty', { formatingOrder: [] })
defaults('defaults-formating-order-kept', { formatingOrder: ['personaPrompt', 5, null] })
defaults('defaults-persona-default', { username: 'Synthetic', userIcon: 'assets/u.png', userNote: 7 })
defaults('defaults-color-scheme-custom', { colorSchemeName: 'custom', colorScheme: { bgcolor: '#000000', textcolor: '#ffffff' } })
defaults('defaults-color-scheme-named', { colorSchemeName: 'light', colorScheme: { bgcolor: '#ffffff' } })
defaults('defaults-nested-fills', {
    NAIsettings: { topK: 1 }, google: { projectId: 'p' }, promptSettings: { sendName: true },
    customTextTheme: { FontColorStandard: '#fff' }, seperateParameters: { memory: {} },
})
defaults('defaults-nested-null', {
    NAIsettings: { starter: null, cfg_scale: null, mirostat_lr: 3 }, google: { accessToken: null },
    promptSettings: { maxThoughtTagDepth: null }, customTextTheme: { FontColorQuote1: null, FontColorQuote2: 'kept' },
    seperateParameters: { overrides: null },
})
defaults('defaults-type-fixes', { localNetworkMode: 'yes', localNetworkTimeoutSec: '30', top_p: '0.5' })
defaults('defaults-ollama-cloud-main', { aiModel: 'ollama-cloud', ollamaModel: 'llama', ollamaModelName: 'Llama' })
defaults('defaults-ollama-cloud-sub', { subModel: 'ollama-cloud', ollamaModel: 'm' })
defaults('defaults-ollama-cloud-kept', { subModel: 'ollama-cloud', ollamaCloudModel: 'kept', ollamaModelSource: 'local' })
defaults('defaults-ollama-cloud-empty', { aiModel: 'ollama-cloud', ollamaCloudModel: '', ollamaModel: 7 })
defaults('defaults-openrouter-provider', {
    openrouterProvider: 'provider-a',
    promptTemplate: [{ type: 'persona', role2: 'user' }, { type: 'memory', role2: null }, { type: 'jailbreak', role: 'char' }],
    botPresets: [
        {
            name: 'p', openrouterProvider: '', localNetworkMode: 1, localNetworkTimeoutSec: null,
            promptTemplate: [{ type: 'plain', role: 'bot' }, { type: 'cache', role: 'char' }, null, 'x'],
        },
        { name: 'q', openrouterProvider: { order: ['x'] }, localNetworkTimeoutSec: '5', promptTemplate: 'not a list' },
    ],
})
defaults('defaults-presets-falsy', { botPresets: 0 })
defaults('defaults-hypa-v3-default', {
    supaMemoryPrompt: 'Summarize',
    hypaV3Settings: { maxChatsPerSummary: 9, memoryTokensRatio: '0.3', unknownSetting: 1, enableSimilarityCorrection: true },
})
defaults('defaults-hypa-v3-override-prompt', { supaMemoryPrompt: 'S', hypaV3Settings: { summarizationPrompt: 5 } })
defaults('defaults-hypa-v3-string-settings', { supaMemoryPrompt: 'S', hypaV3Settings: 'abc' })
defaults('defaults-hypa-v3-presets', {
    hypaV3Presets: [
        { name: '', settings: { queryChatCount: 5, summarizationModel: 7, summaryChunkSeparator: '|' } },
        { name: 'Named', settings: 'x' }, 'text', [], { settings: null }, { name: 3, settings: [1] },
    ],
})
defaults('defaults-hypa-v3-empty-list', { hypaV3Presets: [] })
defaults('defaults-translator-presets', {
    translatorPresets: [{ name: '  ', prompt: 'p', maxResponse: 5 }, 'x', { name: 'Named', prompt: 3, maxResponse: '7' }, []],
    translatorPresetId: 9,
})
defaults('defaults-translator-fraction', {
    translatorPresets: [{ name: 'a', prompt: 'b', maxResponse: 1 }, { name: 'c', prompt: 'd', maxResponse: 2 }], translatorPresetId: 0.5, translatorPrompt: 'legacy',
})
defaults('defaults-translator-legacy', { translatorPrompt: 'Translate {{slot}}', translatorMaxResponse: 300, translatorPresets: [] })
defaults('defaults-translator-legacy-types', { translatorPrompt: 5, translatorMaxResponse: '300', translatorPresetId: -2, translatorPresets: {} })
defaults('defaults-anti-claude', { antiClaudeOverload: true })
defaults('defaults-anti-claude-truthy', { antiClaudeOverload: 'yes', antiServerOverloads: false })
defaults('defaults-hypa-custom', { hypaCustomSettings: { url: 'http://u', model: null, extra: 1, key: 0 } })
defaults('defaults-hypa-custom-string', { hypaCustomSettings: 'abc' })
defaults('defaults-hotkeys-merge', { hotkeys: [{ key: 'x', action: 'reroll' }, 'text', { action: 5 }, { key: 'y', action: 'scrollToActiveChar' }] })
defaults('defaults-hotkeys-scroll-off', { enableScrollToActiveChar: false, hotkeys: [{ key: 'x', action: 'scrollToActiveChar' }, 7] })
defaults('defaults-hotkeys-falsy', { hotkeys: 0, enableScrollToActiveChar: false })
defaults('defaults-fallback-models', { fallbackModels: { otherAx: [0, null, ''], memory: [''], extra: [], model: ['', 'a', ''], translate: ['t'], emotion: [] } })
defaults('defaults-chat-load-pages', { chatLoadInitialPages: '12.7', chatLoadAdditionalPages: true })
defaults('defaults-chat-load-pages-array', { chatLoadInitialPages: [40], chatLoadAdditionalPages: 0.5 })
defaults('defaults-chat-load-pages-hex', { chatLoadInitialPages: '0x10', chatLoadAdditionalPages: {} })
defaults('defaults-chat-load-pages-space', { chatLoadInitialPages: 99.9, chatLoadAdditionalPages: ' ' })
defaults('defaults-inlay-clamp', {
    risunestInlayFormat: 'png', risunestInlayWebpQuality: 150.4, risunestInlayMaxDimension: 1e12, risunestInlaySkipReencode: false,
    risunestInlayAnimationMaxFps: 2.5,
})
defaults('defaults-inlay-round', {
    risunestInlayFormat: 'gif', risunestInlayWebpQuality: 0.4, risunestInlayMaxDimension: -2.5, risunestInlaySkipReencode: 'no',
    risunestInlayAnimationMaxFps: -0.4,
})
defaults('defaults-inlay-half', { risunestInlayWebpQuality: 50.5, risunestInlayMaxDimension: 2.5, risunestInlayAnimationMaxFps: 239.5 })
defaults('defaults-inlay-strings', {
    risunestInlayFormat: 'original', risunestInlayWebpQuality: '90', risunestInlayMaxDimension: 0.49999999999999994,
    risunestInlayAnimationMaxFps: 1000,
})
defaults('defaults-plugins', { plugins: [{ name: 'p' }] })
defaults('defaults-plugins-kept', { plugins: [{}], enableRisuaiProTools: false })
defaults('defaults-streaming-chats', {
    characters: [
        { chaId: 'a', chats: [{ id: 'c1', isStreaming: true, activeStreamingDisplayOptimizationMode: 'strong', message: [] }, { id: 'c2' }] },
        { chaId: 'b' }, { chaId: 'c', chats: null }, { chaId: 'd', chats: '' }, 5, 'text', [], 0,
    ],
})
defaults('defaults-unicode', {
    username: '사용자 🐿️ \ud800', characters: [{ chaId: '캐릭터', chats: [{ id: '대화' }] }],
    translatorPresets: [{ name: '번역', prompt: '프롬프트', maxResponse: 10 }], globalChatVariables: { 변수: '값' },
})
defaults('defaults-not-object', null, { decline: 'database' })
defaults('defaults-array', [], { decline: 'database' })
defaults('defaults-proto-key', { extra: protoObject }, { decline: 'protoKey' })
defaults('defaults-nai-img-config-string', { NAIImgConfig: 'x' }, { decline: 'NAIImgConfig' })
defaults('defaults-nai-img-config-array', { NAIImgConfig: [] }, { decline: 'NAIImgConfig' })
defaults('defaults-lorebook-string', { loreBook: 'ab', loreBookPage: 1 }, { decline: 'loreBook' })
defaults('defaults-lorebook-page-string', { loreBook: [], loreBookPage: '1' }, { decline: 'loreBookPage' })
defaults('defaults-formating-order-string', { formatingOrder: 'main' }, { decline: 'formatingOrder' })
defaults('defaults-nai-settings-number', { NAIsettings: 5 }, { decline: 'NAIsettings' })
defaults('defaults-google-string', { google: 'g' }, { decline: 'google' })
defaults('defaults-prompt-settings-array', { promptSettings: [] }, { decline: 'promptSettings' })
defaults('defaults-custom-text-theme-number', { customTextTheme: 1 }, { decline: 'customTextTheme' })
defaults('defaults-seperate-parameters-string', { seperateParameters: 's' }, { decline: 'seperateParameters' })
defaults('defaults-presets-string', { botPresets: 'ab' }, { decline: 'botPresets' })
defaults('defaults-preset-null', { botPresets: [null] }, { decline: 'botPresets' })
defaults('defaults-preset-array', { botPresets: [[]] }, { decline: 'botPresets' })
defaults('defaults-hypa-v3-presets-string', { hypaV3Presets: 'ab' }, { decline: 'hypaV3Presets' })
defaults('defaults-hypa-v3-presets-number', { hypaV3Presets: 5 }, { decline: 'hypaV3Presets' })
defaults('defaults-hypa-v3-preset-null', { hypaV3Presets: [null] }, { decline: 'hypaV3Presets' })
defaults('defaults-hotkeys-object', { hotkeys: {} }, { decline: 'hotkeys' })
defaults('defaults-hotkey-null', { hotkeys: [null] }, { decline: 'hotkeys' })
defaults('defaults-fallback-models-partial', { fallbackModels: { model: [] } }, { decline: 'fallbackModels' })
defaults('defaults-fallback-models-array', { fallbackModels: [] }, { decline: 'fallbackModels' })
defaults('defaults-chat-load-pages-to-string', { chatLoadInitialPages: { toString: 1 } }, { decline: 'toPrimitive' })
defaults('defaults-plugins-string', { plugins: 'ab' }, { decline: 'plugins' })
defaults('defaults-characters-string', { characters: 'ab' }, { decline: 'characters' })
defaults('defaults-character-null', { characters: [null] }, { decline: 'characters' })
defaults('defaults-chats-string', { characters: [{ chats: 'ab' }] }, { decline: 'chats' })
defaults('defaults-chat-null', { characters: [{ chats: [null] }] }, { decline: 'chats' })
defaults('defaults-chat-array', { characters: [{ chats: [[]] }] }, { decline: 'chats' })

// checkNewFormat
const v5 = { formatversion: 5 }
const noAssets = { customBackground: '', userIcon: '' }
const lorebook = (entries, extra = {}) => ({ ...v5, characters: [], modules: [{ lorebook: entries }], ...extra })
newFormat('new-format-empty', { characters: [], ...noAssets })
newFormat('new-format-character-defaults', {
    ...v5, characters: [{}, null, { name: 'n', chats: null, type: 'group', emotionImages: [['a', 'b']], bias: [] }, 0, '', false],
})
newFormat('new-format-character-number', { ...v5, characters: [5] }, { decline: 'characters' })
newFormat('new-format-character-string', { ...v5, characters: ['ab'] }, { decline: 'characters' })
newFormat('new-format-character-array', { ...v5, characters: [[]] }, { decline: 'characters' })
newFormat('new-format-characters-missing', { ...v5 }, { decline: 'characters' })
newFormat('new-format-not-object', [], { decline: 'database' })
newFormat('new-format-proto-key', { ...v5, characters: [], extra: protoObject }, { decline: 'protoKey' })
newFormat('new-format-modules', {
    ...v5, characters: [],
    modules: [null, 5, 'x', [], { name: 'plain' }, { name: 'empty', lorebook: [] }, { name: 'off', lorebook: 0 }, {
        name: 'book',
        lorebook: [
            { key: 'k', content: '@@end <char> <bot> <user> @@@end @@@@end <CHAR> <char <user>> @@en' },
            { content: '<char> kept', bookVersion: 2 },
            { content: '<char> kept too', bookVersion: 2.5 },
            { content: 'odds <user>', activationPercent: 30 },
            { content: 'zero', activationPercent: 0 },
            { content: 'text percent', bookVersion: null, activationPercent: '50' },
            { activationPercent: 10 },
            { content: 'list', activationPercent: [1, 2] },
            { content: 'object', activationPercent: { a: 1 } },
            { content: 'nested', activationPercent: true, bookVersion: 1.5 },
            { content: 'negative', bookVersion: -1 },
            { content: '😀<user>😀@@end😀', bookVersion: 0 },
        ],
    }],
})
newFormat('new-format-modules-null', { ...v5, characters: [], modules: null, personas: null })
newFormat('new-format-modules-string', { ...v5, characters: [], modules: 'ab' }, { decline: 'modules' })
newFormat('new-format-modules-object', { ...v5, characters: [], modules: {} }, { decline: 'modules' })
newFormat('new-format-module-lorebook-object-reset', { ...v5, characters: [], modules: [{ name: 'm', lorebook: { a: 1 } }] },
    { decline: 'moduleLorebook', answers: ['no', 'yes'] })
newFormat('new-format-module-lorebook-string-kept', { ...v5, characters: [], modules: [{ lorebook: 'abc' }] },
    { decline: 'moduleLorebook', answers: ['yes', 'no'] })
newFormat('new-format-lorebook-entry-null', lorebook([null]), { decline: 'lorebook' })
newFormat('new-format-lorebook-entry-number', lorebook([5]), { decline: 'lorebook' })
newFormat('new-format-lorebook-entry-array', lorebook([[]]), { decline: 'lorebook' })
newFormat('new-format-lorebook-content-missing', lorebook([{ key: 'a' }]), { decline: 'content' })
newFormat('new-format-lorebook-content-number', lorebook([{ content: 5 }]), { decline: 'content' })
newFormat('new-format-lorebook-book-version-string', lorebook([{ content: 'c', bookVersion: '3' }]), { decline: 'bookVersion' })
newFormat('new-format-lorebook-percent-to-string', lorebook([{ content: 'c', activationPercent: { toString: 'x' } }]), { decline: 'toPrimitive' })
newFormat('new-format-personas', { ...v5, characters: [], personas: [{ name: 'a' }, { id: 'kept' }, { id: null }, { id: '' }, { id: 0 }] })
newFormat('new-format-personas-string', { ...v5, characters: [], personas: 'ab' }, { decline: 'personas' })
newFormat('new-format-personas-object', { ...v5, characters: [], personas: {} }, { decline: 'personas' })
newFormat('new-format-persona-null', { ...v5, characters: [], personas: [null] }, { decline: 'personas' })
newFormat('new-format-persona-number', { ...v5, characters: [], personas: [5] }, { decline: 'personas' })
newFormat('new-format-persona-array', { ...v5, characters: [], personas: [[]] }, { decline: 'personas' })
newFormat('new-format-old-paths', {
    formatversion: 0, loreBookToken: 100,
    customBackground: 'C:\\Users\\x\\assets\\bg.png', userIcon: 'assets/u.png',
    characters: [
        {
            image: 'D:\\foo\\assets\\img.png',
            emotionImages: [
                ['joy', 'E:\\a\\assets\\joy.png'], ['x'], [], ['sad', 'assets/sad.png'], ['short', 'ab'], ['none', 'no-assets-here'],
                ['two', 'a/assets/b/assets/c'], ['end', 'trailing/assets/'], ['three', 'x', 'y'], 5, null, 'a', true,
            ],
        },
        { image: '' }, { image: 'ab' }, { image: 'assetsfoo' }, { image: '😀/assets/😀.png' }, { sdData: { kept: true } },
    ],
})
const oldPaths = (characters, extra = {}) => ({ characters, ...noAssets, ...extra })
newFormat('new-format-old-paths-missing-background', { characters: [], userIcon: '' }, { decline: 'customBackground' })
newFormat('new-format-old-paths-user-icon-number', oldPaths([], { userIcon: 5 }), { decline: 'userIcon' })
newFormat('new-format-old-paths-image-number', oldPaths([{ image: 5 }]), { decline: 'image' })
newFormat('new-format-old-paths-emotion-string-list', oldPaths([{ emotionImages: 'ab' }]), { decline: 'emotionImages' })
newFormat('new-format-old-paths-emotion-object-list', oldPaths([{ emotionImages: {} }]), { decline: 'emotionImages' })
newFormat('new-format-old-paths-emotion-string', oldPaths([{ emotionImages: ['ab'] }]), { decline: 'emotionImages' })
newFormat('new-format-old-paths-emotion-object', oldPaths([{ emotionImages: [{}] }]), { decline: 'emotionImages' })
newFormat('new-format-old-paths-emotion-number-path', oldPaths([{ emotionImages: [['a', 5]] }]), { decline: 'emotionImages' })
newFormat('new-format-version-2', {
    formatversion: 2, loreBookToken: null, characters: [{ sdData: null }, { sdData: { a: 1 } }, {}, { sdData: 0 }],
})
newFormat('new-format-version-4', { formatversion: 4, loreBookToken: 7999, characters: [] })
newFormat('new-format-version-4-tokens-kept', { formatversion: 4, loreBookToken: 9000, characters: [] })
newFormat('new-format-version-4-tokens-missing', { formatversion: 4, characters: [] })
newFormat('new-format-version-fraction', { formatversion: 4.5, loreBookToken: 1, characters: [] })
newFormat('new-format-version-6', { formatversion: 6, loreBookToken: 100, characters: [{}] })
newFormat('new-format-version-negative', { formatversion: -1, characters: [{}] })
newFormat('new-format-version-string', { formatversion: '3', characters: [] }, { decline: 'formatversion' })
newFormat('new-format-version-true', { formatversion: true, characters: [] }, { decline: 'formatversion' })
newFormat('new-format-lore-book-token-string', { formatversion: 4, loreBookToken: '100', characters: [] }, { decline: 'loreBookToken' })
newFormat('new-format-character-order', { ...v5, characters: [], characterOrder: null })
newFormat('new-format-character-order-kept', { ...v5, characters: [], characterOrder: ['x', { data: [] }] })
newFormat('new-format-main-prompt-old', { ...v5, characters: [] }, { oldPrompt: 'main' })
newFormat('new-format-main-prompt-old-jailbreak', { ...v5, characters: [] }, { oldPrompt: 'jailbreak' })
newFormat('new-format-main-prompt-other', { ...v5, characters: [], mainPrompt: 'custom' })
newFormat('new-format-trash', {
    ...v5,
    characters: [
        { chaId: 'boundary', trashTime: NOW - 3 * DAY }, { chaId: 'expired', trashTime: NOW - 3 * DAY - 1 }, { chaId: 'zero', trashTime: 0 },
        { chaId: 'null', trashTime: null }, { chaId: 'future', trashTime: NOW + DAY }, { chaId: 'live' }, { chaId: 'negative', trashTime: -1 },
    ],
})
newFormat('new-format-trash-string', { ...v5, characters: [{ trashTime: '5' }] }, { decline: 'trashTime' })
newFormat('new-format-trash-object', { ...v5, characters: [{ trashTime: {} }] }, { decline: 'trashTime' })
newFormat('new-format-unicode', {
    ...v5, characters: [{ name: '이름 😀', chaId: '캐릭터' }], personas: [{ name: '페르소나 \ud800' }],
    modules: [{ name: '모듈', lorebook: [{ content: '<user>님 😀 <char>' }] }],
})

// prepareDatabaseForPersistence
persistence('persistence-empty', {})
persistence('persistence-tauri-empty', {}, { tauri: true })
persistence('persistence-full', full)
persistence('persistence-compat', compat)
persistence('persistence-full-tauri', full, { tauri: true })
persistence('persistence-canonical-order', {
    zeta: 1,
    Alpha: { 10: 'ten', 2: 'two', b: 'b', a: 'a', B: 'B', é: 'e', '😀': 'emoji', Z: 'Z', '01': 'not an index', 4294967295: 'not an index' },
    globalChatVariables: { b: '1', 10: 'ten', 0: 'zero', a: '2', ä: '3', ﬀ: 'ligature', '\uffff': 'last', '\ud83d\ude00': 'pair' },
    characters: [{
        zz: 1, chaId: 'c', aa: [{ y: 1, x: 2 }, [{ d: 1, c: 2 }]],
        chats: [{ message: [{ role: 'user', data: 'hi', chatId: 'm' }], id: 'chat' }],
    }],
    beta: [3, { k: { j: 1, i: 2 } }],
})
persistence('persistence-old-format', {
    customBackground: 'C:\\x\\assets\\bg.png', loreBookToken: 100,
    characters: [
        { chaId: 'a', image: 'C:\\y\\assets\\a.png', chats: [{ message: [] }, { id: 'dup', message: [] }] },
        { chaId: 'a', chats: [{ id: 'dup' }] },
        { chaId: 'gone', trashTime: NOW - 4 * DAY },
        { chaId: 'trashed', trashTime: NOW - DAY },
        { chats: [] },
    ],
    characterOrder: ['gone', { name: 'folder', data: ['a', 'gone'] }, { data: [] }, 'missing'],
    personas: [{ name: 'p' }, { id: 'kept', name: 'q' }],
})
persistence('persistence-old-prompt', { formatversion: 5 }, { oldPrompt: 'main' })
persistence('persistence-unicode', {
    username: '사용자 😀', characters: [{ chaId: '캐릭터', name: '이름', chats: [{ id: '대화', message: [] }] }],
    modules: [{ name: '모듈', lorebook: [{ content: '<user> 😀' }] }],
})
persistence('persistence-module-lorebook', { modules: [{ name: 'm', lorebook: { a: 1 } }] },
    { decline: 'moduleLorebook', answers: ['no', 'no'] })
persistence('persistence-not-object', [], { decline: 'database' })
persistence('persistence-proto-key', { extra: protoObject }, { decline: 'protoKey' })
persistence('persistence-characters-string', { characters: 'ab' }, { decline: 'characters' })
persistence('persistence-character-number', { characters: [5] }, { decline: 'characters' })
persistence('persistence-old-paths-image-number', { characters: [{ image: 5 }] }, { decline: 'image' })
persistence('persistence-chat-id-number', { formatversion: 5, characters: [{ chaId: 'a', chats: [{ id: 5 }] }] }, { decline: 'chatId' })
persistence('persistence-character-order-number', { formatversion: 5, characters: [], characterOrder: [5] }, { decline: 'characterOrder' })
persistence('persistence-many', {
    formatversion: 5,
    characters: Array.from({ length: 40 }, (_, index) => ({
        chaId: index % 7 === 0 ? 'shared' : `c${index}`, name: `n${index}`,
        chats: Array.from({ length: index % 3 }, (_, chat) => ({ id: chat === 0 ? '' : `c${index}-${chat}`, message: [] })),
        ...(index % 11 === 0 ? { trashTime: NOW - index * DAY } : {}),
    })),
    characterOrder: [{ data: ['c1', 'c2', 'missing'] }, 'c3'],
})

writeFileSync(`${dir}prepare.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
console.log(`wrote ${cases.length} cases`)
