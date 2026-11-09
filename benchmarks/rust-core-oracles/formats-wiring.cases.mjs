// Writes the synthetic case file of the card, RISUM and card-as-module import wiring oracle
// (tests/fixtures/rust-core/formats/wiring.cases.json). Add cases here, run
// `node benchmarks/rust-core-oracles/formats-wiring.cases.mjs`, then regenerate the expected file with
// RUST_CORE_ORACLE_WRITE=1.
//
// Each case is a prepared content receipt as `native_file_job_prepared_content` returns it before the native mapping
// is added. The oracle activates it through the TS mapping and records the stored character or module; the app's Rust
// replay maps the same receipt. `native` is what the Rust receipt must carry: `mapped`, or the `reason` and `name` of
// its decline. `ts` is what the TS activation does with the receipt: it stores a character or module, except where the
// mapping finds the content invalid; a case where TS does otherwise says so. A card imported as a module has
// `destination: 'module'`, and `nativeModule` is the module mapping the Rust receipt carries beside the character's.
import { createCipheriv, createHash } from 'node:crypto'
import { mkdirSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/formats/', import.meta.url))
mkdirSync(dir, { recursive: true })

const cases = []
const ids = new Set()
function add(id, receipt, native, ts, module) {
    if (ids.has(id)) throw new Error(`duplicate ${id}`)
    ids.add(id)
    cases.push({
        id, receipt: { casSessionId: `wiring-${id}`, ...receipt },
        ...(module === undefined ? {} : { destination: 'module' }), native, ...(module === undefined ? {} : { nativeModule: module }),
        ...(ts === undefined ? {} : { ts }),
    })
}
const hash = digit => String(digit).repeat(64)
const asset = (token, digit, ext, extra = {}) => ({
    referenceKey: token, token, logicalId: `assets/${hash(digit)}.${ext}`, objectHash: hash(digit), byteSize: 4,
    mime: ext === 'png' ? 'image/png' : '', name: `${hash(digit)}.${ext}`, ext, ...extra,
})
const v3 = data => ({ spec: 'chara_card_v3', spec_version: '3.0', data })
const mapped = 'mapped'
const declined = (reason, name) => ({ reason, name })
// An object with an own `__proto__` key, which only JSON.parse creates.
const protoObject = JSON.parse('{"__proto__": {"polluted": true}, "plain": 1}')

const book = {
    scan_depth: 5, token_budget: 300, recursive_scanning: true, extensions: { risu_fullWordMatching: true },
    entries: [{
        keys: ['alpha', 'beta'], secondary_keys: ['gamma'], content: 'Book entry.', extensions: { risu_case_sensitive: true },
        enabled: true, insertion_order: 3, constant: false, selective: true, name: 'Entry', comment: 'Entry', case_sensitive: true,
        use_regex: false,
    }],
}

add('json-card-full', {
    format: 'json-card',
    metadata: v3({
        name: 'Wiring Full', description: 'Description.', personality: 'Calm.', scenario: 'A room.', first_mes: 'Hello.',
        mes_example: '<START>', creator_notes: 'Notes.', system_prompt: 'System.', post_history_instructions: 'After.',
        alternate_greetings: ['Hi again.'], tags: ['synthetic'], creator: 'oracle', character_version: '2', nickname: 'Full',
        source: ['local'], creation_date: 1700000000, modification_date: 1700000100, group_only_greetings: [],
        character_book: book,
        extensions: {
            risuai: {
                bias: [['word', 5]], viewScreen: 'emotion', customScripts: [{ comment: 'regex', in: 'a', out: 'b', type: 'editdisplay' }],
                utilityBot: false, sdData: [['always', 'x']], triggerscript: [], additionalText: 'More.', backgroundHTML: '<b>bg</b>',
                license: '', largePortrait: true, lorePlus: false, inlayViewScreen: false, defaultVariables: 'a=1',
                lowLevelAccess: false, hideChatIcon: false,
            },
            depth_prompt: { depth: 4, prompt: 'Depth.' },
            other_extension: { kept: 1 },
        },
        assets: [
            { type: 'icon', uri: 'ccdefault:', name: 'main', ext: 'png' },
            { type: 'emotion', uri: '__asset:native-data-1', name: 'happy', ext: 'png' },
            { type: 'x-risu-asset', uri: '__asset:native-data-2', name: 'background', ext: 'webp' },
            { type: 'x-risu-asset', uri: '__asset:native-data-2', name: 'background copy', ext: 'webp' },
        ],
    }),
    assets: [asset('native-data-0', 1, 'png'), asset('native-data-1', 2, 'png'), asset('native-data-2', 3, 'webp')],
    portraitLogicalId: `assets/${hash(1)}.png`,
}, mapped)

add('charx-card-module', {
    format: 'charx-card',
    metadata: v3({
        name: 'Wiring CharX', description: 'From a CharX file.', first_mes: 'Hi.', character_book: book,
        extensions: { risuai: { customScripts: [{ comment: 'card regex' }], triggerscript: [{ comment: 'card trigger' }] } },
        assets: [
            { type: 'icon', uri: 'embeded://assets/icon/main.png', name: 'main', ext: 'png' },
            { type: 'x-risu-asset', uri: 'embeded://assets/other/image/extra.png', name: 'extra', ext: 'png' },
        ],
    }),
    assets: [asset('assets/icon/main.png', 4, 'png'), asset('assets/other/image/extra.png', 5, 'png')],
    portraitLogicalId: `assets/${hash(4)}.png`,
    module: {
        trigger: [{ comment: 'module trigger', type: 'manual', conditions: [], effect: [] }],
        regex: [{ comment: 'module regex', in: 'x', out: 'y', type: 'editoutput' }],
        lorebook: [{ key: 'module key', secondkey: '', insertorder: 1, comment: 'module lore', content: 'Module.', mode: 'normal', alwaysActive: false, selective: false }],
    },
}, mapped)

add('appended-charx-jpeg-module-without-lorebook', {
    format: 'appended-charx-jpeg',
    metadata: v3({ name: 'Wiring JPEG', character_book: book, extensions: { risuai: { utilityBot: true } } }),
    assets: [],
    module: { trigger: [], regex: [{ comment: 'only regex', in: 'q', out: 'r', type: 'editinput' }] },
}, mapped)

add('json-card-minimal', { format: 'json-card', metadata: v3({ name: 'Minimal', extensions: {} }), assets: [] }, mapped)

add('json-card-low-level-access', {
    format: 'json-card',
    metadata: v3({ name: 'Low Level', extensions: { risuai: { lowLevelAccess: true } } }),
    assets: [],
}, declined('unsupported', 'lowLevelAccess'))

add('json-card-proto-key', {
    format: 'json-card',
    metadata: v3({ name: 'Proto', extensions: { risuai: {}, custom: protoObject } }),
    assets: [],
}, declined('unsupported', 'protoKey'))

add('json-card-missing-asset', {
    format: 'json-card',
    metadata: v3({ name: 'Missing', extensions: {}, assets: [{ type: 'x-risu-asset', uri: '__asset:never-staged', name: 'gone', ext: 'png' }] }),
    assets: [],
}, declined('invalid', 'missingAsset'))

add('json-card-data-uri', {
    format: 'json-card',
    metadata: v3({ name: 'Inline', extensions: {}, assets: [{ type: 'x-risu-asset', uri: 'data:image/png;base64,AAAA', name: 'inline', ext: 'png' }] }),
    assets: [],
}, declined('invalid', 'dataUri'))

add('json-card-ccdefault-without-portrait', {
    format: 'json-card',
    metadata: v3({ name: 'No Portrait', extensions: {}, assets: [{ type: 'icon', uri: 'ccdefault:', name: 'main', ext: 'png' }] }),
    assets: [],
}, declined('invalid', 'ccdefaultPortrait'))

add('json-card-duplicate-asset-token', {
    format: 'json-card',
    metadata: v3({ name: 'Duplicate', extensions: {} }),
    assets: [asset('native-data-0', 6, 'png'), asset('native-data-0', 7, 'png')],
}, declined('invalid', 'duplicateAssetToken'))

add('json-card-blank-asset-token', {
    format: 'json-card',
    metadata: v3({ name: 'Blank Token', extensions: {} }),
    assets: [asset('  ', 6, 'png')],
}, declined('invalid', 'assetToken'))

add('json-card-blank-portrait', {
    format: 'json-card',
    metadata: v3({ name: 'Blank Portrait', extensions: {} }),
    assets: [],
    portraitLogicalId: '   ',
}, declined('invalid', 'portraitLogicalId'))

// PNG cards: the job hands TS the `chara` and `ccv3` text chunks, the portrait first and every `chara-ext-asset_` chunk
// under its reference, and TS decodes the card before it maps it.
const base64 = bytes => Buffer.from(bytes).toString('base64')
const encoded = card => base64(JSON.stringify(card))
const portrait = digit => asset('native-png-portrait', digit, 'png')
const chunk = (reference, digit) => asset(reference, digit, 'png', { mime: '' })
const png = (metadata, chunks = [], digit = 8) => ({
    format: 'png-card', metadata, assets: [portrait(digit), ...chunks], portraitLogicalId: portrait(digit).logicalId,
})
const v2 = data => ({ spec: 'chara_card_v2', spec_version: '2.0', data })
// The RCC envelope RisuAI writes: AES-GCM under SHA-256 of the password with a zero IV, then the payload hash.
function rcc(card, { hash } = {}) {
    const cipher = createCipheriv('aes-256-gcm', createHash('sha256').update('RISU_NONE').digest(), Buffer.alloc(12))
    const encrypted = Buffer.concat([cipher.update(JSON.stringify(card)), cipher.final(), cipher.getAuthTag()])
    return ['rcc', 'rccv1', base64(encrypted), hash ?? createHash('sha256').update(encrypted).digest('hex'),
        encoded({ usePassword: false })].join('||')
}

add('png-v3-embedded-assets', png({
    chara: encoded(v2({ name: 'Ignored V2', extensions: {} })),
    ccv3: encoded(v3({
        name: 'Wiring PNG', description: 'From a PNG.', first_mes: 'Hello from PNG.', character_book: book,
        extensions: { risuai: { customScripts: [{ comment: 'png regex', in: 'p', out: 'q', type: 'editdisplay' }] } },
        assets: [
            { type: 'icon', uri: 'ccdefault:', name: 'main', ext: 'png' },
            { type: 'emotion', uri: '__asset:1', name: 'smile', ext: 'png' },
            { type: 'x-risu-asset', uri: '__asset:2', name: 'scene', ext: 'png' },
            { type: 'x-risu-asset', uri: 'embeded://2', name: 'scene by URI', ext: 'png' },
        ],
    })),
}, [chunk('1', 9), chunk('2', 'a')]), mapped)

add('png-v2-chara', png({
    chara: encoded(v2({
        name: 'Wiring PNG V2', description: 'Older card.', first_mes: 'Hi.', character_version: 1.5, character_book: book,
        extensions: {
            risuai: {
                emotions: [['happy', '__asset:1']], additionalAssets: [['scene', '__asset:2', 'png']],
                utilityBot: false, triggerscript: [],
            },
            depth_prompt: { depth: 2, prompt: 'Depth.' },
        },
    })),
}, [chunk('1', 9), chunk('2', 'a')]), mapped)

add('png-v2-character-version-exponent', png({
    chara: encoded(v2({ name: 'Exponent', character_version: 1e21, extensions: {} })),
}), mapped)

add('png-off-spec-card', png({
    chara: encoded({ name: 'Tavern', description: 'Old style.', personality: 'Plain.', scenario: '', first_mes: 'Hey.', mes_example: '' }),
}), mapped)

add('png-bom', png({ ccv3: base64(Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from(JSON.stringify(v3({ name: 'BOM', extensions: {} })))])) }), mapped)

{
    // The lowest bit of the character before the padding comes after the last byte, and `Buffer.from` ignores it.
    const text = encoded(v3({ name: 'Trailing', extensions: {} }))
    const last = text.length - (text.endsWith('==') ? 3 : text.endsWith('=') ? 2 : 0)
    const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
    const changed = text.slice(0, last) + alphabet[alphabet.indexOf(text[last]) | 1] + text.slice(last + 1)
    if (last === text.length || changed === text || !Buffer.from(changed, 'base64').equals(Buffer.from(text, 'base64'))) {
        throw new Error('the trailing-bits case needs a padded encoding that decodes to the same card')
    }
    add('png-trailing-bits', png({ ccv3: changed }), mapped)
}

add('png-lone-surrogate', png({
    ccv3: base64('{"spec":"chara_card_v3","spec_version":"3.0","data":{"name":"Surrogate \\udc00","extensions":{}}}'),
}), mapped)

add('png-low-level-access', png({ ccv3: encoded(v3({ name: 'PNG Low Level', extensions: { risuai: { lowLevelAccess: true } } })) }),
    declined('unsupported', 'lowLevelAccess'))

// A PNG V3 export keeps an `embeded://` asset URI the character already had, and no chunk carries it.
add('png-embeded-uri-without-chunk', png({
    ccv3: encoded(v3({
        name: 'Embedded URI', extensions: {},
        assets: [{ type: 'x-risu-asset', uri: 'embeded://assets/other/image/lost.png', name: 'lost', ext: 'png' }],
    })),
}), declined('invalid', 'missingAsset'))

// TS stages an inline payload before it maps the card; the port sees the payload itself.
add('png-v3-inline-data-uri', png({
    ccv3: encoded(v3({
        name: 'Inline V3', extensions: {},
        assets: [{ type: 'x-risu-asset', uri: `data:image/png;base64,${base64('inline-v3')}`, name: 'inline', ext: 'png' }],
    })),
}), declined('invalid', 'dataUri'), 'stored')

add('png-v2-inline-emotion', png({
    chara: encoded(v2({ name: 'Inline V2', extensions: { risuai: { emotions: [['sad', base64('inline-v2')]] } } })),
}), declined('invalid', 'assetReference'), 'stored')

add('png-rcc', png({ ccv3: rcc(v3({ name: 'Encrypted', extensions: {} })) }), declined('unsupported', 'rcc'))

add('png-rcc-hash-mismatch', png({ ccv3: rcc(v3({ name: 'Tampered', extensions: {} }), { hash: '0'.repeat(64) }) }),
    declined('unsupported', 'rcc'), 'error')

add('png-not-base64', png({ ccv3: 'e30' }), declined('invalid', 'pngBase64'))

add('png-not-utf8', png({ ccv3: base64([0x7b, 0xff, 0x7d]) }), declined('invalid', 'pngUtf8'))

add('png-json-syntax', png({ ccv3: base64('{"spec":') }), declined('invalid', 'json-syntax'))

add('png-not-object', png({ ccv3: base64('[1,2]') }), declined('unsupported', 'card'), 'error')

// `ccv3 ?? chara` takes `chara` past a null `ccv3`. The job never writes a null chunk and the receipt validator refuses
// one, so this pins the decode rule only.
add('png-ccv3-null', png({ ccv3: null, chara: encoded(v2({ name: 'Null V3', description: 'From chara.', extensions: {} })) }), mapped)

// `TextDecoder` drops one BOM; the second is a character `JSON.parse` refuses.
add('png-double-bom', png({
    ccv3: base64(Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf]), Buffer.from(JSON.stringify(v3({ name: 'Two BOMs', extensions: {} })))])),
}), declined('invalid', 'json-syntax'))

// RISUM modules: the job hands TS the file's `module` object with each asset tuple's key blanked, one descriptor per
// asset frame in order, and the owner head of the frames' manifest (absent when the module has no `assets` key).
const frame = (position, digit, ext) => ({
    position, logicalId: `assets/${hash(digit)}.${ext}`, objectHash: hash(digit), byteSize: 4, mime: '', name: '', ext,
})
const risum = (metadata, frames = []) => ({
    format: 'risu-module', metadata, assets: frames,
    ownerHead: Object.hasOwn(metadata, 'assets')
        ? { present: true, manifestHash: hash('c'), entryCount: frames.length }
        : { present: false, manifestHash: null, entryCount: 0 },
})
const moduleLore = (overrides = {}) => ({
    key: 'alpha, beta', secondkey: '', insertorder: 100, comment: 'Lore', content: 'Lore content', mode: 'normal',
    alwaysActive: false, selective: false, ...overrides,
})
const fullModule = {
    type: 'risuModule', name: 'Wiring Module', description: 'A synthetic module.', id: 'stored-module-id',
    lorebook: [moduleLore(), moduleLore({ key: '', comment: 'folder', content: '', mode: 'folder', folder: 'f' })],
    regex: [{ comment: 'regex', in: 'a', out: 'b', type: 'editinput', ableFlag: false }],
    trigger: [{ comment: 'trigger', type: 'start', conditions: [], effect: [{ type: 'v2Header', code: '', indent: 0 }] }],
    lowLevelAccess: false, hideIcon: true, backgroundEmbedding: '<style>x</style>',
    assets: [['asset one', '', 'png'], ['asset two', '', 'webp', 'extra slot', { future: true }]],
    namespace: 'ns', customModuleToggle: 'toggle=1', mcp: { url: 'https://example.invalid/mcp' }, icon: 'stored-icon',
    extra: { nested: [1, 2.5, 1e21, 12345678901234567890] }, 5: 'index key',
}

add('risum-full', risum(fullModule, [frame(0, 1, 'png'), frame(1, 2, 'webp')]), mapped)
add('risum-no-assets', risum({ name: 'No assets', description: 'd', id: 'old', lorebook: [moduleLore()] }), mapped)
add('risum-no-id', risum({ name: 'No id', assets: [['a', '', 'png']] }, [frame(0, 3, 'png')]), mapped)
add('risum-empty-assets', risum({ id: 'first', name: 'Empty assets', assets: [] }), mapped)
// Two frames with the same payload share one alias.
add('risum-duplicate-payloads', risum({ name: 'Duplicates', assets: [['a', '', 'png'], ['b', '', 'png']] }, [frame(0, 4, 'png'), frame(1, 4, 'png')]), mapped)
add('risum-unicode', risum({
    name: '모듈 🐿️', description: '설명', lorebook: [moduleLore({ key: '키, 열쇠', content: '내용 😀' })], assets: [['이름 😀', '', 'png']],
}, [frame(0, 5, 'png')]), mapped)
// Falsy low-level access needs no confirmation, and an `mcp` that is no object has no URL to check.
add('risum-falsy-low-level-string-mcp', risum({ name: 'Falsy', lowLevelAccess: 0, mcp: 'stdio:x' }), mapped)

add('risum-low-level-access', risum({ name: 'Low level', lowLevelAccess: true, assets: [['a', '', 'png']] }, [frame(0, 6, 'png')]),
    declined('unsupported', 'lowLevelAccess'))
add('risum-proto-key', risum({ name: 'Proto', extra: protoObject }), declined('unsupported', 'protoKey'))
add('risum-stdio', risum({ name: 'Stdio', mcp: { url: 'stdio:{"command":"node"}' } }), declined('invalid', 'stdioMcp'))
// TS checks the MCP URL before it asks to confirm low-level access.
add('risum-stdio-low-level', risum({ name: 'Stdio low level', lowLevelAccess: true, mcp: { url: 'stdio:x' } }), declined('invalid', 'stdioMcp'))
// The job reads asset tuples only from an array, so metadata that does not match the frames reaches TS.
add('risum-assets-null', risum({ name: 'Null assets', assets: null }), declined('invalid', 'assetMetadata'))
add('risum-assets-object', risum({ name: 'Object assets', assets: { 0: ['a', '', 'png'] } }), declined('invalid', 'assetMetadata'))
add('risum-assets-count-mismatch', risum({ name: 'Mismatch', assets: [['a', '', 'png']] }), declined('invalid', 'assetMetadata'))
// The job refuses a short tuple, which the receipt validator does not check.
add('risum-tuple-short', risum({ name: 'Short', assets: [['a', '']] }, [frame(0, 7, 'png')]), declined('invalid', 'assetTuple'))

// Cards imported as modules: the same receipts, which the module activation maps as a character and converts with
// `convertCharacterToModule`. A card the mapping declines declines the module too.
const asModule = (id, receipt, native, module, ts) => add(`module-${id}`, receipt, native, ts, module)
const receiptOf = id => {
    const { casSessionId: _, ...receipt } = cases.find(entry => entry.id === id).receipt
    return structuredClone(receipt)
}
asModule('json-card-full', receiptOf('json-card-full'), mapped, mapped)
asModule('charx-card', receiptOf('charx-card-module'), mapped, mapped)
asModule('appended-charx-jpeg', receiptOf('appended-charx-jpeg-module-without-lorebook'), mapped, mapped)
asModule('json-card-minimal', receiptOf('json-card-minimal'), mapped, mapped)
asModule('png-v3', receiptOf('png-v3-embedded-assets'), mapped, mapped)
asModule('png-v2', receiptOf('png-v2-chara'), mapped, mapped)
// Greetings without a first message still make the first messages entry.
asModule('greetings-only', {
    format: 'json-card', metadata: v3({ name: 'Greetings', first_mes: '', alternate_greetings: ['One.', 'Two.'], extensions: {} }), assets: [],
}, mapped, mapped)
asModule('unicode', {
    format: 'json-card',
    metadata: v3({ name: '모듈 카드 🐿️', description: '설명 😀', first_mes: '안녕', post_history_instructions: '노트', extensions: {} }),
    assets: [],
}, mapped, mapped)
// The card maps greetings that are no array as they are, and the conversion reads them as JS does, so TS converts.
asModule('greetings-string', {
    format: 'json-card', metadata: v3({ name: 'Greeting string', first_mes: 'Hi.', alternate_greetings: 'Hey.', extensions: {} }), assets: [],
}, mapped, declined('unsupported', 'alternateGreetings'))
asModule('low-level-access', receiptOf('json-card-low-level-access'), declined('unsupported', 'lowLevelAccess'),
    declined('unsupported', 'lowLevelAccess'))
asModule('proto-key', receiptOf('json-card-proto-key'), declined('unsupported', 'protoKey'), declined('unsupported', 'protoKey'))
asModule('missing-asset', receiptOf('json-card-missing-asset'), declined('invalid', 'missingAsset'), declined('invalid', 'missingAsset'))
asModule('png-inline-data-uri', receiptOf('png-v3-inline-data-uri'), declined('invalid', 'dataUri'), declined('invalid', 'dataUri'), 'stored')

writeFileSync(`${dir}wiring.cases.json`, `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`)
console.log(`wrote ${cases.length} cases`)
