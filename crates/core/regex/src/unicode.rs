//! The pattern shapes whose results follow the Unicode version of the engine's tables.

/// The Unicode version of the regress fork's tables: `crates/regress/src/unicodetables.rs` is generated from UCD 17.0.0.
pub const TABLE_VERSION: (u8, u8) = (17, 0);

/// Whether `pattern` under the normalized `flags` can match differently, or fail to compile, on an engine whose tables
/// come from another Unicode version. It over-approximates:
/// - a property escape (`\p{...}`, `\P{...}`) in Unicode mode, whose sets and valid names change between versions;
/// - ignore case (the `i` flag, or a modifier group that may turn it on) with a non-ASCII unit, a `\u` or `\x` escape,
///   a backreference or a legacy octal escape. Canonicalizing a non-ASCII character follows the version's case
///   mappings, and so does a backreference, which compares two input characters by their canonical forms.
///
/// Ignore case over an otherwise ASCII pattern is left alone: legacy canonicalization never maps a non-ASCII character
/// to ASCII, and Unicode simple case folding does so only for U+017F and U+212A, which every version folds alike.
pub fn depends_on_version(pattern: &[u16], flags: &str) -> bool {
    let unicode = flags.contains('u') || flags.contains('v');
    let mut ignore_case = flags.contains('i');
    let mut cased = false;
    let mut index = 0;
    while index < pattern.len() {
        let unit = pattern[index];
        if unit >= 0x80 { cased = true; }
        if unit == 92 {
            let Some(&next) = pattern.get(index + 1) else { break };
            match next {
                112 | 80 if unicode => return true,
                117 | 120 | 107 | 49..=57 => cased = true,
                next if next >= 0x80 => cased = true,
                _ => {}
            }
            index += 2;
            continue;
        }
        // `(?` and modifier letters: an `i` before any `-` turns ignore case on inside the group.
        if unit == 40 && pattern.get(index + 1) == Some(&63) {
            for &letter in pattern[index + 2..].iter().take_while(|unit| (97..=122).contains(*unit)) {
                if letter == 105 { ignore_case = true; }
            }
        }
        index += 1;
    }
    ignore_case && cased
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_escapes_and_cased_ignore_case_depend_on_the_version() {
        let units = |value: &str| value.encode_utf16().collect::<Vec<u16>>();
        for (pattern, flags, expected) in [
            ("\\p{L}+", "gu", true), ("[^\\P{Lu}]", "giu", true), ("\\p{L}", "g", false), ("\\\\p{L}", "gu", false),
            ("hello [a-z]+ \\w\\b\\W\\s\\d.", "giu", false), ("[^k]", "gi", false), ("é", "g", false), ("é", "gi", true),
            ("한", "gi", true), ("\\u00e9", "gi", true), ("\\u{e9}", "giu", true), ("\\x41", "gi", true), ("\\é", "gi", true),
            ("(a)\\1", "gi", true), ("(a)\\1", "g", false), ("(?<n>a)\\k<n>", "gi", true), ("\\101", "gi", true),
            ("(?i:é)", "g", true), ("(?mi:é)", "g", true), ("(?m-i:é)", "g", false), ("(?-i:é)", "gi", true),
            ("(?<=é)a", "g", false), ("(?:é)", "g", false), ("\\(?i:é)", "g", false),
        ] {
            assert_eq!(depends_on_version(&units(pattern), flags), expected, "{pattern} /{flags}");
        }
    }
}
