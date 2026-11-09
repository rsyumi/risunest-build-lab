// Writes tests/fixtures/rust-core/lorebook/selection.cases.json, the synthetic input of lorebook.oracle.test.ts.
// Usage: node benchmarks/rust-core-oracles/lorebook.cases.mjs, then regenerate the expected file with the oracle.
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const base = {
    root: {
        username: 'Tester', loreBookDepth: 5, loreBookToken: 800, modules: [], enabledModules: [], moduleIntergration: '',
        personas: [], templateDefaultVariables: '', aiModel: 'custom', currentPluginProvider: 'lore-oracle-tokenizer',
        useTokenizerCaching: false, customTokenizer: '',
    },
    character: { chaId: 'char-a', name: 'Aria', chatPage: 0, globalLore: [], defaultVariables: '', type: 'character' },
    chat: { id: 'chat-a', name: 'Chat', note: '', localLore: [], message: [], scriptstate: {}, fmIndex: -1 },
}

const L = (content, fields = {}) => ({
    key: '', secondkey: '', insertorder: 100, comment: '', content, mode: 'normal', alwaysActive: false, selective: false,
    useRegex: false, ...fields,
})
const u = data => ({ role: 'user', data })
const c = (data, extra = {}) => ({ role: 'char', data, ...extra })

const cases = []
function add(id, spec) {
    const entry = { id }
    const character = { ...(spec.character ?? {}) }
    const chat = { ...(spec.chat ?? {}) }
    if (spec.lore) character.globalLore = spec.lore
    if (spec.local) chat.localLore = spec.local
    if (spec.messages) chat.message = spec.messages
    if (spec.root) entry.root = spec.root
    if (Object.keys(character).length) entry.character = character
    if (Object.keys(chat).length) entry.chat = chat
    if (spec.others) entry.others = spec.others
    if (spec.random) entry.random = spec.random
    if (spec.native) entry.native = spec.native
    if (spec.orderedObservation) entry.orderedObservation = spec.orderedObservation
    cases.push(entry)
}

const dragon = (fields = {}) => L('Dragons breathe fire.', { key: 'dragon', comment: 'Dragon', ...fields })

// Keys.
add('key-single-match', { lore: [dragon()], messages: [u('I saw a dragon today')] })
add('key-no-match', { lore: [dragon({ key: 'griffin' })], messages: [u('I saw a dragon today')] })
add('key-case-insensitive', { lore: [dragon({ key: 'DRAGON' })], messages: [u('a Dragon appears')] })
add('key-spaces-removed-from-key', { lore: [dragon({ key: 'red dragon' })], messages: [u('the reddragon roars')] })
add('key-spaces-removed-from-text', { lore: [dragon({ key: 'reddragon' })], messages: [u('a red dragon roars')] })
add('key-second-of-several', { lore: [dragon({ key: 'griffin, dragon ,wyrm' })], messages: [u('one dragon')] })
add('key-only-separators', { lore: [dragon({ key: ' , ,' })], messages: [u('anything at all')] })
add('key-in-char-message', { lore: [dragon()], messages: [u('hello'), c('the dragon wakes')] })
add('key-hidden-in-comment-macro', { lore: [dragon()], messages: [u('a {{// dragon}} hides')] })
add('key-hidden-in-comment-call', { lore: [dragon()], messages: [u('{{comment:dragon}} rest')] })
add('key-joined-across-macro', { lore: [dragon()], messages: [u('dra{{//x}}gon')] })
add('key-macro-across-lines-kept', { lore: [dragon()], messages: [u('{{//a\nb}} dragon')] })
add('key-unicode-lowercase', { lore: [dragon({ key: 'ÉCLAIR' })], messages: [u('un éclair au chocolat')] })
add('key-final-sigma-differs', { lore: [dragon({ key: 'ΑΣ' })], messages: [u('ΑΣΑ')] })
add('key-final-sigma-matches', { lore: [dragon({ key: 'ΟΔΟΣ' })], messages: [u('ΟΔΟΣ ΚΑΙ')] })
add('key-dotted-capital-i', { lore: [dragon({ key: 'İz' })], messages: [u('İZMIR')] })
add('key-always-active-without-key', { lore: [L('Always here.', { alwaysActive: true, comment: 'Always' })], messages: [u('hi')] })
add('key-empty-is-skipped', { lore: [L('Never here.', { comment: 'Never' })], messages: [u('hi')] })
add('key-later-message-source', { lore: [dragon()], messages: [u('nothing'), c('still nothing'), u('a dragon')] })
add('key-first-message-logged-once', { lore: [dragon()], messages: [u('dragon one'), u('dragon two')] })
add('key-username-comment-macro', { root: { username: '//hidden' }, lore: [dragon()], messages: [u('dragon')] })
add('key-two-entries', {
    lore: [dragon({ insertorder: 10 }), L('Wyrms coil.', { key: 'wyrm', comment: 'Wyrm', insertorder: 30 })],
    messages: [u('a dragon and a wyrm')],
})
add('key-local-and-global', {
    lore: [dragon()], local: [L('Local lore.', { key: 'dragon', comment: 'Local' })], messages: [u('dragon')],
})
add('key-empty-message-list', { lore: [dragon(), L('Always.', { alwaysActive: true })], messages: [] })

// Secondary keys.
add('secondary-both-match', { lore: [dragon({ selective: true, secondkey: 'fire' })], messages: [u('dragon fire')] })
add('secondary-miss', { lore: [dragon({ selective: true, secondkey: 'ice' })], messages: [u('dragon fire')] })
add('secondary-ignored-without-selective', { lore: [dragon({ secondkey: 'ice' })], messages: [u('dragon fire')] })
add('secondary-empty', { lore: [dragon({ selective: true, secondkey: '' })], messages: [u('dragon')] })
add('secondary-several', { lore: [dragon({ selective: true, secondkey: 'ice, fire' })], messages: [u('dragon fire')] })
add('secondary-primary-miss', { lore: [dragon({ key: 'wyrm', selective: true, secondkey: 'fire' })], messages: [u('dragon fire')] })
add('secondary-null', { lore: [dragon({ selective: true, secondkey: null })], messages: [u('dragon')] })
add('secondary-other-message', { lore: [dragon({ selective: true, secondkey: 'fire' })], messages: [u('fire first'), c('then dragon')] })

// Full-word matching.
const cat = (fields = {}) => L('Cats purr.', { key: 'cat', comment: 'Cat', ...fields })
add('full-word-rejects-substring', { character: { loreSettings: { fullWordMatching: true } }, lore: [cat()], messages: [u('concatenate')] })
add('full-word-accepts-word', { character: { loreSettings: { fullWordMatching: true } }, lore: [cat()], messages: [u('a cat sat')] })
add('full-word-decorator', { lore: [cat({ content: '@@match_full_word\nCats purr.' })], messages: [u('concat')] })
add('partial-word-decorator', {
    character: { loreSettings: { fullWordMatching: true } }, lore: [cat({ content: '@@match_partial_word\nCats purr.' })],
    messages: [u('concat')],
})
add('full-word-punctuation', { character: { loreSettings: { fullWordMatching: true } }, lore: [cat()], messages: [u('a cat. sat')] })
add('full-word-key-with-space', {
    character: { loreSettings: { fullWordMatching: true } }, lore: [cat({ key: 'red cat' })], messages: [u('a red cat')],
})
add('full-word-case', { character: { loreSettings: { fullWordMatching: true } }, lore: [cat({ key: 'CAT' })], messages: [u('Cat')] })
add('full-word-double-space', { character: { loreSettings: { fullWordMatching: true } }, lore: [cat()], messages: [u('a  cat')] })
add('full-word-null-setting', { character: { loreSettings: { fullWordMatching: null } }, lore: [cat()], messages: [u('concat')] })

// Regex keys.
const rx = (key, fields = {}) => L('Regex lore.', { key, comment: 'Regex', useRegex: true, ...fields })
add('regex-match-insensitive', { lore: [rx('/drag(on)?/i')], messages: [u('A DRAGON')] })
add('regex-no-match', { lore: [rx('/griffin/')], messages: [u('a dragon')] })
add('regex-invalid-source', { lore: [rx('/([/')], messages: [u('([/')] })
add('regex-not-a-literal', { lore: [rx('dragon')], messages: [u('dragon')] })
add('regex-one-invalid-key-fails-all', { lore: [rx('/dragon/, /(/')], messages: [u('dragon')] })
add('regex-global-flag', { lore: [rx('/dragon/g')], messages: [u('dragon'), u('dragon again')] })
add('regex-reads-raw-text', { lore: [rx('/\\{\\{\\/\\/secret\\}\\}/')], messages: [u('x {{//secret}} y')] })
add('regex-case-sensitive', { lore: [rx('/Dragon/')], messages: [u('dragon')] })
add('regex-secondary', { lore: [rx('/dragon/', { selective: true, secondkey: '/fi(re)?/' })], messages: [u('dragon fire')] })
add('regex-additional-keys', { lore: [rx('/dragon/', { content: '@@additional_keys /wing/\nWings.' })], messages: [u('dragon wing')] })
add('regex-comma-splits-key', { lore: [rx('/a{1,2}/')], messages: [u('aa')] })
add('regex-multiline', { lore: [rx('/^dragon$/m')], messages: [u('hello\ndragon')] })
add('regex-recursive-content', {
    lore: [L('The ember glows.', { alwaysActive: true, comment: 'Ember' }), rx('/emb(er)?/', { comment: 'Fire' })], messages: [u('hi')],
})
add('regex-unknown-flag', { lore: [rx('/abc/x')], messages: [u('abc')] })
add('regex-duplicate-flag', { lore: [rx('/a/gg')], messages: [u('a')] })
add('regex-exclude-keys', { lore: [rx('/dragon/', { content: '@@exclude_keys /fire/\nNo fire.' })], messages: [u('dragon fire')] })
add('regex-sticky-flag', { lore: [rx('/dragon/y')], messages: [u('dragon first'), u('a dragon')] })
add('regex-empty-pattern', { lore: [rx('//')], messages: [u('anything')] })

// Scan depth.
const old = [u('a dragon long ago'), c('reply'), u('later'), c('latest')]
add('depth-root-excludes-old', { root: { loreBookDepth: 1 }, lore: [dragon()], messages: old })
add('depth-root-includes', { root: { loreBookDepth: 4 }, lore: [dragon()], messages: old })
add('depth-character-override', { character: { loreSettings: { scanDepth: 1 } }, lore: [dragon()], messages: old })
add('depth-decorator', { lore: [dragon({ content: '@@scan_depth 1\nDragons.' })], messages: old })
add('depth-decorator-missing-argument', { root: { loreBookDepth: 1 }, lore: [dragon({ content: '@@scan_depth\nDragons.' })], messages: old })
add('depth-decorator-zero', { lore: [dragon({ content: '@@scan_depth 0\nDragons.' })], messages: [u('dragon')] })
add('depth-fractional-setting', { character: { loreSettings: { scanDepth: 3.5 } }, lore: [dragon()], messages: old })
add('depth-negative', { root: { loreBookDepth: -1 }, lore: [dragon()], messages: [u('dragon')] })
add('depth-two-entries', {
    lore: [dragon({ content: '@@scan_depth 1\nNear.' }), L('Far.', { key: 'dragon', comment: 'Far', content: '@@scan_depth 4\nFar.' })],
    messages: old,
})
add('depth-beyond-messages', { root: { loreBookDepth: 40 }, lore: [dragon()], messages: old })
add('depth-decorator-not-a-number', { root: { loreBookDepth: 1 }, lore: [dragon({ content: '@@scan_depth abc\nDragons.' })], messages: old })
add('depth-null-setting', { character: { loreSettings: { scanDepth: null } }, root: { loreBookDepth: 1 }, lore: [dragon()], messages: old })

// Recursion.
const guard = (fields = {}) => L('It guards the treasure.', { key: 'dragon', comment: 'Guard', ...fields })
const treasure = (fields = {}) => L('Gold and gems.', { key: 'treasure', comment: 'Treasure', ...fields })
add('recursion-same-pass', { lore: [guard(), treasure()], messages: [u('a dragon')] })
add('recursion-next-pass', { lore: [treasure(), guard()], messages: [u('a dragon')] })
add('recursion-disabled', { character: { loreSettings: { recursiveScanning: false } }, lore: [treasure(), guard()], messages: [u('a dragon')] })
add('recursion-unrecursive', { lore: [treasure(), guard({ content: '@@unrecursive\nIt guards the treasure.' })], messages: [u('a dragon')] })
add('recursion-recursive-overrides-setting', {
    character: { loreSettings: { recursiveScanning: false } },
    lore: [treasure(), guard({ content: '@@recursive\nIt guards the treasure.' })], messages: [u('a dragon')],
})
add('recursion-no-recursive-search', {
    lore: [treasure({ content: '@@no_recursive_search\nGold and gems.' }), guard()], messages: [u('a dragon')],
})
add('recursion-chain', {
    lore: [L('Shiny gems.', { key: 'gold', comment: 'Gems' }), L('Gold coins.', { key: 'treasure', comment: 'Gold' }), guard()],
    messages: [u('a dragon')],
})
add('recursion-uses-decorated-content', {
    lore: [guard({ content: '@@depth 2\nIt guards the treasure.' }), treasure()], messages: [u('dragon')],
})
add('recursion-full-word', {
    character: { loreSettings: { fullWordMatching: true } },
    lore: [L('Treasures and more.', { key: 'dragon', comment: 'Plural' }), treasure()], messages: [u('dragon')],
})
add('recursion-default-source', { lore: [guard({ comment: '' }), treasure()], messages: [u('a dragon')] })
add('recursion-probability-redraw', {
    lore: [treasure({ content: '@@probability 50\nGold and gems.' }), guard()], messages: [u('a dragon')], random: [0.9, 0.1],
})
add('recursion-stops-without-new-content', {
    character: { loreSettings: { recursiveScanning: true } },
    lore: [guard(), L('Unrelated.', { key: 'griffin', comment: 'Griffin' })], messages: [u('dragon')],
})
add('recursion-always-active-feeds-keys', {
    lore: [L('A dragon sleeps.', { alwaysActive: true, comment: 'Sleep' }), guard(), treasure()], messages: [u('hi')],
})

// Probability.
const chance = (arg, fields = {}) => dragon({ content: `@@probability${arg === undefined ? '' : ' ' + arg}\nMaybe.`, ...fields })
add('probability-passes', { lore: [chance(50)], messages: [u('dragon')], random: [0.3] })
add('probability-fails', { lore: [chance(50)], messages: [u('dragon')], random: [0.7] })
add('probability-missing-argument', { lore: [chance()], messages: [u('dragon')], random: [0.99] })
add('probability-zero-boundary', { lore: [chance(0)], messages: [u('dragon')], random: [0] })
add('probability-hundred', { lore: [chance(100)], messages: [u('dragon')], random: [0.999] })
add('probability-tape-exhausted', { lore: [chance(50)], messages: [u('dragon')], random: [] })
add('probability-always-active', { lore: [L('@@probability 40\nSometimes.', { alwaysActive: true, comment: 'Some' })], messages: [], random: [0.5] })
add('probability-two-draws', { lore: [dragon({ content: '@@probability 90\n@@probability 10\nRare.' })], messages: [u('dragon')], random: [0.2, 0.5] })
add('probability-not-a-number', { lore: [chance('abc')], messages: [u('dragon')], random: [0.5] })
add('probability-float-product', { lore: [chance(30)], messages: [u('dragon')], random: [0.3] })
add('probability-inactive-skips-search', { lore: [chance(10)], messages: [u('dragon')], random: [0.5] })
add('probability-each-entry', {
    lore: [chance(50, { comment: 'One' }), chance(50, { comment: 'Two' }), chance(50, { comment: 'Three' })],
    messages: [u('dragon')], random: [0.1, 0.9, 0.4, 0.6],
})

// Order and priority.
const always = (content, fields = {}) => L(content, { alwaysActive: true, ...fields })
add('order-ascending-output', {
    lore: [always('Ten.', { insertorder: 10, comment: 'Ten' }), always('Thirty.', { insertorder: 30, comment: 'Thirty' }),
        always('Twenty.', { insertorder: 20, comment: 'Twenty' })],
})
add('order-priority-decides-budget', {
    root: { loreBookToken: 3 },
    lore: [always('@@priority 5\nAAAAAAAA', { insertorder: 50, comment: 'Low' }), always('@@priority 10\nBBBBBBBB', { insertorder: 1, comment: 'High' })],
})
add('order-equal-priority-stable', {
    root: { loreBookToken: 4 },
    lore: [always('AAAA', { comment: 'A' }), always('BBBB', { comment: 'B' }), always('CCCC', { comment: 'C' }), always('DDDD', { comment: 'D' })],
})
add('order-priority-not-a-number', {
    lore: [always('@@priority abc\nNaN one.', { insertorder: 5, comment: 'N1' }), always('Plain.', { insertorder: 7, comment: 'P' }),
        always('@@priority x\nNaN two.', { insertorder: 6, comment: 'N2' })],
})
add('order-ignore-on-max-context', {
    root: { loreBookToken: 2 },
    lore: [always('@@ignore_on_max_context\nIgnored.', { insertorder: 900, comment: 'Ignored' }), always('Kept.', { insertorder: 1, comment: 'Kept' })],
})
add('order-negative-and-fractional', {
    lore: [always('A.', { insertorder: -5, comment: 'A' }), always('B.', { insertorder: 2.5, comment: 'B' }), always('C.', { insertorder: 2.25, comment: 'C' })],
})
add('order-missing-insertorder', {
    lore: [always('A.', { insertorder: undefined, comment: 'A' }), always('B.', { insertorder: 3, comment: 'B' })],
})
add('order-null-insertorder', {
    lore: [always('A.', { insertorder: null, comment: 'A' }), always('B.', { insertorder: -1, comment: 'B' }), always('C.', { insertorder: 1, comment: 'C' })],
})

// Large sorts: duplicate orders and NaN priorities exercise V8's TimSort merges and galloping.
let seed = 12345
const next = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648
function sortCase(id, count, range, nanRate, priorityRate, budget = 1e9) {
    const lore = []
    for (let index = 0; index < count; index++) {
        const roll = next()
        let content = `entry ${index} text`.padEnd(8 + Math.floor(next() * 12), '.')
        if (roll < nanRate) content = `@@priority n${index}\n${content}`
        else if (roll < nanRate + priorityRate) content = `@@priority ${Math.floor(next() * range)}\n${content}`
        lore.push(always(content, { insertorder: Math.floor(next() * range), comment: `S${index}` }))
    }
    add(id, { root: { loreBookToken: budget }, lore })
}
sortCase('sort-12', 12, 4, 0.2, 0.2)
sortCase('sort-40', 40, 6, 0.25, 0.1)
sortCase('sort-70', 70, 10, 0.2, 0.2)
sortCase('sort-150', 150, 5, 0.3, 0.1)
sortCase('sort-200', 200, 20, 0.15, 0.25)
sortCase('sort-100-budget', 100, 8, 0.2, 0.2, 120)
sortCase('sort-90-no-nan', 90, 3, 0, 0.5)
sortCase('sort-130-runs', 130, 2, 0.5, 0)

// Token budget.
add('budget-keeps-first-two', {
    root: { loreBookToken: 10 },
    lore: [always('aaaaaaaaaaaaaaaaaaaa', { comment: 'A', insertorder: 3 }), always('bbbbbbbbbbbbbbbbbbbb', { comment: 'B', insertorder: 2 }),
        always('cccccccccccccccccccc', { comment: 'C', insertorder: 1 })],
})
add('budget-greedy-skip', {
    root: { loreBookToken: 10 },
    lore: [always('a'.repeat(32), { comment: 'Big', insertorder: 3 }), always('b'.repeat(20), { comment: 'Mid', insertorder: 2 }),
        always('c'.repeat(8), { comment: 'Small', insertorder: 1 })],
})
add('budget-zero-keeps-empty', { root: { loreBookToken: 0 }, lore: [always('', { comment: 'Empty' }), always('x', { comment: 'X' })] })
add('budget-character-override', {
    root: { loreBookToken: 1000 }, character: { loreSettings: { tokenBudget: 2 } },
    lore: [always('12345678', { comment: 'Two' }), always('1234', { comment: 'One' })],
})
add('budget-negative', { root: { loreBookToken: -1 }, lore: [always('', { comment: 'Empty' }), always('x', { comment: 'X' })] })
add('budget-null-override', {
    root: { loreBookToken: 1 }, character: { loreSettings: { tokenBudget: null } }, lore: [always('1234', { comment: 'A' }), always('12345', { comment: 'B' })],
})
add('budget-exact-fit', { root: { loreBookToken: 3 }, lore: [always('12345678', { comment: 'A' }), always('1234', { comment: 'B' })] })

// Position, depth and role decorators.
const deco = (content, fields = {}) => always(content, { comment: 'Deco', ...fields })
add('decorator-depth', { lore: [deco('@@depth 3\nDeep.')] })
add('decorator-reverse-depth', { lore: [deco('@@reverse_depth 2\nReverse.')] })
add('decorator-end', { lore: [deco('@@end\nEnd.')] })
add('decorator-fallback-line', { lore: [deco('@@depth abc\n@@@depth 4\nFallback.')] })
add('decorator-fallback-skipped', { lore: [deco('@@depth 2\n@@@depth 5\nNo fallback.')] })
add('decorator-role-user', { lore: [deco('@@role user\nUser.')] })
add('decorator-role-assistant', { lore: [deco('@@role assistant\nAssistant.')] })
add('decorator-role-unknown-fallback', { lore: [deco('@@role bogus\n@@@role user\nFallback role.')] })
add('decorator-position-named', { lore: [deco('@@position after_desc\nAfter.')] })
add('decorator-position-pt', { lore: [deco('@@position pt_custom\nCustom.')] })
add('decorator-position-unknown', { lore: [deco('@@position nowhere\nNowhere.')] })
add('decorator-instruct-and-icon', { lore: [deco('@@instruct_depth 3\n@@is_user_icon x\n@@instruct_scan_depth 1\nIgnored.')] })
add('decorator-disable-ui-prompt', { lore: [deco('@@disable_ui_prompt system_prompt\n@@disable_ui_prompt other\nDisabled.')] })
add('decorator-unknown', { lore: [deco('@@foo bar\nUnknown.')] })
add('decorator-depth-parse-int', {
    lore: [deco('@@depth 2.9\nA.', { comment: 'A' }), deco('@@depth -3\nB.', { comment: 'B' }), deco('@@depth 0x10\nC.', { comment: 'C' }),
        deco('@@depth  7abc\nD.', { comment: 'D' })],
})
add('decorator-only-decorators', { lore: [deco('@@depth 1\n@@role user')] })
add('decorator-trimmed-lines', { lore: [deco('  @@depth 1  \n  Text with spaces.  \n  more  ')] })
add('decorator-after-content', { lore: [deco('Text first.\n@@depth 3')] })
add('decorator-triple-end', { lore: [deco('@@@end\nTriple end.')] })
add('decorator-missing-argument', { lore: [deco('@@depth\nNo argument.')] })
add('decorator-argument-list', { lore: [dragon({ content: '@@additional_keys  fire , ,wing \nArgs.' })], messages: [u('dragon wing')] })
add('decorator-empty-name', { lore: [deco('@@\n@@@depth 6\nEmpty name.')] })
add('decorator-crlf', { lore: [deco('@@depth 2\r\nCarriage.\r\nReturn.')] })

// Injection.
const target = (fields = {}) => always('The target text here.', { comment: 'Target', insertorder: 10, ...fields })
add('inject-append', { lore: [target(), always('@@inject_lore Target\nAppended.', { comment: 'Inj' })] })
add('inject-prepend', { lore: [target(), always('@@inject_lore Target\n@@inject_prepend\nPrepended.', { comment: 'Inj' })] })
add('inject-replace', { lore: [target(), always('@@inject_lore Target\n@@inject_replace text\nSWAP', { comment: 'Inj' })] })
add('inject-replace-match-pattern', { lore: [target(), always('@@inject_lore Target\n@@inject_replace text\n[$&]', { comment: 'Inj' })] })
add('inject-replace-dollar-patterns', {
    lore: [target(), always("@@inject_lore Target\n@@inject_replace target\n<$$|$`|$'|$1|$<n>|$>", { comment: 'Inj' })],
})
add('inject-missing-target', { lore: [target(), always('@@inject_lore Nowhere\nLost.', { comment: 'Inj' })] })
add('inject-at-position', { lore: [always('@@inject_at chat_prompt\n@@inject_prepend x\nAt.', { comment: 'At' })] })
add('inject-location-joined', {
    lore: [target({ comment: 'Two Words' }), always('@@inject_lore Two, Words\nJoined.', { comment: 'Inj' })],
})
add('inject-default-source', { lore: [target({ comment: '' }), always('@@inject_lore lorebook 0\nDefault.', { comment: 'Inj' })] })
add('inject-over-budget', {
    root: { loreBookToken: 6 }, lore: [target(), always('@@inject_lore Target\n@@priority -5\nToo long to fit here.', { comment: 'Inj' })],
})
add('inject-two-into-one', {
    lore: [target(), always('@@inject_lore Target\nFirst.', { comment: 'I1', insertorder: 5 }),
        always('@@inject_lore Target\nSecond.', { comment: 'I2', insertorder: 7 })],
})
add('inject-replace-not-found', { lore: [target(), always('@@inject_lore Target\n@@inject_replace absent\nX', { comment: 'Inj' })] })
add('inject-target-over-budget', {
    root: { loreBookToken: 3 }, lore: [target({ content: '@@priority -1\nThe target text here.' }), always('@@inject_lore Target\nShort', { comment: 'Inj' })],
})
add('inject-replace-then-lore', { lore: [target(), always('@@inject_replace here\n@@inject_lore Target\nTHERE', { comment: 'Inj' })] })
add('inject-replace-empty-param', { lore: [target(), always('@@inject_lore Target\n@@inject_replace\nStart:', { comment: 'Inj' })] })
add('inject-first-matching-source', {
    lore: [target({ insertorder: 1 }), target({ content: 'Second target.', insertorder: 2 }), always('@@inject_lore Target\nWhich.', { comment: 'Inj' })],
})

// Activation decorators.
add('activate-without-match', { lore: [dragon({ key: 'zzz', content: '@@activate\nForced.' })], messages: [u('dragon')] })
add('dont-activate-with-match', { lore: [dragon({ content: '@@dont_activate\nBlocked.' })], messages: [u('dragon')] })
add('activate-only-after-too-early', { lore: [dragon({ content: '@@activate_only_after 3\nLater.' })], messages: [u('dragon')] })
add('activate-only-after-reached', { lore: [dragon({ content: '@@activate_only_after 3\nLater.' })], messages: [u('x'), u('dragon')] })
add('activate-only-every-even', { lore: [dragon({ content: '@@activate_only_every 2\nEven.' })], messages: [u('dragon')] })
add('activate-only-every-odd', { lore: [dragon({ content: '@@activate_only_every 2\nEven.' })], messages: [u('x'), u('dragon')] })
add('activate-only-every-zero', { lore: [dragon({ content: '@@activate_only_every 0\nNever.' })], messages: [u('dragon')] })
add('activate-only-after-fallback', { lore: [dragon({ key: 'zzz', content: '@@activate_only_after abc\n@@@activate\nFallback.' })], messages: [u('x')] })
add('greeting-matches', { chat: { fmIndex: 0 }, lore: [dragon({ content: '@@is_greeting 1\nGreeting.' })], messages: [u('dragon')] })
add('greeting-differs', { lore: [dragon({ content: '@@is_greeting 1\nGreeting.' })], messages: [u('dragon')] })
add('greeting-null-index', { chat: { fmIndex: null }, lore: [dragon({ content: '@@is_greeting 0\nGreeting.' })], messages: [u('dragon')] })
add('greeting-string-index', { chat: { fmIndex: '1' }, lore: [dragon({ content: '@@is_greeting 2\nGreeting.' })], messages: [u('dragon')] })
add('additional-keys-present', { lore: [dragon({ content: '@@additional_keys fire\nBoth.' })], messages: [u('dragon fire')] })
add('additional-keys-absent', { lore: [dragon({ content: '@@additional_keys fire\nBoth.' })], messages: [u('dragon')] })
add('exclude-keys-present', { lore: [dragon({ content: '@@exclude_keys fire\nNot fire.' })], messages: [u('dragon fire')] })
add('exclude-keys-absent', { lore: [dragon({ content: '@@exclude_keys ice\nNot ice.' })], messages: [u('dragon fire')] })
add('exclude-keys-all-one-message', { lore: [dragon({ content: '@@exclude_keys_all fire,ice\nAll.' })], messages: [u('dragon fire ice')] })
add('exclude-keys-all-every-message', {
    lore: [dragon({ content: '@@exclude_keys_all fire,ice\nAll.' })], messages: [u('dragon fire ice'), u('only fire')],
})
add('exclude-keys-all-no-messages', { lore: [dragon({ content: '@@scan_depth 0\n@@exclude_keys_all fire\n@@activate\nAll.' })], messages: [u('dragon')] })
add('exclude-keys-all-empty-scan', { lore: [L('@@scan_depth 0\n@@exclude_keys_all fire\nAll.', { key: 'x', comment: 'Empty' })], messages: [u('x')] })
add('activate-then-dont', { lore: [dragon({ content: '@@activate\n@@dont_activate\nLast wins.' })], messages: [u('dragon')] })
add('activate-needs-key-or-always', { lore: [L('@@activate\nNo key.', { comment: 'NoKey' })], messages: [u('x')] })
add('activate-only-after-fraction', { lore: [dragon({ content: '@@activate_only_after 2.7\nFraction.' })], messages: [u('dragon')] })
add('additional-keys-full-word', {
    lore: [dragon({ content: '@@match_full_word\n@@additional_keys fire\nWords.' })], messages: [u('dragon firestorm')],
})
add('exclude-before-primary-order', {
    lore: [dragon({ content: '@@exclude_keys wyrm\n@@additional_keys fire\nOrder.' })], messages: [u('dragon fire wyrm')],
})

// Keep and don't activate after match.
const keep = (fields = {}) => dragon({ id: 'k1', content: '@@keep_activate_after_match\nKept.', comment: 'Keep', ...fields })
const dont = (fields = {}) => dragon({ id: 'd1', content: '@@dont_activate_after_match\nOnce.', comment: 'Dont', ...fields })
add('keep-writes-on-match', { lore: [keep()], messages: [u('dragon')] })
add('keep-forced-by-state', { chat: { scriptstate: { '$__internal_ka_k1': 'true' } }, lore: [keep()], messages: [u('nothing')] })
add('keep-hash-suffix', { lore: [keep({ id: undefined })], messages: [u('dragon')] })
add('dont-writes-on-match', { lore: [dont()], messages: [u('dragon')] })
add('dont-forced-by-state', { chat: { scriptstate: { '$__internal_da_d1': 'true' } }, lore: [dont()], messages: [u('dragon')] })
add('keep-forced-by-character-default', { character: { defaultVariables: 'x=1\n__internal_ka_k1=true\n' }, lore: [keep()], messages: [u('none')] })
add('keep-forced-by-template-default', { root: { templateDefaultVariables: '__internal_ka_k1=true' }, lore: [keep()], messages: [u('none')] })
add('keep-null-state-uses-default', {
    chat: { scriptstate: { '$__internal_ka_k1': null } }, character: { defaultVariables: '__internal_ka_k1=true' }, lore: [keep()], messages: [u('none')],
})
add('keep-numeric-id', { lore: [keep({ id: 7 })], messages: [u('dragon')] })
add('keep-boolean-id', { lore: [keep({ id: true })], messages: [u('dragon')] })
add('keep-overwrites-false', { chat: { scriptstate: { '$other': 'x', '$__internal_ka_k1': 'false' } }, lore: [keep()], messages: [u('dragon')] })
add('keep-over-budget-still-writes', { root: { loreBookToken: 0 }, lore: [keep()], messages: [u('dragon')] })
add('keep-fallback-line', { lore: [keep({ content: '@@keep_activate_after_match\n@@@dont_activate\nFallback.' })], messages: [u('dragon')] })
add('keep-two-entries-in-order', {
    lore: [keep({ id: 'b' }), keep({ id: 'a', comment: 'Keep A' }), dont({ id: 'c' })], messages: [u('dragon')],
})
add('keep-child-hash', {
    lore: [L('@@keep_activate_after_match\nParent text.', { key: 'zzz', comment: 'Parent', mode: 'normal' }),
        L('child placeholder', { key: 'zzz', mode: 'child', comment: 'Child' })],
    messages: [u('x')],
})
add('keep-boolean-state', { chat: { scriptstate: { '$__internal_ka_k1': true } }, lore: [keep()], messages: [u('none')] })
add('dont-visible-to-later-entry', {
    lore: [dont({ id: 's' }), dont({ id: 's', comment: 'Dont again' })], messages: [u('dragon')],
})
add('dont-visible-next-pass', {
    lore: [dont({ id: 's', key: 'treasure', comment: 'Late' }), dont({ id: 's', content: '@@dont_activate_after_match\nThe treasure.' })],
    messages: [u('dragon')],
})
add('keep-numeric-state', { chat: { scriptstate: { '$__internal_ka_k1': 1 } }, lore: [keep()], messages: [u('dragon')] })
add('keep-and-dont-together', {
    lore: [dragon({ id: 'kd', content: '@@keep_activate_after_match\n@@dont_activate_after_match\nBoth.' })], messages: [u('dragon')],
})

// Child entries.
add('child-takes-inactive-parent', {
    lore: [L('Parent content.', { key: 'zzz', comment: 'Parent', id: 'p1' }), L('Child content.', { key: 'x', comment: 'Child', id: 'p1', mode: 'child' })],
    messages: [u('x')],
})
add('child-parent-active', {
    lore: [L('Parent content.', { key: 'x', comment: 'Parent', id: 'p1' }), L('Child content.', { key: 'x', comment: 'Child', id: 'p1', mode: 'child' })],
    messages: [u('x')],
})
add('child-without-parent', { lore: [L('Child content.', { key: 'x', comment: 'Child', id: 'p9', mode: 'child' })], messages: [u('x')] })
add('child-undefined-ids', {
    lore: [L('First.', { key: 'zzz', comment: 'First' }), L('Child.', { key: 'x', comment: 'Child', mode: 'child' })], messages: [u('x')],
})
add('child-without-key-skipped', {
    lore: [L('Parent.', { key: 'zzz', comment: 'Parent', id: 'p1' }), L('Child.', { key: '', comment: 'Child', id: 'p1', mode: 'child' })],
    messages: [u('x')],
})
add('child-empty-parent-comment', {
    lore: [L('Parent.', { key: 'zzz', comment: '', id: 'p1' }), L('Child.', { key: 'x', comment: 'Child', id: 'p1', mode: 'child' })],
    messages: [u('x')],
})
add('child-parent-after', {
    lore: [L('Child.', { key: 'x', comment: 'Child', id: 'p1', mode: 'child' }), L('Parent.', { key: 'zzz', comment: 'Parent', id: 'p1' })],
    messages: [u('x')],
})
add('child-recursion-second-pass', {
    lore: [L('Parent mentions treasure.', { key: 'zzz', comment: 'Parent', id: 'p1' }),
        L('Child.', { key: 'x', comment: 'Child', id: 'p1', mode: 'child' }), treasure()],
    messages: [u('x')],
})

// Folders and lorePlus fields.
add('folder-entry-inactive', {
    lore: [L('', { key: 'folder:f1', comment: 'Folder', mode: 'folder' }), dragon({ folder: 'folder:f1' })], messages: [u('dragon')],
})
add('folder-entry-always-active', { lore: [L('', { key: 'folder:f2', comment: 'Folder', mode: 'folder', alwaysActive: true })], messages: [] })
add('lore-plus-fields', {
    lore: [dragon({ extentions: { risu_case_sensitive: true, depth: 3 }, activationPercent: 50, loreCache: { key: 'x', data: ['y'] }, bookVersion: 2 })],
    messages: [u('DRAGON')],
})
add('folder-key-in-message', {
    lore: [L('Folder text.', { key: 'folder:f3', comment: 'Folder', mode: 'folder' })], messages: [u('see folder:f3 here')],
})
add('folder-member-selective', {
    lore: [L('', { key: 'folder:f4', comment: 'Folder', mode: 'folder' }), dragon({ folder: 'folder:f4', selective: true, secondkey: 'fire' })],
    messages: [u('dragon fire')],
})

// Modules.
const mod = (id, lorebook, fields = {}) => ({ name: id, description: '', id, lorebook, ...fields })
const modLore = text => [L(text, { alwaysActive: true, comment: text })]
add('module-enabled', { root: { modules: [mod('m1', modLore('M1'))], enabledModules: ['m1'] } })
add('module-not-enabled', { root: { modules: [mod('m1', modLore('M1'))], enabledModules: [] } })
add('module-chat', { root: { modules: [mod('m1', modLore('M1')), mod('m2', modLore('M2'))] }, chat: { modules: ['m2'] } })
add('module-character', { root: { modules: [mod('m1', modLore('M1')), mod('m2', modLore('M2'))] }, character: { modules: ['m1'] } })
add('module-integration', {
    root: { modules: [mod('m1', modLore('M1')), mod('m2', modLore('M2')), mod('m3', modLore('M3'))], moduleIntergration: ' m3 , m1' },
})
add('module-namespace', { root: { modules: [mod('m1', modLore('M1'), { namespace: 'ns' })], enabledModules: ['ns'] } })
add('module-duplicate-id', {
    root: { modules: [mod('m1', modLore('First')), mod('m1', modLore('Second'))], enabledModules: ['m1'] },
})
add('module-persona', {
    root: { personas: [{ id: 'p1', name: 'Persona', personaPrompt: '', icon: '', largePortrait: false, embeddedModule: mod('pm', modLore('Persona module')) }] },
    chat: { bindedPersona: 'p1' },
})
add('module-persona-duplicate', {
    root: {
        modules: [mod('pm', modLore('Stored module'))], enabledModules: ['pm'],
        personas: [{ id: 'p1', name: 'Persona', personaPrompt: '', icon: '', largePortrait: false, embeddedModule: mod('pm', modLore('Persona module')) }],
    },
    chat: { bindedPersona: 'p1' },
})
add('module-lorebook-object', { root: { modules: [mod('m1', L('Single object.', { alwaysActive: true, comment: 'Object' }))], enabledModules: ['m1'] } })
add('module-lore-after-character', {
    root: { modules: [mod('m1', [L('Module treasure.', { key: 'dragon', comment: 'Module' })])], enabledModules: ['m1'] },
    lore: [treasure()], local: [L('Local dragon.', { key: 'hello', comment: 'Local' })], messages: [u('hello')],
})
add('module-order-enabled-chat-character', {
    root: { modules: [mod('a', modLore('A')), mod('b', modLore('B')), mod('c', modLore('C'))], enabledModules: ['c'] },
    chat: { modules: ['a'] }, character: { modules: ['b'] },
})
add('module-without-lorebook', { root: { modules: [mod('m1', undefined), mod('m2', modLore('M2'))], enabledModules: ['m1', 'm2'] } })
add('module-integration-empty-piece', { root: { modules: [mod('', modLore('Empty id'))], moduleIntergration: 'x,' } })
add('module-persona-unbound', {
    root: { personas: [{ id: 'p1', name: 'Persona', personaPrompt: '', icon: '', largePortrait: false, embeddedModule: mod('pm', modLore('Persona module')) }] },
    chat: { bindedPersona: 'p2' },
})

// Speaker names in the match log.
add('speaker-message-name', { lore: [dragon()], messages: [c('a dragon', { name: 'Bob' })] })
add('speaker-saying-other', {
    others: [{ chaId: 'char-b', name: 'Brin', chatPage: 0, chats: [], type: 'character' }], lore: [dragon()], messages: [c('a dragon', { saying: 'char-b' })],
})
add('speaker-saying-unknown', { lore: [dragon()], messages: [c('a dragon', { saying: 'char-z' })] })
add('speaker-saying-without-name', {
    others: [{ chaId: 'char-c', chatPage: 0, chats: [], type: 'character' }], lore: [dragon()], messages: [c('a dragon', { saying: 'char-c' })],
})
add('speaker-user-name', { root: { username: 'Quinn' }, lore: [dragon()], messages: [u('a DRAGON')] })

// Errors.
add('error-position-without-argument', { lore: [deco('@@position\nNo argument.')] })
add('error-message-data-number', { lore: [dragon()], messages: [u(5)] })
add('error-tape-after-activation', { lore: [always('First.', { comment: 'First' }), chance(50)], messages: [u('dragon')], random: [] })

// CBS the literal processor does not evaluate.
add('cbs-user-macro', { lore: [always('{{user}} is here.', { comment: 'User' })], native: 'unsupported-cbs' })
add('cbs-char-tag', { lore: [always('<char> waits.', { comment: 'Char' })], native: 'unsupported-cbs' })
add('cbs-if-block', { lore: [always('{{#if 1}}shown{{/if}}', { comment: 'If' })], native: 'unsupported-cbs' })
add('cbs-pure-block', { lore: [dragon({ content: '{#pure}text{/pure}' })], messages: [u('dragon')], native: 'unsupported-cbs' })

// String settings and insertion orders, which selection converts with ToNumber.
add('string-insertorder-numeric', {
    lore: [always('A.', { insertorder: '10', comment: 'A' }), always('B.', { insertorder: ' 3 ', comment: 'B' }),
        always('C.', { insertorder: '0b11', comment: 'C' }), always('D.', { insertorder: '1e1', comment: 'D' })],
})
add('string-insertorder-not-a-number', {
    lore: [always('A.', { insertorder: 'abc', comment: 'A' }), always('B.', { insertorder: '', comment: 'B' }),
        always('C.', { insertorder: '-0', comment: 'C' }), always('D.', { insertorder: '\n-2.5\t', comment: 'D' })],
})
add('string-insertorder-special', {
    lore: [always('A.', { insertorder: '0x10', comment: 'A' }), always('B.', { insertorder: 'Infinity', comment: 'B' }),
        always('C.', { insertorder: '-Infinity', comment: 'C' }), always('D.', { insertorder: '1_000', comment: 'D' }),
        always('E.', { insertorder: '.5', comment: 'E' })],
})
add('string-insertorder-with-budget', {
    root: { loreBookToken: 2 },
    lore: [always('@@priority 1\nAAAA', { insertorder: '50', comment: 'Low' }), always('BBBB', { insertorder: '7', comment: 'Seven' })],
})
add('insertorder-boolean', { lore: [always('A.', { insertorder: true, comment: 'A' }), always('B.', { insertorder: false, comment: 'B' })] })
add('string-scan-depth', { character: { loreSettings: { scanDepth: '1' } }, lore: [dragon()], messages: old })
add('string-scan-depth-blank', { character: { loreSettings: { scanDepth: '' } }, lore: [dragon()], messages: [u('dragon')] })
add('string-scan-depth-not-a-number', { character: { loreSettings: { scanDepth: 'all' } }, lore: [dragon()], messages: old })
add('string-scan-depth-padded', { character: { loreSettings: { scanDepth: ' 4 \n' } }, lore: [dragon()], messages: old })
add('string-token-budget', {
    character: { loreSettings: { tokenBudget: '3' } }, lore: [always('12345678', { comment: 'Two' }), always('1234', { comment: 'One' })],
})
add('string-token-budget-hex', {
    character: { loreSettings: { tokenBudget: '0x2' } }, lore: [always('12345678', { comment: 'Two' }), always('1234', { comment: 'One' })],
})
add('string-token-budget-not-a-number', { character: { loreSettings: { tokenBudget: 'lots' } }, lore: [always('', { comment: 'Empty' })] })
add('string-token-budget-blank', { character: { loreSettings: { tokenBudget: '' } }, lore: [always('', { comment: 'Empty' }), always('x', { comment: 'X' })] })
add('string-root-depth-and-budget', {
    root: { loreBookDepth: '1', loreBookToken: '4' }, lore: [dragon(), always('1234', { comment: 'Fits' })], messages: [u('dragon'), u('later')],
})

// Text that a tailored default locale (tr, az, lt) lowercases differently. Root mapping applies here; a native
// selector told the locale is tailored declines these.
add('locale-key-capital-i', { lore: [dragon({ key: 'IVY' })], messages: [u('ivy grows')] })
add('locale-message-capital-i', { lore: [dragon()], messages: [u('I see a DRAGON')] })
add('locale-message-dotted-i', { lore: [dragon()], messages: [u('İstanbul dragon')] })
add('locale-lithuanian-grave', { lore: [dragon({ key: 'ÌR' })], messages: [u('ìr')] })
add('locale-lithuanian-j-accent', { lore: [dragon({ key: 'J\u0301A' })], messages: [u('j\u0301a')] })
add('locale-unscanned-message', { root: { loreBookDepth: 1 }, lore: [dragon()], messages: [u('I was here'), u('dragon')] })
add('locale-username', { root: { username: 'Iris' }, lore: [dragon()], messages: [u('dragon')] })
add('locale-recursive-content', {
    lore: [always('In the cave lies treasure.', { comment: 'Cave' }), treasure()], messages: [u('hello')],
})
add('locale-regex-only', { lore: [rx('/Ivy/')], messages: [u('Ivy')] })

// useTokenizerCaching: TS reuses a plugin tokenizer result per text. Each case names its own provider, which is part
// of the cache key, so every case starts with an empty cache.
const memo = id => ({ useTokenizerCaching: true, currentPluginProvider: `lore-oracle-memo-${id}` })
add('memo-duplicate-contents', {
    root: memo('duplicate-contents'),
    lore: [always('Same text.', { comment: 'A' }), always('Same text.', { comment: 'B' }), always('Other.', { comment: 'C' }),
        always('Same text.', { comment: 'D' })],
})
add('memo-child-and-parent', {
    root: memo('child-and-parent'),
    lore: [L('@@keep_activate_after_match\nParent text.', { key: 'zzz', comment: 'Parent' }),
        L('child placeholder', { key: 'zzz', mode: 'child', comment: 'Child' })],
    messages: [u('x')],
})
add('memo-recursion-duplicate', {
    root: memo('recursion-duplicate'),
    lore: [treasure({ content: 'Gold.' }), guard(), L('Gold.', { key: 'treasure', comment: 'Again' })], messages: [u('a dragon')],
})
add('memo-budget-duplicates', {
    root: { ...memo('budget-duplicates'), loreBookToken: 4 },
    lore: [always('12345678', { comment: 'A' }), always('12345678', { comment: 'B' }), always('1234', { comment: 'C' })],
})
{
    const lore = []
    for (let index = 0; index < 30; index++) {
        lore.push(always(`shared ${index % 5}`, { insertorder: Math.floor(next() * 6), comment: `M${index}` }))
    }
    add('memo-sort-duplicates', { root: memo('sort-duplicates'), lore })
}

// A throw after a write: TS keeps the dont-activate variable it wrote before `@@position` without an argument throws.
add('error-after-dont-activate-write', {
    lore: [L('@@dont_activate_after_match\nFirst.', { key: 'x', comment: 'First' }), always('@@position\nSecond.', { comment: 'Second' })],
    messages: [u('x')],
})

// Stored values selection cannot use are declined only where TS reads them.
add('field-key-number-always-active', { lore: [always('Number key.', { key: 5, comment: 'Number' })] })
add('field-key-number-searched', { lore: [dragon({ key: 5 })], messages: [u('dragon')], native: 'unsupported-lore-key' })
add('field-secondkey-number-not-selective', { lore: [dragon({ secondkey: 7 })], messages: [u('dragon')] })
add('field-secondkey-number-selective', {
    lore: [dragon({ secondkey: 7, selective: true })], messages: [u('dragon')], native: 'unsupported-lore-secondkey',
})
add('field-content-missing-skipped', { lore: [{ key: '', comment: 'No content', insertorder: 100 }, dragon()], messages: [u('dragon')] })
add('field-content-missing-read', { lore: [{ key: 'dragon', comment: 'No content' }], messages: [u('dragon')], native: 'unsupported-lore-content' })
add('field-content-number-read', { lore: [always(5, { comment: 'Number' })], native: 'unsupported-lore-content' })
add('field-folder-number', { lore: [dragon({ folder: 5 })], messages: [u('dragon')] })
add('field-id-array-unread', { lore: [dragon({ id: [1, 2] })], messages: [u('dragon')] })
add('field-id-array-variable', {
    lore: [dragon({ id: [1, 2], content: '@@keep_activate_after_match\nKept.' })], messages: [u('dragon')], native: 'unsupported-lore-id',
})
add('field-id-object-child', {
    lore: [L('Parent.', { key: 'zzz', id: {}, comment: 'Parent' }), L('Child.', { key: 'zzz', id: {}, mode: 'child', comment: 'Child' })],
    messages: [u('x')],
})
add('field-insertorder-array-skipped', { lore: [L('Skipped.', { insertorder: [5], comment: 'Skipped' }), dragon()], messages: [u('dragon')] })
add('field-insertorder-array-read', { lore: [always('Read.', { insertorder: [5], comment: 'Read' })], native: 'unsupported-lore-insertorder' })
add('field-comment-number-skipped', { lore: [L('Skipped.', { comment: 5 }), dragon()], messages: [u('dragon')] })

// V8's TimSort past one run (more than 64 entries) and with galloping merges (long blocks of equal keys), with NaN
// priorities.
sortCase('sort-260-gallop', 260, 3, 0.2, 0.3)
sortCase('sort-600-gallop', 600, 2, 0.1, 0.4)
sortCase('sort-1000-ties', 1000, 4, 0.25, 0.25)

const orderedTarget = 'I phase-current.'
for (const [id, mode] of [
    ['ordered-current-keep-dont-recursive', 'success'],
    ['ordered-current-count-reject', 'count-reject'],
    ['ordered-current-count-defer', 'count-defer'],
]) {
    add(id, {
        lore: [
            always('@@keep_activate_after_match\nlead-one.', { id: 'prior-a', comment: 'Prior A' }),
            always('@@keep_activate_after_match\nlead-two.', { id: 'prior-b', comment: 'Prior B' }),
            always(`@@keep_activate_after_match\n@@dont_activate_after_match\n${orderedTarget}`, { id: 'current', comment: 'Current' }),
        ],
        orderedObservation: { mode, targetId: 'current', targetText: orderedTarget,
            normalizeTexts: ['lead-one.', 'lead-two.', orderedTarget] },
    })
}

const file = { schema: 1, base, cases }
const casesPath = fileURLToPath(new URL('../../tests/fixtures/rust-core/lorebook/selection.cases.json', import.meta.url))
writeFileSync(casesPath, `${JSON.stringify(file, null, 1)}\n`)
console.log(`${cases.length} cases`)
