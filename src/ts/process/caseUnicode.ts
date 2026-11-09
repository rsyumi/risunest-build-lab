/** The Unicode version of one set of the JS engine's tables, as the native stages tell them apart. */
export type EngineUnicode = 'before16' | 'unicode16' | 'unicode17' | 'from18'

/**
 * The Unicode version of `toLowerCase` and `toUpperCase`: U+10D70 gains its capital in Unicode 16 and U+A7D3 in
 * Unicode 17, and Unicode 18 adds the letter U+A7DD, which lowercases to U+0277.
 */
export function caseUnicode(): EngineUnicode {
    if ('\u{10D70}'.toUpperCase() !== '\u{10D50}') return 'before16'
    if ('\u{277}'.toUpperCase() !== '\u{277}' || '\u{A7DD}'.toLowerCase() !== '\u{A7DD}') return 'from18'
    if ('\u{A7D3}'.toUpperCase() !== '\u{A7D3}') return 'unicode17'
    return 'unicode16'
}
