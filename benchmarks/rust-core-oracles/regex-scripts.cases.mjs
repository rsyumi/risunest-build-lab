// Writes tests/fixtures/rust-core/regex/scripts.cases.json, the synthetic processScriptFull cases of the regex oracle.
// Add cases here, run `node benchmarks/rust-core-oracles/regex-scripts.cases.mjs`, then regenerate the expected file
// with RUST_CORE_ORACLE_WRITE=1.
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const cases = []
const script = (pattern, out, type = 'editoutput', flag, ableFlag) => ({
    in: pattern, out, type, ...(flag === undefined ? {} : { flag }), ...(ableFlag === undefined ? {} : { ableFlag }),
})
const flagged = (pattern, out, flag, type = 'editoutput') => script(pattern, out, type, flag, true)
const run = (input, mode = 'editoutput', extra = {}) => ({ mode, input, ...extra })
const add = (id, scripts, runs, extra = {}) => cases.push({ id, scripts, runs: Array.isArray(runs) ? runs : [runs], ...extra })
const one = (id, scripts, input, extra = {}, runExtra = {}) => add(id, [].concat(scripts), run(input, runExtra.mode, runExtra), extra)

// Flags and ableFlag.
const flagInput = 'Aaba aA 가a'
for (const [name, flag] of [['empty', ''], ['g', 'g'], ['i', 'i'], ['gi', 'gi'], ['ig', 'ig'], ['gg', 'gg'], ['gig', 'gig'],
    ['spaced', ' g '], ['unknown', 'x'], ['letters', 'abc'], ['upper', 'G'], ['gy', 'gy'], ['y', 'y'], ['yi', 'yi'], ['gu', 'gu'],
    ['u', 'u'], ['gm', 'gm'], ['gs', 'gs'], ['gd', 'gd'], ['all-but-v', 'dgimsuy'], ['gim', 'gim'], ['iu', 'iu'], ['giu', 'giu'],
    ['uv', 'uv'], ['tab', '\tgi\n']]) {
    one(`flags-able-${name}`, flagged('a', 'X', flag), flagInput)
}
for (const flag of ['i', 'y', '', 'gim']) one(`flags-unable-${flag || 'empty'}`, script('a', 'X', 'editoutput', flag, false), flagInput)
one('flags-able-absent', script('a', 'X', 'editoutput', undefined, true), flagInput)
one('flags-unable-absent', script('a', 'X'), flagInput)
one('flags-v-unsupported', flagged('a', 'X', 'gv'), flagInput, {}, { unsupported: 'unicode_sets' })
one('flags-legacy-case-cased-input', flagged('a', 'X', 'gi'), 'aÄa')
one('flags-unicode-case-cased-input', flagged('ä', 'X', 'giu'), 'aÄäa')
one('flags-legacy-case-class', flagged('[a-c]+', '_', 'gi'), 'ABC abc 한글 xyz')
one('flags-legacy-case-backref', flagged('(a)\\1', '_', 'gi'), 'aA Aa aa')
one('flags-legacy-case-kelvin-pattern', flagged('[K]', '_', 'gi'), 'k K')
one('flags-multiline', flagged('^a', 'X', 'gm'), 'ab\nab\r\nab')
one('flags-no-multiline', flagged('^a', 'X', 'g'), 'ab\nab')
one('flags-dotall', flagged('a.b', 'X', 'gs'), 'a\nb a-b')
one('flags-no-dotall', flagged('a.b', 'X', 'g'), 'a\nb a-b a b')
one('flags-metadata-then-flags', flagged('a', 'X', '<order 1>i'), flagInput)
one('flags-only-metadata', flagged('a', 'X', '<order 1>'), flagInput)
one('flags-metadata-unable', script('a', 'X', 'editoutput', '<order 1>i', false), flagInput)

// Ordering by `<order N>`: each script appends its index at the end, so the output spells the execution order.
const marker = (index, order, extra = '') => flagged('$', String.fromCharCode(97 + index), order === undefined ? `g${extra}` : `g<order ${order}>${extra}`)
const orderSets = [
    [0, 1], [1, 0], [2, 1, 3], [-1, 0], [1, 1], [1, 1, 1, 2], [undefined, 1], [3, undefined, 3], ['x', 1], ['x', 'x'],
    ['x', 0, 1], [1, 'x', 2, 'x', 0, 3], ['0x10', 15], ['1e3', 2], [' 5', 4], ['5abc', 4], ['-0', 0], ['-0', '-0'],
    ['9'.repeat(400), 1], ['-' + '9'.repeat(400), 0], ['1.9', 1], ['+2', 1], ['0X1f', 30], ['0x', 1], ['', 1], ['abc', 'def', 0],
    [2, 'x', 1], ['x', 2, 'x', 1], [0, 0, 'x', 0], [5, 4, 3, 2, 1, 0], [0, 1, 2, 3, 4, 5], [1, 0, 1, 0, 1, 0], ['x', 'x', 1, 'x'],
    [1, 2, 'x', 3, 4, 'x', 5, 6, 'x', 7], ['x', 9, 8, 'x', 7, 6, 'x', 5], [3, 1, 'x', 2], ['00012', 11], [' \n7', 6],
]
orderSets.forEach((orders, set) => {
    add(`order-${set}`, orders.map((order, index) => marker(index, order)), run('>'))
})
for (const [length, withNaN] of [[63, true], [64, false], [70, false], [64, true]]) {
    const scripts = Array.from({ length }, (_, index) => flagged('$', String.fromCharCode(0x4e00 + index), `g<order ${withNaN && index % 7 === 3 ? 'x' : (index * 37) % 11}>`))
    const id = `order-many-${length}${withNaN ? '-nan' : ''}`
    add(id, scripts, run('>', 'editoutput', length >= 64 && withNaN ? { unsupported: 'script-order-nan' } : {}))
}
add('order-mixed-modes', [marker(0, 2), script('$', 'Z', 'editinput'), marker(1, 1), marker(2)], [run('>'), run('>', 'editinput')])
add('order-preset-first', [flagged('$', 'c', 'g'), flagged('$', 'd', 'g<order 1>')], run('>'), { preset: [flagged('$', 'a', 'g'), flagged('$', 'b', 'g')] })
add('order-preset-sorted', [flagged('$', 'c', 'g')], run('>'), { preset: [flagged('$', 'a', 'g'), flagged('$', 'b', 'g<order 1>')] })

// Metadata items and the replacement rewrite.
for (const [name, flag, out, input] of [
    ['no-end-nl-off', 'g', '<b>', 'a'], ['no-end-nl-on', 'g<no_end_nl>', '<b>', 'a'], ['no-end-nl-list', 'g<order 1, no_end_nl>', '<b>', 'a'],
    ['no-end-nl-unable', 'g<no_end_nl>', '<b>', 'a'], ['end-gt-only', 'g', '>', 'aXa'], ['newline-token', 'g', 'x$ny', 'a'],
    ['newline-token-repeat', 'g', '$n$n$$n', 'a'], ['data-token', 'g', '[{{data}}]', 'abca'], ['data-token-twice', 'g', '{{data}}{{data}}', 'ba'],
    ['data-dollar', 'g', '$${{data}}', 'a'], ['unknown-action-match', 'g<foo>', 'Y', 'aa'], ['unknown-action-no-match', 'g<foo>', 'Y', 'bb'],
    ['cbs-action-plain-pattern', 'g<cbs>', 'Y', 'aba'], ['order-word-action', 'g<order>', 'Y', 'aa'], ['nested-brackets', 'g<<order 1>>', 'Y', 'aa'],
    ['empty-brackets', 'g<>', 'Y', 'aa'], ['newline-brackets', 'g<a\nb>', 'Y', 'aa'], ['two-blocks', '<order 1>g<no_end_nl>', 'Y>', 'aa'],
    ['spaced-items', 'g< order 2 ,  no_end_nl >', 'Y>', 'aa'], ['action-move-in-list', 'g<order 1,move_bottom>', 'M', 'xay'],
]) {
    const able = name !== 'no-end-nl-unable'
    // `{{data}}` survives the rewrite, so its output reaches CBS.
    one(`meta-${name}`, script('a', out, 'editoutput', flag, able), input, {}, out.includes('{{data}}') ? { unsupported: 'cbs' } : {})
}

// Replacement tokens.
const tokenPattern = '(a)(x)?(?<n>b)(?<m>z)?'
for (const [name, template] of [
    ['match', '[$&]'], ['prefix', '[$`]'], ['suffix', "[$']"], ['dollar', '[$$]'], ['one', '[$1]'], ['missing-group', '[$2]'],
    ['named', '[$<n>]'], ['named-missing', '[$<m>]'], ['named-unknown', '[$<q>]'], ['named-empty', '[$<>]'], ['named-open', '[$<n]'],
    ['zero', '[$0]'], ['double-zero', '[$00]'], ['zero-one', '[$01]'], ['zero-three', '[$03]'], ['five', '[$5]'], ['ten', '[$10]'],
    ['end-dollar', '[$'], ['dollar-letter', '[$x]'], ['dollar-dollar-one', '[$$1]'], ['triple', '$$$1$&$$'],
    ['surrogate-template', '\ud800$&\udc00'], ['korean', '한$1글'],
]) {
    one(`token-${name}`, script(tokenPattern, template), 'xab-axb-ab')
}
const twelve = '(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)'
for (const [name, template] of [['ten', '$10'], ['eleven', '$11'], ['twelve', '$12'], ['thirteen', '$13'], ['ninety', '$99'], ['one-zero-zero', '$100']]) {
    one(`token-many-${name}`, script(twelve, `[${template}]`), 'abcdefghijklm')
}
one('token-named-no-groups', script('a', '[$<n>]'), 'aa')
one('token-duplicate-names', script('(?<d>a)|(?<d>b)', '[$<d>]'), 'abc')
one('token-backref-named', script('(?<q>[a-z])\\k<q>', '<$<q>>'), 'aabbc')

// Global, sticky, empty matches and UTF-16.
const pair = '😀'
for (const [name, pattern, flag, input, out] of [
    ['empty-global', '', 'g', 'abc', '-'], ['empty-sticky-global', '', 'gy', 'abc', '-'], ['empty-sticky', '', 'y', 'abc', '-'],
    ['empty-unicode-pair', '(?:)', 'gu', `a${pair}b`, '-'], ['empty-legacy-pair', '(?:)', 'g', `a${pair}b`, '-'],
    ['star', 'a*', 'g', 'baaacaa', '-'], ['star-unicode', 'x*', 'gu', `${pair}x${pair}`, '-'], ['lookahead', '(?=a)', 'g', 'banana', '^'],
    ['lookbehind', '(?<=a)n', 'g', 'banana', 'N'], ['negative-lookbehind', '(?<!a)n', 'g', 'nanan', 'N'], ['word-boundary', '\\b', 'g', 'ab cd', '|'],
    ['caret-multiline', '^', 'gm', 'a\nb\n', '>'], ['dollar-multiline', '$', 'gm', 'a\nb', '<'], ['sticky-run', 'a', 'gy', 'aaba', 'x'],
    ['sticky-first', 'b', 'y', 'ab', 'x'], ['sticky-start', 'a', 'y', 'ab', 'x'], ['dot-unicode', '.', 'gu', `${pair}a`, '[$&]'],
    ['dot-legacy', '.', 'g', `${pair}a`, '[$&]'], ['lone-high-unicode', '\\ud83d', 'gu', `${pair}\ud83d`, '#'],
    ['lone-high-legacy', '\\ud83d', 'g', `${pair}\ud83d`, '#'], ['lone-low-legacy', '\\ude00', 'g', `${pair}\ude00x`, '#'],
    ['input-lone-surrogate', 'x', 'g', '\udc00x\ud800', 'y'], ['class-astral', `[${pair}a]`, 'gu', `${pair}a`, '_'],
    ['class-astral-legacy', `[${pair}a]`, 'g', `${pair}a`, '_'], ['property-letter', '\\p{L}+', 'gu', 'ab 한글 12', '<$&>'],
    ['property-script', '\\p{Script=Hangul}', 'gu', 'a한b글', '#'], ['unicode-escape-braces', '\\u{1F600}', 'gu', `a${pair}`, 'E'],
    ['backref', '(a)\\1', 'g', 'aaa aa', '_'], ['lazy', 'a+?', 'g', 'aaa', '_'], ['alternation-order', 'a|ab', 'g', 'abab', '[$&]'],
    ['capture-reset', '(?:(a)|b)+', 'g', 'ab', '[$1]'], ['nested-capture-reset', '((a)|(b))+', 'g', 'ab', '[$1|$2|$3]'],
    ['empty-check', '(a*)+', 'g', 'b', '[$1]'], ['empty-check-capture', '(a*)*b', 'g', 'aab', '[$1]'], ['classic', '(?:a|ab)(c|bcd)(d*)', 'g', 'abcd', '[$1|$2]'],
    ['quantified-group', '(ab){2,3}', 'g', 'abababab', '[$1]'], ['counted', 'a{2}', 'g', 'aaaaa', '_'], ['bounded-range', 'a{1,2}', 'g', 'aaaaa', '_'],
    ['negated-class', '[^ab]', 'g', 'abcab d', '_'], ['digit-class', '\\d+', 'g', 'a12b345', '#'], ['space-class', '\\s+', 'g', 'a \t　﻿b c', '_'],
    ['word-class', '\\w+', 'g', 'ab_1 한 cd', '<$&>'], ['escape-dot', '\\.', 'g', 'a.b.', '!'], ['escape-slash', '\\/', 'g', 'a/b', '|'],
    ['annex-brace', 'a{', 'g', 'a{b', '_'], ['annex-bracket', ']', 'g', 'a]b', '_'], ['annex-octal', '\\101', 'g', 'ABA', '_'],
    ['annex-control', '\\cJ', 'g', 'a\nb', '_'], ['annex-identity', '\\q', 'g', 'aqb', '_'], ['hex-escape', '\\x41', 'g', 'ABA', '_'],
    ['modifier-group', '(?i:a)b', 'g', 'AbaBab', '_'], ['first-only', 'a', '', 'aaa', '_'], ['global-unicode-pairs', `${pair}`, 'gu', `${pair}${pair}`, 'P'],
]) {
    one(`engine-${name}`, flagged(pattern, out, flag), input)
}
one('engine-large-input', script('(ab)+c', '[$1]'), { repeat: 'ababc ', times: 20000 })
one('engine-large-safe', script('ab|c', '-'), { repeat: 'xabc', times: 50000 })
one('engine-large-unicode', flagged('\\p{L}', '', 'gu'), { repeat: 'a한😀 ', times: 20000 })
one('engine-safe-then-general', [script('ab', 'cd'), script('(?<=c)d', 'D'), script('cD', 'E')], 'xabyab')
one('engine-safe-surrogate-input', script('ab', 'cd'), `ab\ud800ab`)
one('engine-safe-surrogate-template', script('ab', '\ud800'), 'abab')
for (const [name, pattern, flag, input, out] of [
    ['sigma-unicode-case', 'σ', 'giu', 'Σσς', '_'], ['sharp-s-unicode-case', 'ß', 'giu', 'ẞßss', '_'], ['kelvin-unicode-case', 'k', 'giu', 'K k K', '_'],
    ['class-unicode-case', '[a-z]+', 'giu', 'ſKK', '<$&>'], ['word-unicode-case', '\\w', 'giu', 'ſK-', '_'], ['boundary-unicode-case', '\\b', 'giu', 'ſa K', '|'],
    ['lookbehind-variable', '(?<=a+)b', 'g', 'aaab b', 'B'], ['lookbehind-capture', '(?<=(a)(b)?)c', 'g', 'acabc', '[$1$2]'],
    ['backref-in-lookbehind', '(?<=\\1(a))b', 'g', 'aab ab', 'B'], ['backref-before-group', '\\1(a)', 'g', 'aa', '[$&]'],
    ['nested-backref', '(a\\1)', 'g', 'aa', '[$1]'], ['empty-class', '[]', 'g', 'abc', '_'], ['any-class-legacy', '[^]', 'g', 'a\nb', '_'],
    ['backspace-class', '[\\b]', 'g', 'a\bb', '_'], ['null-escape', '\\0', 'g', 'a\u0000b', '_'], ['zero-quantifier', 'a{0}b', 'g', 'ab', '_'],
    ['quantified-lookahead-legacy', '(?=a)*b', 'g', 'ab', '_'], ['emoji-property', '\\p{Extended_Pictographic}', 'gu', `a${pair}b`, 'E'],
    ['not-letter', '\\P{L}+', 'gu', 'ab12한!', '_'], ['max-code-point', '\\u{10FFFF}', 'gu', 'a􏿿', 'M'],
    // Property escapes, their complements and negated classes of them, with and without ignore case.
    ['property-letter-case', '\\p{L}+', 'giu', 'aB 한 12 ſK', '<$&>'], ['not-letter-case', '\\P{L}+', 'giu', 'ab12한!ſ', '_'],
    ['negated-letter-class', '[^\\p{L}]', 'gu', 'ab12한!', '_'], ['negated-letter-class-case', '[^\\p{L}]', 'giu', 'ab12한!ſK', '_'],
    ['upper-property', '\\p{Lu}', 'gu', 'aBcDſKΣσ1', '_'], ['upper-property-case', '\\p{Lu}', 'giu', 'aBcDſKΣσ1', '_'],
    ['not-upper-property', '\\P{Lu}', 'gu', 'aBcDſKΣσ1', '_'], ['not-upper-property-case', '\\P{Lu}', 'giu', 'aBcDſKΣσ1', '_'],
    ['negated-upper-class', '[^\\p{Lu}]', 'gu', 'aBcDſKΣσ1', '_'], ['negated-upper-class-case', '[^\\p{Lu}]', 'giu', 'aBcDſKΣσ1', '_'],
    ['negated-not-upper-class-case', '[^\\P{Lu}]', 'giu', 'aBcDſKΣσ1', '_'], ['lower-property-case', '\\p{Ll}+', 'giu', 'AbC ΣσςK', '<$&>'],
    ['not-lower-class-case', '[\\P{Ll}]', 'giu', 'AbC1ſ', '_'], ['script-property-case', '\\p{Script=Greek}', 'giu', 'aΣσςΩ', '_'],
    ['surrogate-range-legacy', '[\\ud800-\\udfff]', 'g', `a${pair}`, '_'], ['lone-trail-unicode', '\\udc00', 'gu', `🐀\udc00`, '_'],
    ['dot-lone-unicode', '.', 'gu', '𐀀\ud800', '[$&]'], ['dollar-end', '$', 'g', 'a\n', '!'], ['optional-group', '(a)|b', 'g', 'ab', '[$1]'],
    ['legacy-named-escape', '\\k<n>', 'g', 'k<n>', '_'], ['unicode-named-backref', '(?<a>.)\\k<a>', 'gu', 'aabb', '[$<a>]'],
    ['legacy-dash-escape', '\\-', 'g', 'a-b', '_'], ['unicode-dash-escape-class', '[\\-]', 'gu', 'a-b', '_'],
    ['alternation-many', Array.from({ length: 50 }, (_, index) => `w${index}`).join('|'), 'g', 'w1 w49 w50 w7', '<$&>'],
    ['lazy-capture', '<(.+?)>', 'g', '<a><bb>', '[$1]'], ['greedy-capture', '<(.+)>', 'g', '<a><bb>', '[$1]'],
    ['nested-quantifier', '(?:a+)+b', 'g', 'aaab', '_'], ['case-legacy-latin-pattern', 'é', 'gi', 'eE 한', '_'],
    ['case-legacy-latin-range', '[à-ÿ]', 'gi', 'aA 한', '_'], ['case-legacy-korean', '[가-힣]+', 'gi', 'aA 한글', '_'],
    ['case-legacy-negated-word','[^\\w]', 'gi', 'sSkKiI-', '_'], ['case-legacy-negated-letters', '[^a-z]', 'gi', 'sSkK-', '_'],
    ['case-legacy-not-word-boundary', '\\B', 'gi', 'sk ab', '|'], ['case-legacy-not-space', '\\S', 'gi', 'sk -', '_'],
    ['case-legacy-dot', '.', 'gi', 'sk -', '_'],
]) {
    one(`engine2-${name}`, flagged(pattern, out, flag), input)
}
one('engine2-case-legacy-dotless', flagged('ı', 'X', 'gi'), 'iI')
one('engine2-case-legacy-wide-range', flagged('[À-ž]', 'X', 'gi'), 'iI')
one('engine2-case-legacy-dotted-input', flagged('i', 'X', 'gi'), 'iİ')
// Word complements close the positive word set before complementing in both modes.
for (const [name, pattern, flag, input] of [['legacy-not-word', '\\W', 'gi', 'sSkKiI-'], ['legacy-not-word-class', '[\\W]', 'gi', 'sSkKiI-'],
    ['legacy-negated-not-word', '[^\\W]', 'gi', 'sSkKiI-'], ['legacy-not-word-range', '[\\W-z]', 'gi', 'sSkKiI-'],
    ['unicode-not-word-class', '[\\W]', 'giu', 'sSkKiI-'], ['unicode-not-word', '\\W', 'giu', 'sSkKſK-']]) {
    one(`engine2-case-${name}`, flagged(pattern, '_', flag), input)
}
for (const [name, pattern, flag, input] of [['unicode-negated-word', '[^\\w]', 'giu', 'sSkKſK-'], ['unicode-word-class', '[\\w]+', 'giu', 'ſK-'],
    ['unicode-not-word-no-case', '[\\W]', 'gu', 'sſK-'], ['legacy-not-word-no-case', '[\\W]', 'g', 'sſK-'],
    ['escaped-backslash-w', '\\\\W', 'gi', 'a\\Wb'], ['unicode-negated-not-digit', '[^\\D]', 'giu', 'a1ſ']]) {
    one(`engine2-case-${name}`, flagged(pattern, '_', flag), input)
}
// Modifier groups turn ignore case on without the flag, and get the same admission.
for (const [name, pattern, flag, input, unsupported] of [['modifier-kelvin', '(?i:k)', 'g', 'k K K', undefined],
    ['modifier-not-word', '(?i:[\\W])', 'g', 'sSkK-', undefined], ['modifier-cased-input', '(?i:a)', 'g', 'aÄ', undefined],
    ['modifier-ascii', '(?i:a)b', 'g', 'ab Ab aB 한', undefined], ['modifier-unicode-kelvin', '(?i:k)', 'gu', 'k K K', undefined],
    ['modifier-off', '(?-i:a)b', 'gi', 'aB AB', undefined], ['modifier-off-cased-input', '(?-i:a)b', 'gi', 'aB ſ', undefined]]) {
    one(`engine2-case-${name}`, flagged(pattern, '_', flag), input, {}, unsupported ? { unsupported } : {})
}

// Canonicalize: literals, classes, complements, captures, scoped flags and UTF-16.
const caseInput = 'sSſ kKK iIİı ßẞ éÉ µμΜ σςΣ ᾀᾈ ᾳᾼ ﬀff 𐐀𐐨 \ud800'
for (const flags of ['gi', 'giu']) {
    for (const [name, pattern] of [
        ['ascii', '[a-z]+'], ['kelvin', 'K'], ['long-s', 'ſ'], ['dotless', 'ı'], ['sharp-s', 'ß'],
        ['latin', '[à-ÿ]'], ['greek', 'ᾀ'], ['greek-class', '[ᾀ]'], ['greek-range', '[ᾀ-ᾇ]'],
        ['greek-not', '[^ᾀ]'], ['greek-subscript', 'ᾳ'], ['ligature', 'ﬀ'], ['micro', 'µ'], ['sigma', 'σ'],
        ['word', '\\w'], ['not-word', '\\W'], ['word-class', '[\\w]'], ['not-word-class', '[\\W]'],
        ['negated-not-word', '[^\\W]'], ['mixed-not-word', '[\\Wk]'], ['negated-mixed', '[^\\Ws]'],
        ['empty-class', '[^\\w\\W]'], ['all-class', '[\\w\\W]'], ['scope-on', '(?i:[\\W])'],
        ['scope-off', '(?-i:[\\W])'], ['astral', '𐐀'], ['surrogate', '\\ud800'],
    ]) one(`canonical-${flags}-${name}`, flagged(pattern, '[$&]', flags), caseInput)
    one(`canonical-${flags}-backrefs`, flagged('(s|k|ı|ᾀ|ᾳ|µ|σ|é|𐐀)\\1', '[$1:$&]', flags), 'sſ sS kK kK ıI ıı ᾀᾈ ᾀᾀ ᾳᾼ µΜ σς éÉ 𐐀𐐨')
    one(`canonical-${flags}-named-backref`, flagged('(?<c>ᾀ|ſ|K|ß)\\k<c>', '[$<c>]', flags), 'ᾀᾈ ᾀᾀ ſS KK ßẞ')
}
one('canonical-host-produced-input', [flagged('[\\W]', '_', 'giu'), flagged('[a-z]+', '[$&]', 'gi')], 'initial',
    { lua: true, hooks: { editoutput: 1 } }, { lua: { value: 'intermediate' }, hooks: [{ value: 'ſK éÉ kK' }] })
one('canonical-repeat-back', [flagged('ᾀ', '@@repeat_back end', 'gi')], 'ᾈ',
    { chat: { messages: [{ role: 'char', data: 'ᾀ' }] } }, { chatIndex: 1 })

// Invalid patterns leave the data unchanged and later scripts still run.
for (const [name, pattern, flag] of [
    ['open-paren', '(', 'g'], ['close-paren', ')', 'g'], ['open-bracket', '[', 'g'], ['leading-star', '*a', 'g'], ['range-order', 'a{2,1}', 'g'],
    ['trailing-backslash', 'a\\', 'g'], ['duplicate-name', '(?<a>x)(?<a>y)', 'g'], ['unicode-named-backref', '\\k<a>', 'gu'],
    ['unknown-property', '\\p{Foo}', 'gu'], ['class-order', '[z-a]', 'g'], ['double-star', 'a**', 'g'], ['bad-name', '(?<1a>x)', 'g'],
    ['unicode-brace', 'a{', 'gu'], ['unicode-bracket', ']', 'gu'], ['unicode-identity', '\\q', 'gu'], ['lone-quantifier', '{1}', 'g'],
    ['lookbehind-quantified', '(?<=a)+', 'gu'], ['bad-modifier', '(?ii:a)', 'g'], ['unicode-octal', '\\1', 'gu'], ['bad-flags', 'a', 'gq'],
]) {
    const scripts = [flagged(pattern, 'X', flag), script('b', 'B')]
    one(`invalid-${name}`, scripts, 'abc')
}
one('invalid-empty-pattern-skipped', [script('', 'X'), script('b', 'B')], 'abc')
one('invalid-dynamic-empty', [flagged('', 'X', 'g<cbs>'), script('b', 'B')], 'abc')

// Directives.
const emotions = [['happy', 'happy.png'], ['sad', 'sad.png'], ['happy', 'second.png']]
for (const [name, out, input, extra] of [
    ['emo-match', '@@emo happy', 'smile', {}], ['emo-trim', '@@emo   sad  ', 'smile', {}], ['emo-unknown', '@@emo angry', 'smile', {}],
    ['emo-no-match', '@@emo happy', 'frown', {}], ['emo-simple', '@@emo happy', 'smile', { emotions: null }],
    ['emo-prior-four', '@@emo sad', 'smile', { priorEmotions: [['a', '1', 1], ['b', '2', 2], ['c', '3', 3], ['d', '4', 4]] }],
    ['emo-prior-five', '@@emo sad', 'smile', { priorEmotions: [['a', '1', 1], ['b', '2', 2], ['c', '3', 3], ['d', '4', 4], ['e', '5', 5]] }],
    ['emo-prior-five-unknown', '@@emo angry', 'smile', { priorEmotions: [['a', '1', 1], ['b', '2', 2], ['c', '3', 3], ['d', '4', 4], ['e', '5', 5]] }],
    ['emo-prior-six-simple', '@@emo sad', 'smile', { emotions: null, priorEmotions: [['a', '1', 1], ['b', '2', 2], ['c', '3', 3], ['d', '4', 4], ['e', '5', 5], ['f', '6', 6]] }],
    ['emo-no-space', '@@emohappy', 'smile', {}], ['emo-korean', '@@emo 기쁨', 'smile', { emotions: [['기쁨', 'joy.png']] }],
]) {
    one(`directive-${name}`, script('smile', out), input, { emotions, ...extra })
}
add('directive-emo-several', [script('a', '@@emo happy'), script('b', '@@emo sad'), script('c', '@@emo happy')], run('abc'), { emotions })
add('directive-emo-then-replace', [script('a', '@@emo sad'), script('a', 'b')], run('aa'), { emotions })
for (const [name, scripts, input, runExtra] of [
    ['move-top', [script('<b>', '@@move_top [$&]')], 'x<b>y<b>z', {}], ['move-bottom', [script('b+', '@@move_bottom ($&)')], 'abbcb', {}],
    ['move-top-global', [flagged('b+', '@@move_top ($&)', 'gg')], 'abbcb', {}], ['move-bottom-global', [flagged('(b)(c)?', '@@move_bottom $1-$2-$0-$3', 'gg')], 'abcab', {}],
    ['move-dollar-tokens', [flagged('a(.)', '@@move_top $$1|$1|$&|$`|$01', 'gg')], 'xa1ya2', {}],
    ['move-named', [flagged('(?<NaN>a)(?<x>b)', '@@move_top $<x>|$<NaN>|$<1>', 'gg')], 'abab', {}],
    ['move-action-top', [flagged('b', 'T', 'g<move_top>')], 'abc', {}], ['move-action-bottom', [flagged('b', 'T', 'g<move_bottom>')], 'abcb', {}],
    ['move-action-both', [flagged('b', 'T', 'g<move_bottom,move_top>')], 'abc', {}], ['move-prefix-no-space', [script('b', '@@move_topX')], 'abc', {}],
    ['move-empty-match', [script('x*', '@@move_top [$&]')], 'abc', {}], ['move-empty-global', [flagged('x*', '@@move_top [$&]', 'gg')], 'ab', {}],
    ['move-no-match', [script('q', '@@move_top [$&]')], 'abc', {}], ['move-sticky', [flagged('a', '@@move_bottom <$&>', 'ggy')], 'aab', {}],
    ['move-unicode', [flagged('.', '@@move_top [$&]', 'ggu')], `${pair}a`, {}], ['move-input-dollar', [script('\\$.', '@@move_top [$&]')], 'a$1b$&c', {}],
    ['move-then-replace', [script('b', '@@move_top B'), script('B', 'C')], 'abc', {}],
    ['inject-global-chat', [script('b', '@@inject')], 'abc', { chatIndex: -1 }], ['inject-action-global', [flagged('b', 'Q', 'g<inject>')], 'abc', { chatIndex: -1 }],
    ['inject-message', [script('b', '@@inject')], 'abc', { chatIndex: 0 }],
    ['repeat-back-match', [script('b', '@@repeat_back end')], 'abc', { chatIndex: 2 }], ['repeat-back-global', [script('q', '@@repeat_back end')], 'abc', { chatIndex: -1 }],
    ['repeat-back-message', [script('q', '@@repeat_back end')], 'abc', { chatIndex: 1 }],
    ['other-directive', [script('b', '@@other $&')], 'abc', {}], ['other-directive-no-match', [script('q', '@@other')], 'abc', {}],
    ['directive-invalid-pattern', [script('(', '@@emo happy'), script('a', 'A')], 'abc', {}],
]) {
    add(`directive-${name}`, scripts, run(input, 'editoutput', runExtra), { emotions })
}
add('directive-inject-capture', [script('b', '@@inject'), script('c', 'C')], run('abc', 'editoutput', { chatIndex: 1 }), { capture: true })
add('directive-inject-action-capture', [flagged('b+', 'Q', 'g<inject>')], run('abbcb', 'editoutput', { chatIndex: 0 }), { capture: true })

// `@@inject` writes the rule input into the message at chatID and goes on without the matches; a message that does
// not exist throws, which keeps the rule input.
const chat = (...data) => ({ messages: data.map((value, index) => ({ role: index % 2 ? 'char' : 'user', data: value })) })
const at = (input, chatIndex, extra = {}) => run(input, 'editoutput', { chatIndex, ...extra })
add('inject-into-message', [script('b', '@@inject'), script('c', 'C')], at('abc', 1), { chat: chat('m0', 'm1') })
add('inject-action-message', [flagged('b+', 'Q', 'g<inject>')], at('abbcb', 0), { chat: chat('m0') })
add('inject-twice', [script('b', '@@inject'), script('c', '@@inject')], at('abc', 0), { chat: chat('m0') })
add('inject-past-the-end', [script('b', '@@inject'), script('c', 'C')], at('abc', 2), { chat: chat('m0', 'm1') })
add('inject-negative-index', [script('b', '@@inject'), script('c', 'C')], at('abc', -2), { chat: chat('m0') })
add('inject-no-match', [script('q', '@@inject')], at('abc', 0), { chat: chat('m0') })
add('inject-runs', [script('b', '@@inject')], [at('abc', 0), at('xbz', 1), at('ab', 0)], { chat: chat('m0', 'm1') })
add('inject-cache-hit', [script('b', '@@inject')], [at('abc', 0), at('xbz', 0), at('abc', 0)], { chat: chat('m0') })
add('inject-capture-message', [script('b', '@@inject')], at('abc', 0), { capture: true, chat: chat('m0') })
add('inject-after-hook', [script('b', '@@inject')], at('abc', 0, { hooks: [{ value: 'hbh' }] }), { chat: chat('m0'), hooks: { editoutput: 1 } })
add('inject-editinput', [script('b', '@@inject', 'editinput')], run('abc', 'editinput', { chatIndex: 0 }), { chat: chat('m0') })
// The Lua listeners' writes come before the rules read the messages.
add('inject-after-lua-add', [script('b', '@@inject')], at('abc', 0, { luaEffects: [{ addChat: ['char', 'x'] }] }), { lua: true })
add('inject-after-lua-cut', [script('b', '@@inject')], at('abc', 1, { luaEffects: [{ cutChat: [0, 1] }] }), { lua: true, chat: chat('m0', 'm1') })
add('inject-after-lua-remove', [script('b', '@@inject')], at('abc', 1, { luaEffects: [{ removeChat: 0 }] }), { lua: true, chat: chat('m0', 'm1') })
add('inject-after-lua-set', [script('b', '@@inject')], at('abc', 0, { lua: { value: 'xbx' }, luaEffects: [{ setChat: [0, 'L'] }, { setChat: [1, 'M'] }] }),
    { lua: true, chat: chat('m0', 'm1') })
add('lua-writes-in-order', [script('q', 'Q')], at('abc', 0, { luaEffects: [{ addChat: ['user', 'u'] }, { setChat: [0, 'S'] }, { cutChat: [1] }] }),
    { lua: true, chat: chat('m0', 'm1') })
add('lua-writes-then-cache', [script('b', 'B')], [at('abc', 0, { luaEffects: [{ addChat: ['char', 'x'] }] }), at('abc', 0, { luaEffects: [{ removeChat: 0 }] })],
    { lua: true, chat: chat('m0') })

// `@@repeat_back` without a match reads the nearest earlier message with the role of the one at chatID, else the
// greeting, and adds what the pattern matches there.
const talk = {
    messages: [{ role: 'user', data: 'u0 q1' }, { role: 'char', data: 'c1 q2 q3' }, { role: 'user', data: 'u2' }, { role: 'char', data: 'c3' }],
    firstMessage: 'hello q0', alternateGreetings: ['alt q8', 'alt q9'],
}
for (const [name, out, flag] of [['end', '@@repeat_back end'], ['start', '@@repeat_back start'], ['end-nl', '@@repeat_back end_nl'],
    ['start-nl', '@@repeat_back start_nl'], ['bare', '@@repeat_back'], ['trailing-space', '@@repeat_back '], ['unknown', '@@repeat_back middle'],
    ['extra-words', '@@repeat_back start more'], ['global', '@@repeat_back end', 'g'], ['sticky', '@@repeat_back end', 'y'],
    ['action', 'X', 'g<repeat_back>'], ['action-position', 'X start_nl', 'g<repeat_back>']]) {
    add(`repeat-back-${name}`, [flag === undefined ? script('q\\d', out) : flagged('q\\d', out, flag)], at('abc', 3), { chat: talk })
}
add('repeat-back-empty-match-end', [script('^(?=c)', '@@repeat_back end')], at('abc', 3), { chat: talk })
add('repeat-back-empty-match-bare', [script('^(?=c)', '@@repeat_back')], at('abc', 3), { chat: talk })
add('repeat-back-source-no-match', [script('z\\d', '@@repeat_back end'), script('c', 'C')], at('abc', 3), { chat: talk })
add('repeat-back-greeting', [script('q\\d', '@@repeat_back end')], at('abc', 0), { chat: talk })
add('repeat-back-first-of-role', [script('q\\d', '@@repeat_back end')], at('abc', 1), { chat: talk })
add('repeat-back-alternate-greeting', [script('q\\d', '@@repeat_back end')], at('abc', 0), { chat: { ...talk, fmIndex: 1 } })
add('repeat-back-missing-greeting', [script('q\\d', '@@repeat_back end'), script('c', 'C')], at('abc', 0), { chat: { ...talk, fmIndex: 5 } })
add('repeat-back-missing-message', [script('q\\d', '@@repeat_back end'), script('c', 'C')], at('abc', 6), { chat: talk })
add('repeat-back-negative-index', [script('q\\d', '@@repeat_back end')], at('abc', -2), { chat: talk })
add('repeat-back-two-rules', [script('q\\d', '@@repeat_back end'), script('u\\d', '@@repeat_back start_nl')], at('abc', 2), { chat: talk })
add('repeat-back-then-rule', [script('q\\d', '@@repeat_back end'), script('q2', 'Z')], at('abc', 3), { chat: talk })
add('repeat-back-cache', [script('q\\d', '@@repeat_back end')], [at('abc', 3), at('abc', 3), at('abd', 3)], { chat: talk })
add('repeat-back-capture', [script('q\\d', '@@repeat_back end')], at('abc', 3), { capture: true, chat: talk })
add('repeat-back-editinput', [script('q\\d', '@@repeat_back start', 'editinput')], run('abc', 'editinput', { chatIndex: 2 }), { chat: talk })
add('repeat-back-after-lua', [script('q\\d', '@@repeat_back end')], at('abc', 3, { luaEffects: [{ setChat: [1, 'c1 q7'] }] }), { lua: true, chat: talk })
add('repeat-back-after-hook', [script('q\\d', '@@repeat_back end')], at('abc', 3, { hooks: [{ value: 'hh' }] }), { chat: talk, hooks: { editoutput: 1 } })
add('repeat-back-after-inject', [script('b', '@@inject'), script('q\\d', '@@repeat_back end')], at('abc', 3), { chat: talk })

// Modes.
const modeScripts = [script('a', 'I', 'editinput'), script('a', 'O', 'editoutput'), script('a', 'P', 'editprocess'),
    script('a', 'D', 'editdisplay'), script('a', 'Q', 'editfoo'), script('a', 'R', '')]
for (const mode of ['editinput', 'editoutput', 'editprocess']) add(`mode-${mode}`, modeScripts, run('aba', mode))
add('mode-editdisplay-capture', modeScripts, run('aba', 'editdisplay'), { capture: true })
add('mode-editdisplay-live', modeScripts, run('aba', 'editdisplay', { unsupported: 'editdisplay-live' }))
add('mode-all-capture', modeScripts, ['editinput', 'editoutput', 'editprocess', 'editdisplay'].map((mode) => run('aba', mode)), { capture: true })
add('mode-role-capture', [script('a', 'b', 'editdisplay')], run('aa', 'editdisplay', { role: 'user', chatIndex: 3 }), { capture: true })
add('mode-no-scripts', [], [run('abc'), run('abc', 'editinput')])
add('mode-other-mode-only', [script('a', 'b', 'editinput')], run('abc'))

// Lua edit listeners and plugin hooks.
for (const [name, mode, lua, extra] of [
    ['lua-input', 'editinput', { value: 'lua-x' }, {}], ['lua-output', 'editoutput', { value: 'a-from-lua' }, {}],
    ['lua-process-skipped', 'editprocess', { value: 'never' }, {}], ['lua-undefined', 'editoutput', { undefined: true }, {}],
    ['lua-throw', 'editinput', { throw: 'lua failed' }, {}], ['lua-capture-skipped', 'editoutput', { value: 'never' }, { capture: true }],
    ['lua-index', 'editoutput', { value: 'aaa' }, {}], ['lua-empty', 'editoutput', { value: '' }, {}],
]) {
    const chatIndex = name === 'lua-index' ? 4 : -1
    add(`host-${name}`, [script('a', 'A', mode)], run('banana', mode, { lua, chatIndex }), { lua: true, ...extra })
}
add('host-no-listeners', [script('a', 'A')], run('banana', 'editoutput', { lua: { value: 'never' } }))
for (const [name, mode, count, replies, extra] of [
    ['hook-one', 'editoutput', 1, [{ value: 'hooked a' }], {}], ['hook-null', 'editoutput', 1, [{ null: true }], {}],
    ['hook-undefined', 'editinput', 1, [{ undefined: true }], {}], ['hook-chain', 'editoutput', 3, [{ value: 'a1' }, { null: true }, { value: 'aa2' }], {}],
    ['hook-throw', 'editoutput', 2, [{ value: 'first' }, { throw: 'hook failed' }], {}], ['hook-process', 'editprocess', 1, [{ value: 'proc a' }], {}],
    ['hook-capture-skipped', 'editoutput', 2, [{ value: 'never' }], { capture: true }], ['hook-empty', 'editoutput', 1, [{ value: '' }], {}],
]) {
    add(`host-${name}`, [script('a', 'A', mode)], run('banana', mode, { hooks: replies }), { hooks: { [mode]: count }, ...extra })
}
add('host-hook-other-mode', [script('a', 'A')], run('banana', 'editoutput', { hooks: [{ value: 'never' }] }), { hooks: { editinput: 2 } })
add('host-lua-then-hooks', [script('a', 'A', 'editinput')], run('banana', 'editinput', { lua: { value: 'lua a' }, hooks: [{ value: 'hook a' }, { undefined: true }] }),
    { lua: true, hooks: { editinput: 2 } })
add('host-lua-throw-then-hook', [script('a', 'A')], run('banana', 'editoutput', { lua: { throw: 'x' }, hooks: [{ value: 'hook a' }] }),
    { lua: true, hooks: { editoutput: 1 } })
add('host-process-hooks-no-lua', [script('a', 'A', 'editprocess')], run('banana', 'editprocess', { lua: { value: 'never' }, hooks: [{ value: 'h a' }] }),
    { lua: true, hooks: { editprocess: 1 } })
add('host-hooks-twice', [script('a', 'A')], [run('banana', 'editoutput', { hooks: [{ value: 'one a' }] }), run('banana', 'editoutput', { hooks: [{ value: 'two a' }] })],
    { hooks: { editoutput: 1 } })

// The result cache.
add('cache-hit-emotion', [script('a', '@@emo happy')], [run('a'), run('a'), run('b'), run('a')], { emotions })
add('cache-bypass', [script('a', '@@emo happy')], [run('a'), run('a')], { emotions, cache: 'bypass' })
add('cache-chat-index', [script('a', '@@emo happy')], [run('a', 'editoutput', { chatIndex: 1 }), run('a', 'editoutput', { chatIndex: 2 }), run('a', 'editoutput', { chatIndex: 1 })], { emotions })
add('cache-mode', [script('a', '@@emo happy'), script('a', '@@emo sad', 'editinput')], [run('a'), run('a', 'editinput'), run('a'), run('a', 'editinput')], { emotions })
add('cache-lua-key', [script('a', '@@emo happy')], [run('x', 'editoutput', { lua: { value: 'a' } }), run('y', 'editoutput', { lua: { value: 'a' } })], { emotions, lua: true })
add('cache-hook-key', [script('a', '@@emo happy')], [run('x', 'editoutput', { hooks: [{ value: 'a' }] }), run('y', 'editoutput', { hooks: [{ value: 'a' }] })],
    { emotions, hooks: { editoutput: 1 } })
add('cache-empty-scripts', [], [run('a'), run('a')])
add('cache-other-mode-scripts', [script('a', '@@emo happy', 'editinput')], [run('a'), run('a')], { emotions })
add('cache-large-entry', [script('a', '@@emo happy')], [run({ repeat: 'a', times: 1_100_000 }), run({ repeat: 'a', times: 1_100_000 })], { emotions, profile: 'low-spec' })
add('cache-fits-entry', [script('a', '@@emo happy')], [run({ repeat: 'a', times: 500_000 }), run({ repeat: 'a', times: 500_000 })], { emotions, profile: 'low-spec' })
add('cache-normal-large', [script('a', '@@emo happy')], [run({ repeat: 'a', times: 2_200_000 }), run({ repeat: 'a', times: 2_200_000 })], { emotions })
add('cache-low-spec-evicts', [script('a', '@@emo happy')], [run('a'), run('a', 'editoutput', { fill: 250 }), run('a')], { emotions, profile: 'low-spec' })
add('cache-low-spec-keeps', [script('a', '@@emo happy')], [run('a'), run('a', 'editoutput', { fill: 249 }), run('a')], { emotions, profile: 'low-spec' })
add('cache-touch-keeps', [script('a', '@@emo happy')], [run('a'), run('a', 'editoutput', { fill: 200 }), run('a'), run('b', 'editoutput', { fill: 100 }), run('a')],
    { emotions, profile: 'low-spec' })
add('cache-byte-evicts', [script('a', '@@emo happy')], [run('a'), run({ repeat: 'b', times: 400_000 }), run({ repeat: 'c', times: 400_000 }), run('a')],
    { emotions, profile: 'low-spec' })
add('cache-flag-in-key', [flagged('A', '@@emo happy', 'gi<cbs>')], [run('a'), run('a')], { emotions })

// Pipelines over mixed text.
for (const [name, scripts, input] of [
    ['chain', [script('a', 'b'), script('b', 'c'), script('c', 'd')], 'abc'],
    ['chain-empty-out', [script('a', ''), script('^b', 'B')], 'abab'],
    ['korean', [script('안녕', '잘가'), script('(가)$', '[$1]')], '안녕 친구 잘가'],
    ['emoji', [flagged(pair, '[E]', 'gu'), script('E', 'e')], `${pair}x${pair}`],
    ['html-like', [script('<b>(.*?)</b>', '<strong>$1</strong>')], '<b>one</b> <b>two</b>'],
    ['angle-brackets', [script('(\\w+)>', '$1 >')], 'a> b>'],
    ['dollar-input', [script('x', '$&$&')], '$1 x $& x'],
    ['newline-input', [script('\\n', ' / ')], 'a\nb\r\nc'],
    ['crlf', [flagged('\\r?\\n', '|', 'gm')], 'a\r\nb\nc'],
    ['long-template', [script('a', 'xyz'.repeat(100))], 'aa'],
    ['many-scripts', Array.from({ length: 40 }, (_, index) => script(`${index}`, `${index + 1}`)), '0'],
    ['brace-safe', [script('\\[', '(')], '[a] {b} {c'],
    ['bidi', [script('‮', '')], 'a‮b'],
    ['nul', [script('\\0', '0')], 'a\u0000b'],
    ['replacement-gt-newline', [script('x', '<br>'), script('\\n', 'N')], 'axbx'],
]) {
    one(`pipeline-${name}`, scripts, input)
}

const ids = new Set()
for (const entry of cases) { if (ids.has(entry.id)) throw new Error(`duplicate case ${entry.id}`); ids.add(entry.id) }
// One case per line keeps the scripts readable in review.
const text = `{"schema":1,"cases":[\n${cases.map((entry) => JSON.stringify(entry)).join(',\n')}\n]}\n`
JSON.parse(text)
writeFileSync(fileURLToPath(new URL('../../tests/fixtures/rust-core/regex/scripts.cases.json', import.meta.url)), text)
console.log(`${cases.length} cases`)
