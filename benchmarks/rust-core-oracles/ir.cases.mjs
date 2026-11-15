// Writes the synthetic case files of the ir oracle (tests/fixtures/rust-core/ir/*.cases.json). Add cases here, run
// `node benchmarks/rust-core-oracles/ir.cases.mjs`, then regenerate the expected files with RUST_CORE_ORACLE_WRITE=1.
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/ir/', import.meta.url))
mkdirSync(dir, { recursive: true })

// String encoding: a plain JSON string when well-formed, otherwise its UTF-16 units.
const str = (value) => value.isWellFormed() ? value : { u: [...Array(value.length).keys()].map(i => value.charCodeAt(i)) }
const units = (...values) => ({ u: values })

function write(set, cases) {
    const ids = new Set()
    for (const entry of cases) { if (ids.has(entry.id)) throw new Error(`duplicate ${set} ${entry.id}`); ids.add(entry.id) }
    // One case per line keeps the tagged values readable in review.
    const text = `{"schema": 1, "cases": [\n${cases.map(entry => JSON.stringify(entry)).join(',\n')}\n]}\n`
    if (text.includes('\r')) throw new Error('CR in cases')
    writeFileSync(join(dir, `${set}.cases.json`), text)
    console.log(set, cases.length)
}

// splitmix64 for reproducible synthetic doubles.
let seed = 0x5eed_1b0bn
function next64() {
    seed = (seed + 0x9e3779b97f4a7c15n) & 0xffffffffffffffffn
    let z = seed
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & 0xffffffffffffffffn
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & 0xffffffffffffffffn
    return z ^ (z >> 31n)
}
const hex = (bits) => bits.toString(16).padStart(16, '0')
const bitsOf = (number) => { const view = new DataView(new ArrayBuffer(8)); view.setFloat64(0, number); return view.getBigUint64(0) }

// --- JSON.parse ---
const parseSources = [
    ['null', 'null'], ['true', 'true'], ['false', 'false'], ['zero', '0'], ['negative-zero', '-0'], ['negative-zero-fraction', '-0.0'],
    ['zero-exponent', '0e0'], ['negative-zero-exponent', '-0e-0'], ['one', '1'], ['negative-one', '-1'], ['fraction', '1.5'],
    ['exponent-21', '1e21'], ['exponent-upper', '1E21'], ['exponent-plus', '1E+2'], ['exponent-minus', '1e-2'], ['small', '1e-7'],
    ['thirty-digits', '123456789012345678901234567890'], ['beyond-safe', '9007199254740993'], ['overflow', '1e400'],
    ['negative-overflow', '-1e400'], ['underflow', '1e-400'], ['negative-underflow', '-1e-400'], ['tenth', '0.1'],
    ['subnormal-round-up', '2.5e-324'], ['min-subnormal', '4.9406564584124654e-324'], ['max', '1.7976931348623157e308'],
    ['above-max', '1.7976931348623159e308'], ['long-fraction', '0.30000000000000004441'],
    ['empty-string', '""'], ['ascii-string', '"abc"'], ['escape-letter', '"\\u0041"'], ['isolated-high', '"\\ud800"'],
    ['isolated-low', '"\\udfff"'], ['escaped-pair', '"\\ud83d\\ude00"'], ['upper-hex-pair', '"\\uD800\\uDC00"'],
    ['high-then-letter', '"\\ud83dx"'], ['reversed-pair', '"\\ude00\\ud83d"'], ['short-escapes', '"\\\\\\"\\/\\b\\f\\n\\r\\t"'],
    ['control-escapes', '"\\u0000\\u001f"'], ['line-separators', '"\u2028\u2029"'], ['hangul', '"한국어"'], ['raw-emoji', '"😀"'],
    ['escaped-high-raw-low', '"\\ud83d\u{1F600}"'],
    ['empty-array', '[]'], ['nested-array', '[1,[2,[3]]]'], ['mixed-array', '[null,true,"x",{}]'],
    ['json-whitespace', ' \t\n\r[ 1 , 2 ] \n'], ['empty-object', '{}'], ['object', '{"a":1,"b":2}'],
    ['duplicate-key', '{"a":1,"b":2,"a":3}'], ['proto-key', '{"__proto__":{"x":1}}'],
    ['index-keys', '{"b":1,"2":2,"1":3,"a":4,"0":5}'], ['index-boundary', '{"4294967294":1,"4294967295":2,"01":3,"1":4}'],
    ['non-index-numbers', '{"-1":1,"1.5":2,"1":3}'], ['duplicate-index', '{"1":"a","0":"b","1":"c"}'],
    ['isolated-key', '{"\\ud800":1,"a":2}'], ['nested-object', '{"outer":{"inner":[{"deep":null}]},"after":"x"}'],
    ['empty-key', '{"":1}'], ['depth-20', `${'['.repeat(20)}1${']'.repeat(20)}`],
]
const invalidSources = [
    ['empty-source', ''], ['blank-source', ' '], ['leading-zero', '01'], ['plus', '+1'], ['trailing-dot', '1.'], ['leading-dot', '.5'],
    ['bare-exponent', '1e'], ['minus', '-'], ['array-trailing-comma', '[1,]'], ['object-trailing-comma', '{"a":1,}'],
    ['unquoted-key', '{a:1}'], ['single-quote', "'x'"], ['hex-escape', '"\\x41"'], ['short-unicode', '"\\u12"'],
    ['bad-unicode', '"\\u12G4"'], ['unterminated', '"abc'], ['raw-newline', '"a\nb"'], ['raw-tab', '"\t"'], ['nan', 'NaN'],
    ['infinity', 'Infinity'], ['negative-infinity', '-Infinity'], ['two-values', 'true false'], ['partial-literal', 'nul'],
    ['missing-comma', '[1 2]'], ['missing-colon', '{"a" 1}'], ['nbsp', '\u00a01'], ['bom', '\ufeff1'], ['vertical-tab', '\u000b1'],
    ['form-feed', '\u000c1'], ['unclosed-array', '['], ['hex-number', '0x10'], ['negative-leading-zero', '-01'],
]
for (const [id, source] of [...parseSources, ...invalidSources]) if (!source.isWellFormed()) throw new Error(`${id} source`)
write('json-parse', [
    ...parseSources.map(([id, source]) => ({ id, source })),
    ...invalidSources.map(([id, source]) => ({ id: `invalid-${id}`, source })),
])

// --- JSON.parse over UTF-16 text, which can hold raw isolated surrogates ---
// Every json-parse source also runs as the second element of an array whose first element holds a raw isolated
// surrogate, so the whole grammar is parsed from text that is not well-formed; an invalid source stays invalid.
const unitSources = [
    ['plain', '"plain 한국어 😀"'], ['plain-object', '{"b":1,"2":2,"a":[true,null]}'],
    ['raw-high', '"\ud800"'], ['raw-low', '"\udfff"'], ['raw-reversed-pair', '"\ude00\ud83d"'], ['raw-high-high-low', '"\ud800\ud800\udc00"'],
    ['raw-high-escaped-low', '"\ud83d\\ude00"'], ['escaped-high-raw-low', '"\\ud83d\ude00"'], ['raw-high-then-quote-escape', '"\ud800\\""'],
    ['raw-low-after-emoji', '"😀\udc00"'], ['raw-around-escapes', '"\ud800\\n\\u0041\udbff"'], ['raw-in-key', '{"\udc00":1,"a":2}'],
    ['raw-and-escaped-key', '{"\ud800":1,"b":2,"\\ud800":3}'], ['raw-index-keys', '{"\ud800":0,"1":1,"0":2}'],
    ['raw-nested', '{"outer":["\ud800",{"inner":"x\udfffy"}],"after":-0}'], ['raw-whitespace-around', ' \n["\ud800"] \t'],
    ['invalid-raw-outside-string', '[\ud800]'], ['invalid-raw-before-value', '\ud8001'], ['invalid-raw-after-value', '"\ud800"\udc00'],
    ['invalid-raw-then-garbage', '["\ud800",x]'], ['invalid-raw-unterminated', '"\ud800'], ['invalid-raw-key-unquoted', '{\ud800:1}'],
    ...[...parseSources, ...invalidSources.map(([id, source]) => [`invalid-${id}`, source])]
        .map(([id, source]) => [`wrapped-${id}`, `["\ud800",${source}]`]),
]
write('json-parse-units', unitSources.map(([id, source]) => ({ id, source: str(source) })))

// --- JSON.stringify ---
// Tagged values: ["undefined"], ["null"], ["bool", b], ["number", repr], ["string", s], ["array", [...]], ["object", [[key, value], ...]].
const S = (value) => ['string', str(value)]
const N = (repr) => ['number', repr]
const A = (...items) => ['array', items]
const O = (...entries) => ['object', entries.map(([key, value]) => [str(key), value])]
const U = ['undefined']
const controls = String.fromCharCode(...Array.from({ length: 32 }, (_, i) => i))
let deep = N('1')
for (let i = 0; i < 20; i++) deep = i % 2 ? A(deep) : O(['k', deep])
write('json-stringify', [
    { id: 'undefined', value: U }, { id: 'null', value: ['null'] }, { id: 'true', value: ['bool', true] }, { id: 'false', value: ['bool', false] },
    ...[['zero', '0'], ['negative-zero', '-0'], ['nan', 'NaN'], ['infinity', 'Infinity'], ['negative-infinity', '-Infinity'],
        ['exponent-21', '1e21'], ['small', '1e-7'], ['fraction', '1.5'], ['negative-fraction', '-1.5'], ['integer', '123456789'],
        ['sum', '0.30000000000000004'], ['min-subnormal', '5e-324'], ['max', '1.7976931348623157e308'], ['hundred', '100'],
        ['exponent-20', '1e20'], ['tiny', '123e-20'], ['safe-limit', '9007199254740992']].map(([id, repr]) => ({ id: `number-${id}`, value: N(repr) })),
    { id: 'string-empty', value: S('') }, { id: 'string-ascii', value: S('abc') },
    { id: 'string-isolated-high', value: S('\ud800') }, { id: 'string-isolated-low', value: S('\udc00') },
    { id: 'string-pair', value: S('\ud83d\ude00') }, { id: 'string-reversed-pair', value: S('\ude00\ud83d') },
    { id: 'string-controls', value: S(controls) }, { id: 'string-quote-backslash-slash', value: S('"\\/') },
    { id: 'string-delete', value: S('\u007f') }, { id: 'string-line-separators', value: S('\u2028\u2029') },
    { id: 'string-noncharacters', value: S('\ufffe\uffff') }, { id: 'string-hangul-emoji', value: S('한국어 😀') },
    { id: 'string-isolated-inside', value: S('a\ud800b') }, { id: 'string-high-at-end', value: S('ab\udbff') },
    { id: 'string-high-high-low', value: S('\ud800\ud800\udc00') }, { id: 'string-escapes-and-isolated', value: S('\n\ud800"\t') },
    { id: 'array-empty', value: A() }, { id: 'array-undefined', value: A(U) }, { id: 'array-mixed-undefined', value: A(N('1'), U, N('2'), ['null']) },
    { id: 'array-nonfinite', value: A(N('NaN'), N('Infinity'), N('-0')) }, { id: 'array-nested', value: A(A(), A(A(S('x')))) },
    { id: 'object-empty', value: O() }, { id: 'object-undefined', value: O(['a', U]) },
    { id: 'object-undefined-first', value: O(['a', U], ['b', N('1')]) }, { id: 'object-undefined-middle', value: O(['a', N('1')], ['b', U], ['c', N('2')]) },
    { id: 'object-undefined-last', value: O(['a', N('1')], ['b', U]) },
    { id: 'object-escaped-keys', value: O(['"', N('1')], ['\n', N('2')], ['\ud800', N('3')], ['', N('4')]) },
    { id: 'object-proto-key', value: O(['__proto__', O(['x', N('1')])]) },
    { id: 'object-index-keys', value: O(['0', S('a')], ['1', S('b')], ['10', S('c')], ['x', S('d')], ['01', S('e')]) },
    { id: 'object-nested-undefined', value: O(['list', A(U, O(['gone', U], ['kept', ['null']]))]) },
    { id: 'object-key-order', value: O(['zeta', N('1')], ['alpha', N('2')], ['mid', N('3')]) },
    { id: 'depth-20', value: deep },
])

// --- Number.prototype.toString ---
const fixedBits = [
    ['zero', 0x0n], ['negative-zero', 0x8000000000000000n], ['nan', 0x7ff8000000000000n], ['nan-payload', 0x7ff0000000000001n],
    ['infinity', 0x7ff0000000000000n], ['negative-infinity', 0xfff0000000000000n], ['one', 0x3ff0000000000000n],
    ['max', 0x7fefffffffffffffn], ['min-subnormal', 0x1n], ['max-subnormal', 0x000fffffffffffffn], ['min-normal', 0x0010000000000000n],
    ['two-53', 0x4340000000000000n], ['exponent-21', bitsOf(1e21)], ['below-exponent-21', bitsOf(1e21) - 1n],
    ['exponent-minus-7', bitsOf(1e-7)], ['above-exponent-minus-7', bitsOf(1e-7) + 1n], ['exponent-minus-6', bitsOf(1e-6)],
    ['below-exponent-minus-6', bitsOf(1e-6) - 1n], ['tenth', bitsOf(0.1)], ['third', bitsOf(1 / 3)], ['sum', bitsOf(0.1 + 0.2)],
    ['twenty-digits', bitsOf(123456789012345680000)], ['negative-max', 0xffefffffffffffffn], ['negative-tiny', 0x8000000000000001n],
]
const toStringCases = fixedBits.map(([id, bits]) => ({ id, bits: hex(bits) }))
for (let i = 0; i < 300; i++) toStringCases.push({ id: `random-bits-${i}`, bits: hex(next64()) })
for (let i = 0; i < 100; i++) {
    // Short decimals and integers, the shapes prompts and settings carry.
    const digits = Number(next64() % 1000000n)
    const scale = Number(next64() % 12n)
    toStringCases.push({ id: `random-decimal-${i}`, bits: hex(bitsOf((i % 2 ? -1 : 1) * digits / 10 ** scale)) })
}
write('number-to-string', toStringCases)

// --- Number(string) ---
const fromText = [
    ['empty', ''], ['space', ' '], ['all-whitespace', '\t\n\v\f\r \u00a0\u1680\u2000\u200a\u2028\u2029\u202f\u205f\u3000\ufeff'],
    ['zero', '0'], ['negative-zero', '-0'], ['plus-zero', '+0'], ['double-zero', '00'], ['leading-zeros', '007'], ['one', '1'],
    ['negative-one', '-1'], ['plus-one', '+1'], ['trailing-dot', '1.'], ['leading-dot', '.5'], ['negative-leading-dot', '-.5'],
    ['plus-leading-dot', '+.5'], ['exponent', '1.5e3'], ['exponent-plus', '1.5E+3'], ['exponent-minus', '1.5e-3'],
    ['bare-exponent', '1e'], ['bare-exponent-sign', '1e+'], ['exponent-only', 'e5'], ['dot', '.'], ['minus', '-'], ['plus', '+'],
    ['dot-exponent', '1.e5'], ['dot-only-exponent', '.e5'], ['infinity', 'Infinity'], ['plus-infinity', '+Infinity'],
    ['negative-infinity', '-Infinity'], ['lower-infinity', 'infinity'], ['upper-infinity', 'INFINITY'], ['inf', 'Inf'], ['nan', 'NaN'],
    ['hex', '0x10'], ['hex-upper-prefix', '0X1f'], ['hex-upper', '0xFF'], ['hex-empty', '0x'], ['hex-negative', '-0x10'],
    ['hex-plus', '+0x10'], ['hex-bad-digit', '0x1g'], ['hex-fraction', '0x1.5'], ['octal', '0o17'], ['octal-upper', '0O17'],
    ['octal-bad-digit', '0o8'], ['binary', '0b101'], ['binary-upper', '0B101'], ['binary-bad-digit', '0b2'], ['binary-empty', '0b'],
    ['octal-empty', '0o'], ['separator', '1_000'], ['comma', '1,000'], ['padded', ' 12 '], ['newlines', '\n12\n'],
    ['inner-space', '1 2'], ['trailing-letters', '12abc'], ['letters', 'abc'], ['fullwidth', '１２'], ['next-line', '\u00851'],
    ['mongolian-vowel-separator', '\u180e1'], ['trailing-nul', '1\u0000'], ['zero-width-space', '\u200b1'],
    ['beyond-safe', '9007199254740993'], ['safe-half', '9007199254740992.5'], ['thirty-digits', '123456789012345678901234567890'],
    ['e308', '1e308'], ['e309', '1e309'], ['negative-e309', '-1e309'], ['e-324', '1e-324'], ['subnormal-round-up', '2.5e-324'],
    ['below-half-subnormal', '2.4703282292062327e-324'], ['above-half-subnormal', '2.4703282292062328e-324'], ['e-400', '1e-400'],
    ['long-fraction', `0.${'0'.repeat(400)}1`], ['long-integer', `1${'0'.repeat(400)}`], ['long-exact', `${'9'.repeat(30)}.${'9'.repeat(30)}`],
    ['hex-safe-max', '0x1fffffffffffff'], ['hex-tie-down', '0x20000000000001'], ['hex-tie-up', '0x20000000000003'],
    ['hex-sticky', '0x200000000000010000000000001'], ['hex-tie-wide', '0x2000000000000100000000000000'], ['hex-65-bits', '0x1FFFFFFFFFFFFFFFF'],
    ['hex-overflow', `0x${'f'.repeat(300)}`], ['hex-just-overflow', `0x${'f'.repeat(256)}`], ['hex-power-1020', `0x1${'0'.repeat(255)}`],
    ['hex-max', `0x1fffffffffffff${'0'.repeat(242)}`], ['hex-leading-zeros', `0x${'0'.repeat(100)}1`],
    ['binary-54-ones', `0b${'1'.repeat(54)}`], ['binary-53-ones', `0b${'1'.repeat(53)}`], ['octal-wide', `0o${'7'.repeat(30)}`],
    ['whitespace-hex', '\u3000 0x10 \ufeff'], ['whitespace-infinity', '\u2028-Infinity\u2029'],
]
write('number-from-text', [
    ...fromText.map(([id, text]) => ({ id, text: str(text) })),
    { id: 'isolated-high', text: units(0xd800) }, { id: 'digit-isolated-high', text: units(49, 0xd800) },
    { id: 'isolated-between-spaces', text: units(0x20, 0xdc00, 0x20) }, { id: 'digits-isolated-whitespace', text: units(0x20, 49, 50, 0x20, 0xd800) },
])

// --- trim, trimStart, trimEnd ---
const trimTexts = [
    ['empty', ''], ['space', ' '], ['padded', '  a  '], ['nbsp', '\u00a0a\u00a0'], ['bom', '\ufeffa\ufeff'],
    ['line-separators', '\u2028a\u2029'], ['next-line-kept', '\u0085a\u0085'], ['mongolian-kept', '\u180ea\u180e'],
    ['zero-width-kept', '\u200ba\u200b'], ['all-whitespace', '\t\n\v\f\r \u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000\ufeff'],
    ['inner-space', 'a b'], ['controls', ' \t\n\r\v\f'], ['emoji', '  😀  '], ['hangul', '\u3000한국어\u3000'], ['no-space', 'abc'],
    ['mixed-edges', '\n\t x y \u00a0\r'],
]
write('trim', [
    ...trimTexts.map(([id, text]) => ({ id, text: str(text) })),
    { id: 'isolated-inside-spaces', text: units(0x20, 0xd800, 0x20) }, { id: 'isolated-low-leading', text: units(0xdc00, 0x20) },
    { id: 'isolated-high-trailing', text: units(0x20, 0x41, 0xdbff) }, { id: 'only-isolated', text: units(0xd800) },
])

// --- PromptMessage / Multimodal round trips ---
const image = (extra = []) => O(['type', S('image')], ['base64', S('data:image/png;base64,AAAA')], ['width', N('512')], ['height', N('256')], ...extra)
write('prompt-message', [
    { id: 'main-site-empty-multimodals', kind: 'message', value: O(['role', S('user')], ['content', S('Hello')], ['memo', S('chat-1')], ['attr', A()], ['multimodals', A()], ['thoughts', A()]) },
    { id: 'main-site-undefined-memo', kind: 'message', value: O(['role', S('assistant')], ['content', S('Reply')], ['memo', U], ['attr', A(S('nameAdded'))], ['multimodals', A()], ['thoughts', A(S('plan'))]) },
    { id: 'main-site-image', kind: 'message', value: O(['role', S('user')], ['content', S('Look')], ['memo', S('chat-2')], ['attr', A()], ['multimodals', A(image())], ['thoughts', A()]) },
    { id: 'image-without-dimensions', kind: 'message', value: O(['role', S('user')], ['content', S('x')], ['multimodals', A(O(['type', S('image')], ['base64', S('data:,')], ['width', U], ['height', U]))]) },
    { id: 'example-assistant', kind: 'message', value: O(['role', S('assistant')], ['content', S('Hi there')], ['name', S('example_assistant')]) },
    { id: 'example-user', kind: 'message', value: O(['role', S('user')], ['content', S('Hello')], ['name', S('example_user')]) },
    { id: 'new-chat-example', kind: 'message', value: O(['role', S('system')], ['content', S('[Start a new chat]')], ['memo', S('NewChatExample')]) },
    { id: 'removable-and-cache-point', kind: 'message', value: O(['role', S('user')], ['content', S('a')], ['memo', S('m')], ['removable', ['bool', true]], ['cachePoint', ['bool', true]]) },
    { id: 'false-flags', kind: 'message', value: O(['role', S('user')], ['content', S('a')], ['removable', ['bool', false]], ['cachePoint', ['bool', false]]) },
    { id: 'shuffled-keys', kind: 'message', value: O(['cachePoint', ['bool', true]], ['thoughts', A(S('t'))], ['removable', ['bool', true]], ['memo', S('m')], ['name', S('n')], ['attr', A(S('x'))], ['content', S('c')], ['role', S('function')]) },
    { id: 'extra-keys', kind: 'message', value: O(['role', S('user')], ['zeta', A(N('1'), N('2'))], ['content', S('x')], ['custom', N('1')], ['alpha', ['null']]) },
    { id: 'extra-index-keys', kind: 'message', value: O(['role', S('user')], ['content', S('x')], ['10', S('ten')], ['2', S('two')], ['tail', ['bool', true]]) },
    { id: 'extra-undefined', kind: 'message', value: O(['role', S('user')], ['content', S('x')], ['gone', U], ['kept', N('-0')]) },
    { id: 'extra-proto-key', kind: 'message', value: O(['role', S('user')], ['content', S('x')], ['__proto__', O(['polluted', ['bool', true]])]) },
    { id: 'extra-nested', kind: 'message', value: O(['role', S('user')], ['content', S('x')], ['meta', O(['list', A(U, N('NaN'), S('\ud800'))], ['empty', O()])]) },
    { id: 'content-escapes', kind: 'message', value: O(['role', S('user')], ['content', S('quote " backslash \\ newline \n tab \t \u0001 \ud800 😀 한국어 \u2028')]) },
    { id: 'empty-content', kind: 'message', value: O(['role', S('system')], ['content', S('')]) },
    { id: 'all-fields', kind: 'message', value: O(['role', S('assistant')], ['content', S('c')], ['name', S('n')], ['memo', S('m')], ['attr', A(S('a1'), S('a2'))], ['multimodals', A(image(), O(['type', S('audio')], ['base64', S('data:audio/mp3;base64,BBBB')]))], ['thoughts', A(S('t1'), S('t2'))], ['removable', ['bool', true]], ['cachePoint', ['bool', false]], ['x', N('1')]) },
    { id: 'multimodal-lua-extra', kind: 'message', value: O(['role', S('user')], ['content', S('x')], ['multimodals', A(image([['caption', S('cat')], ['5', N('5')]]))]) },
    { id: 'multimodal-nonfinite-dimensions', kind: 'message', value: O(['role', S('user')], ['content', S('x')], ['multimodals', A(O(['type', S('video')], ['base64', S('v')], ['width', N('NaN')], ['height', N('-0')]))]) },
    { id: 'standalone-image', kind: 'multimodal', value: image() },
    { id: 'standalone-shuffled', kind: 'multimodal', value: O(['height', N('10')], ['base64', S('b')], ['width', N('20')], ['type', S('signature')]) },
    { id: 'standalone-extra', kind: 'multimodal', value: O(['type', S('audio')], ['note', S('n')], ['base64', S('b')], ['1', N('1')]) },
    { id: 'standalone-isolated-base64', kind: 'multimodal', value: O(['type', S('image')], ['base64', S('\udc00')]) },
])
