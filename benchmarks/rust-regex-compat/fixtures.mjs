export const units = value => Array.from({ length: value.length }, (_, index) => value.charCodeAt(index))
export const rule = (pattern, flags = 'g', replacement = 'X') => ({ pattern: units(pattern), flags, replacement: units(replacement) })
export const text = value => {
    let output = ''
    for (let index = 0; index < value.length; index += 8192) output += String.fromCharCode(...value.slice(index, index + 8192))
    return output
}

// Synthetic matrix recovered from the standalone engine-study experiment.
export function cases() {
    const output = []
    const patterns = ['a', '(a)(b)?', '(?<first>a)(?<second>b)?', '(?<=a)b', 'a(?=b)', '(a|b)\\1', '(?<x>a)\\k<x>', '(?:)', '^|$', '.*?', '[a-z]+', '\\w+', '\\d+', '\\s+', '😀', '[😀]', '[\\p{Letter}&&\\p{ASCII}]', '\\p{Emoji}', '(?<=([ab]+)([bc]+))$', '(?:a|(b))\\1', '(?<x>a)|(?<x>b)', 'a{2,1}', '[z-a]']
    const inputs = ['ab aab abb\n😀한글 İ ı ſ K\r\n', '😀', 'x\ud800y', '', 'aaaba']
    const replacements = ['X', '$&|$`|$\u0027|$$', '$0:$1:$2:$10:$01:$99', '$<first>:$<second>:$<absent>']
    for (const [pi, pattern] of patterns.entries()) for (const flags of ['g', 'gu', 'gy', 'gim', 'gv']) for (const [ii, input] of inputs.entries()) {
        output.push({ id: `retained-${pi}-${flags}-${ii}`, rules: [rule(pattern, flags, replacements[(pi + ii) % replacements.length])], input: units(input), unsupported: flags === 'gv' ? 'unicode_sets' : undefined })
    }
    for (const flags of ['gg', 'z', 'uv']) output.push({ id: `invalid-${flags}`, rules: [rule('a', flags)], input: units('a') })
    for (const flags of ['g', 'gu', 'gy', 'guy', 'gdu', '', 'y', 'u']) {
        for (const pattern of ['(?:)', 'a*', '(?=.)', '(?!a)', '.', '[😀]', '\ud800', '\udc00', '😀', '(?<=.)', '(?<x>a)?b']) {
            for (const input of ['😀a', '\ud800a\udc00', 'aa', '', 'ab']) {
                output.push({ id: `utf16-${output.length}`, rules: [rule(pattern, flags, '|$&:$<x>|')], input: units(input), required: true })
            }
        }
    }
    for (const flags of ['g', 'gu', 'gui', 'gyu']) {
        for (const pattern of ['(a)(b)?', '(?<first>a)(?<second>b)?', '(a)|(b)', '(?<x>a)|(?<x>b)', '(?<=([ab]+)([bc]+))$', '(a)\\1', '(?<x>a)\\k<x>']) {
            for (const replacement of ['$0:$00:$01:$02:$09:$10:$12:$99:$100', '$$-$&-$`-$\u0027-$', '$<first>:$<second>:$<absent>:$<>:$<', '\ud800$1\udc00', '$<x>:$<\ud800>']) {
                output.push({ id: `substitution-${output.length}`, rules: [rule(pattern, flags, replacement)], input: units('aab abab 😀'), required: true })
            }
        }
    }
    output.push({ id: 'ordered-backreference-lookbehind', rules: [rule('(a)\\1', 'g', '$1b'), rule('(?<=a)b', 'g', '$`:$&:$\u0027')], input: [0xd800, 97, 97, 0xdc00], required: true })
    output.push({ id: 'ordered-dependent-chain', rules: Array.from({ length: 100 }, (_, index) => rule(`S${index}!`, 'g', `S${index + 1}!`)), input: units('S0!😀'), required: true })
    for (const pattern of ['k', 's', '[a-z]+', '\\w+']) output.push({ id: `unicode-case-${pattern}`, rules: [rule(pattern, 'gui', '[$&]')], input: units('Kkſsİıſ K'), required: true })
    for (const flags of ['g', 'gm', 'gs', 'gms', 'gmu', 'gsu', 'gmsu', 'gdu']) {
        for (const pattern of ['.', '^a', 'b$', '^|$', '.+?', 'a.*b']) {
            for (const input of ['a\nb', 'a\rb', 'a\r\nb', 'a\u2028b', 'a\u2029b', '😀\na', '']) {
                output.push({ id: `line-flags-${output.length}`, rules: [rule(pattern, flags, '[$&]')], input: units(input), required: true })
            }
        }
    }
    for (const probe of escapeProbes()) output.push({ ...probe, required: true })
    output.push(...unicodeCases())
    for (const flags of ['g', 'gu']) output.push({ id: `escaped-backslash-${flags}`, rules: [rule(String.raw`\\u0061`, flags)], input: units(String.raw`\u0061`), required: true })
    // Exercise every BMP uppercase mapping and expansion through Node's real RegExp.
    // Inputs include both directions, captures and a nonmatching sentinel.
    for (let unit = 0; unit <= 0xffff; unit++) {
        const source = String.fromCharCode(unit), upper = source.toUpperCase()
        if (source === upper) continue
        const escaped = `\\u${unit.toString(16).padStart(4, '0')}`
        const input = `${source}${upper}|${upper}${source}|${source}${source}|${upper}${upper}|!`
        for (const pattern of [`[${escaped}]`, `(${escaped})\\1`]) {
            output.push({ id: `legacy-canonical-${unit}-${pattern}`, rules: [rule(pattern, 'gi', '[$&:$1]')], input: units(input), required: true })
        }
    }
    for (const flags of ['gi', 'giu']) for (const pattern of ['\\W', '[\\W]', '[^\\W]', '[\\Wk]', '[^\\Ws]', '[\\w\\W]', '[^\\w\\W]']) {
        output.push({ id: `canonical-word-${flags}-${pattern}`, rules: [rule(pattern, flags, '[$&]')], input: units('sSſ kKK iIİı ßẞ éÉ ᾀᾈ 𐐀𐐨 \ud800-'), required: true })
    }
    return output
}

export function escapeProbes() {
    return [
        [String.raw`\u{61}`, 'g', 'a'],
        [String.raw`\u{61}`, 'g', 'u'.repeat(61)],
        [String.raw`\uD83D\uDE00`, 'g', '😀'],
        [String.raw`[\uD83D\uDE00]`, 'g', '😀'],
        [String.raw`\uD800\u0041`, 'gu', '\ud800A'],
        [String.raw`\uD800\uD800`, 'gu', '\ud800\ud800'],
        [String.raw`\uD800\u{41}`, 'gu', '\ud800A'],
    ].map(([pattern, flags, input], index) => ({ id: `escape-semantics-${index}`, rules: [rule(pattern, flags)], input: units(input) }))
}
export function unicodeCases() {
    const output = []
    const patterns = [
        String.raw`\u0061`, String.raw`\u0028`, String.raw`\u002a`, String.raw`\u005b`,
        String.raw`\u005c`, String.raw`\u0024`, String.raw`\u005e`, String.raw`\u007c`,
        String.raw`[\u0061-\u0063]`, String.raw`[\uD800-\uDFFF]`,
        String.raw`\uD83D\uDE00`, String.raw`[\uD83D\uDE00]`,
        String.raw`\uD800\u0041`, String.raw`\uD800\uD800`, String.raw`\uD800\u{41}`,
        String.raw`\u{D800}\u{DC00}`, String.raw`\u{D800}`, String.raw`\uDC00`,
        String.raw`\u{0}`, String.raw`\u{61}`, String.raw`\u{1F600}`,
        String.raw`\u{10FFFF}`, String.raw`\u{0000000000000061}`,
        String.raw`\\u0061`, String.raw`\\\u0061`, String.raw`\\\\u0061`,
        '\\uD800\udc00', '\ud800\\uDC00',
    ]
    const inputs = ['aabc', '(*[\\$^|', '😀', '\ud800A', '\ud800\ud800', '\ud800', '\udc00', '\ud800\udc00', '\\a', String.raw`\u0061`, String.raw`\\u0061`, 'u'.repeat(61), '', '\u{10ffff}']
    const replacements = ['X', '$&|$`|$\u0027|$$', '$0:$01:$1:$99', '\ud800$&\udc00']
    for (const flags of ['g', 'gu', 'gy', 'guy']) for (const pattern of patterns) for (const [index, input] of inputs.entries()) {
        output.push({ id: `unicode-body-${output.length}`, rules: [rule(pattern, flags, replacements[index % replacements.length])], input: units(input), required: true })
    }
    for (const flags of ['g', 'gu', 'gy', 'guy']) {
        for (const [name, spellings] of [['𐐀', ['𐐀', String.raw`\uD801\uDC00`, String.raw`\u{10400}`]], ['a', ['a', String.raw`\u0061`, String.raw`\u{61}`]]]) {
            for (const definition of spellings) for (const reference of spellings) {
                output.push({ id: `unicode-name-${output.length}`, rules: [rule(`(?<${definition}>a)\\k<${reference}>`, flags, `$<${name}>:$1:$$:$&`)], input: units('aaz'), required: true })
            }
        }
        for (const pattern of [String.raw`(?<a\u003e>z)`, String.raw`(?<a\u{3e}>z)`, String.raw`(?<a>z)\k<a\u003e>`]) {
            output.push({ id: `unicode-v8-delimiter-${output.length}`, rules: [rule(pattern, flags)], input: units('a>z'), required: true })
        }
        for (const pattern of [String.raw`(?<\uD800>a)`, String.raw`(?<\uD800\u0041>a)`, '(?<\ud800>a)', '(?<\ud801\\uDC00>a)', '(?<\\uD801\udc00>a)', String.raw`(?<\u{D801}\uDC00>a)`]) {
            output.push({ id: `unicode-invalid-name-${output.length}`, rules: [rule(pattern, flags)], input: units('a>z'), reject: true })
        }
    }
    const malformed = [String.raw`\u`, String.raw`\u123`, String.raw`\uZZZZ`, String.raw`\u+041`, String.raw`\u-041`, String.raw`\u{}`, String.raw`\u{+61}`, String.raw`\u{-61}`, String.raw`\u{ 61}`, String.raw`\u{110000}`, String.raw`\u{123456789}`, String.raw`\u{61`, String.raw`\uD800\u004`, String.raw`\uD800\uZZZZ`]
    for (const flags of ['gu', 'guy']) for (const pattern of malformed) {
        output.push({ id: `unicode-invalid-body-${output.length}`, rules: [rule(pattern, flags)], input: units('u123 uuuu041'), reject: true })
    }
    for (const [pattern, input] of [[String.raw`\u+041`, 'uuu041'], [String.raw`\u-041`, 'u-041'], [String.raw`\uZZZZ`, 'uZZZZ'], [String.raw`\u123`, 'u123'], [String.raw`\uD800\uZZZZ`, '\ud800uZZZZ']]) {
        output.push({ id: `unicode-legacy-identity-${output.length}`, rules: [rule(pattern, 'g', '[$&]')], input: units(input), required: true })
    }
    for (const flags of ['g', 'gu', 'gy', 'guy']) {
        for (const [pattern, input] of [[String.raw`(?<a\u003e>z)`, '>z'], [String.raw`(?<a\u{3e}>z)`, '>z'], [String.raw`(?<a\u003eb>z)`, 'b>z'], [String.raw`(?<a>z)\k<a\u003e>`, 'zz>'], [String.raw`(?<a>z)\k<a\u{3e}>`, 'zz>']]) {
            output.push({ id: `unicode-v8-captures-${output.length}`, rules: [rule(pattern, flags, '$<a>|$&|$1')], input: units(input), required: true })
        }
        for (const pattern of [String.raw`(?<\u003e>z)`, String.raw`(?<a>z)\k<\u003e>`]) {
            output.push({ id: `unicode-invalid-name-start-${output.length}`, rules: [rule(pattern, flags)], input: units('zz>'), reject: true })
        }
    }
    return output
}


// Additive first-match observations; the replacement and Unicode corpora stay separate.
export function controlledCases() {
    const output = []
    const patterns = ['(?<x>a)|(?<x>b)(c)?', '(?<=([ab]+)([bc]+))$', '(?=(a+))a+', '(a{64})\\1', '(?<=\\1(a{64}))$', 'a+', 'a*?', 'a{64}', '(?!a+$)', '(?<!a+)$', '(.)', '(\\uD800)\\1']
    const inputs = ['b', 'abc', 'a'.repeat(128), '😀\ud800a\udc00', '\ud800\ud800']
    for (const pattern of patterns) for (const flags of ['g', 'gu', 'gui']) for (const input of inputs) {
        output.push({ id: `controlled-${output.length}`, rules: [rule(pattern, flags)], input: units(input), start: 0 })
    }
    // Starts outside and immediately before/after a supplementary pair. An interior
    // low-surrogate start remains an explicit old-API observation in Rust tests.
    for (const start of [0, 2, 3, 4, 5, 6]) for (const flags of ['g', 'gu']) {
        output.push({ id: `controlled-start-${flags}-${start}`, rules: [rule('(.)', flags)], input: units('😀\ud800a\udc00'), start })
    }
    return output
}

export function controlledBounds() {
    const input = units('a'.repeat(256))
    const groups = '()'.repeat(32)
    return [
        { id: 'dispatch-backtracking', rules: [rule('(?:a|aa)+$')], input: [...input, 33], limits: { work_units: 128 }, abort: 'WorkLimit' },
        { id: 'candidate-scan', rules: [rule('z')], input: units('a'.repeat(16)), limits: { work_units: 40 }, abort: 'WorkLimit' },
        { id: 'greedy-scan', rules: [rule('a+')], input, limits: { work_units: 32 }, abort: 'WorkLimit' },
        { id: 'minimum-scan', rules: [rule('a{64}')], input, limits: { work_units: 32 }, abort: 'WorkLimit' },
        { id: 'lazy-maximum-scan', rules: [rule('a*?')], input, limits: { work_units: 32 }, abort: 'WorkLimit' },
        ...['g', 'gui'].flatMap(flags => [
            { id: `forward-backref-${flags}`, rules: [rule('(a{128})\\1', flags)], input, limits: { work_units: 200 }, abort: 'WorkLimit' },
            { id: `backward-backref-${flags}`, rules: [rule('(?<=\\1(a{128}))$', flags)], input, start: 256, limits: { work_units: 200 }, abort: 'WorkLimit' },
        ]),
        { id: 'negative-lookahead', rules: [rule('(?!a+$)')], input, limits: { work_units: 32 }, abort: 'WorkLimit' },
        { id: 'negative-lookbehind', rules: [rule('(?<!a+)$')], input, start: 256, limits: { work_units: 32 }, abort: 'WorkLimit' },
        { id: 'capture-initialization', rules: [rule(`z${groups}`)], input: units('b'), limits: { work_units: 24 }, abort: 'WorkLimit' },
        { id: 'capture-copy-restore-result', rules: [rule(`(?!(?:z${groups}))`)], input: units('a'), limits: { work_units: 120 }, abort: 'WorkLimit' },
        { id: 'allocation', rules: [rule('(a)')], input: units('a'), limits: { allocation_bytes: 0 }, abort: 'AllocationBudget' },
        { id: 'depth', rules: [rule('(?=(?=(a)))a')], input: units('a'), limits: { lookaround_depth: 1 }, abort: 'LookaroundDepth' },
        { id: 'already-cancelled', rules: [rule('a')], input: units('a'), cancelled: true, abort: 'Cancelled' },
        { id: 'expired-deadline', rules: [rule('a')], input: units('a'), deadline_ms: 0, abort: 'DeadlineExceeded' },
    ]
}
