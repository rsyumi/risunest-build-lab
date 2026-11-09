// Writes the synthetic case file of the RISUM export oracle (tests/fixtures/rust-core/formats/risum-export.cases.json).
// Add cases here, run `node benchmarks/rust-core-oracles/formats-risum-export.cases.mjs`, then regenerate the expected
// file with RUST_CORE_ORACLE_WRITE=1.
//
// Each case is a stored root module, as the JSON text the store holds for it, and the payload of every asset key it
// names (hex). The oracle writes the file through `exportModuleLegacy` with those payloads in the asset store; the app's
// native RISUM export writes the same module from a store holding the same payloads. `native` is what the native job
// does with the metadata: `ported`, the port's text, or `{declined}` with the `Unsupported` the port answers, where the
// job writes its own layout. A stored text TS never writes itself (`-0`, an object in other than JS key order) comes
// from a placeholder replaced in the file's text.
import { mkdirSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const dir = fileURLToPath(new URL('../../tests/fixtures/rust-core/formats/', import.meta.url))
mkdirSync(dir, { recursive: true })

const cases = []
const ids = new Set()
function add(id, module, payloads = {}, native = 'ported') {
    if (ids.has(id)) throw new Error(`duplicate ${id}`)
    ids.add(id)
    cases.push({ id, module: JSON.parse(JSON.stringify(module)), payloads, native })
}
const bytes = text => Buffer.from(text, 'utf8').toString('hex')
const raw = { '"__NEGATIVE_ZERO__"': '-0', '"__RAW_KEY_ORDER__"': '{"named": "n", "10": "ten", "b": "b", "2": "two", "a": "a"}' }

const lore = (overrides = {}) => ({
    key: 'alpha, beta', secondkey: '', insertorder: 100, comment: 'Lore', content: 'Lore content', mode: 'normal',
    alwaysActive: false, selective: false, ...overrides,
})
add('full', {
    name: 'Synthetic Module', description: 'A synthetic module.', id: 'stored-module-1',
    lorebook: [lore(), lore({ key: '', comment: 'second', content: 'other', mode: 'folder', folder: 'f', insertorder: -5 })],
    regex: [{ comment: 'regex', in: 'a', out: 'b', type: 'editinput', ableFlag: false }],
    trigger: [{ comment: 'trigger', type: 'start', conditions: [], effect: [{ type: 'v2Header', code: '', indent: 0 }] }],
    cjs: '', lowLevelAccess: false, hideIcon: true, backgroundEmbedding: '<style>x</style>', customModuleToggle: '',
    // Array-index keys come first in JS order, as the stored text has them.
    toggles: { 2: 'two', 10: 'ten', named: 'n', 1: 'one' },
    assets: [
        ['first', 'stored-a1', 'png', { tail: 1 }],
        ['duplicate', 'stored-a1', 'png'],
        ['empty', 'stored-empty', 'dat', 'tail'],
        ['second', 'stored-a2', 'webp', null],
    ],
}, { 'stored-a1': bytes('first payload'), 'stored-empty': '', 'stored-a2': bytes('second payload') })
// `assets ??= []` adds the key last; the frames read `assets ?? []`.
add('no-assets', { name: 'No assets', description: 'd', id: 'stored-module-2' })
add('assets-null', { name: 'Null assets', description: 'd', id: 'stored-module-3', assets: null })
add('assets-empty', { name: 'Empty assets', id: 'stored-module-4', assets: [] })
// Numbers as JS writes them: an exponent at 1e21 and above, the digits below.
add('numbers', {
    name: 'Numbers', id: 'stored-module-5',
    numbers: { big: 1e21, twenty: 1e20, huge: 1.5e300, tiny: 1e-7, small: 1.23e-18, frac: 0.1, sum: 0.1 + 0.2, negative: -42, wide: 12345678901234567000 },
    assets: [],
})
add('unicode', {
    name: '모듈 🐿️', description: 'quote " backslash \\ newline \n tab \t controls \u0001\u001f\u007f separators   ',
    id: 'stored-module-6', assets: [['이름 😀', 'stored-ko', 'png']],
}, { 'stored-ko': bytes('한국어 payload') })
add('empty-containers', {
    name: 'Empty', id: 'stored-module-7', list: [], object: {}, nested: [[], {}, [[]], { inner: {} }], assets: [],
})
const deep = {}
let level = deep
for (let depth = 0; depth < 40; depth += 1) { level.next = { depth }; level = level.next }
add('deep', { name: 'Deep', id: 'stored-module-8', deep, assets: [] })
// An own `__proto__` key, which only JSON.parse creates, is one the port declines.
add('proto-key', JSON.parse('{"name": "Proto", "id": "stored-module-9", "extra": {"__proto__": {"polluted": true}, "plain": 1}, "assets": []}'),
    {}, { declined: 'protoKey' })
// JSON.parse reads `-0` and keys in JS order; the stored text holds them as written.
add('negative-zero', { name: 'Negative zero', id: 'stored-module-11', zero: '__NEGATIVE_ZERO__', nested: [1, '__NEGATIVE_ZERO__'], assets: [] })
add('raw-key-order', { name: 'Raw key order', id: 'stored-module-12', mixed: '__RAW_KEY_ORDER__', assets: [] })
// The writer reads only each tuple's key.
add('odd-tuples', {
    name: 'Odd tuples', id: 'stored-module-13',
    assets: [[7, 'stored-o1', null], ['pair', 'stored-o2'], [{ name: 'object' }, 'stored-o1', 3, 'tail']],
}, { 'stored-o1': bytes('odd one'), 'stored-o2': bytes('odd two') })
// Declined, the job writes `assets ?? []` too.
add('proto-key-assets-null', JSON.parse('{"name": "Proto null", "id": "stored-module-10", "extra": {"__proto__": {"polluted": true}}, "assets": null}'),
    {}, { declined: 'protoKey' })
add('proto-key-no-assets', JSON.parse('{"name": "Proto absent", "id": "stored-module-14", "extra": {"__proto__": {"polluted": true}}}'),
    {}, { declined: 'protoKey' })

let text = `${JSON.stringify({ schema: 1, cases }, null, 2)}\n`
for (const [placeholder, value] of Object.entries(raw)) text = text.replaceAll(placeholder, value)
writeFileSync(`${dir}risum-export.cases.json`, text)
console.log(`wrote ${cases.length} cases`)
