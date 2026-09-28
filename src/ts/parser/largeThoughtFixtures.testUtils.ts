// Deterministic synthetic reasoning text for the large expansion checks.

/** The Android stress shape: 500,049 UTF-16 units once wrapped. */
export function repeatedKoreanEmojiThought(repeat = 50_000) {
    return '합성 추론 🐿️ '.repeat(repeat)
}

function random(seed: number) {
    let state = seed >>> 0
    return () => {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0
        return state / 2 ** 32
    }
}

export const graphemeSamples = [
    '\u{1f43f}\u{fe0f}', // variation selector
    '\u{1f44d}\u{1f3fd}', // skin tone
    '\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}', // ZWJ family
    '\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}',
    '\u{1f9d1}\u{1f3ff}\u{200d}\u{1f4bb}',
    '\u{1f1f0}\u{1f1f7}', // regional indicator pair
    '\u{1f469}\u{1f3fb}\u{200d}\u{1f9b0}',
    'e\u{301}', // combining marks
    'a\u{308}\u{323}',
    '\u{1112}\u{1161}\u{11ab}', // conjoining jamo
    '\r\n',
]

/** Hangul syllables mixed with multi-code-point graphemes and rare CRLF breaks. */
export function mixedGraphemeText(
    length: number,
    { spaces = true, seed = 7 }: { spaces?: boolean; seed?: number } = {},
) {
    const next = random(seed)
    const parts: string[] = []
    let size = 0
    while (size < length) {
        const roll = next()
        const part =
            roll < 0.7
                ? String.fromCharCode(0xac00 + Math.floor(next() * 11172))
                : roll < 0.85
                  ? graphemeSamples[Math.floor(next() * graphemeSamples.length)]
                  : spaces
                    ? ' '
                    : String.fromCharCode(0xac00 + Math.floor(next() * 11172))
        parts.push(part)
        size += part.length
    }
    return parts.join('')
}

export function asciiControlText(length: number, seed = 7) {
    const next = random(seed)
    let text = ''
    while (text.length < length)
        text += next() < 0.17 ? ' ' : String.fromCharCode(97 + Math.floor(next() * 26))
    return text
}

export function wrapThought(body: string) {
    return `<Thoughts>${body}\nLATEST-SYNTHETIC</Thoughts>\n**Answer**`
}
