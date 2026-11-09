//! The text steps of a response: incomplete-sentence removal and the emotion inlay.

use risunest_ir::stage::StageError;
use risunest_ir::text::is_whitespace;
use risunest_ir::{Budget, Text};

/// `isLastCharPunctuation`'s list (`src/ts/util.ts:474-478`) as UTF-16 units.
const PUNCTUATION: &[u16] = &[
    0x2e, 0x21, 0x3f, 0x3002, 0xff01, 0xff1f, 0x2026, 0x40, 0x23, 0x24, 0x25, 0x5e, 0x26, 0x2a, 0x28, 0x29, 0x2d, 0x5f, 0x2b,
    0x3d, 0x7b, 0x7d, 0x5b, 0x5d, 0x7c, 0x5c, 0x3a, 0x3b, 0x3c, 0x3e, 0x2c, 0x2f, 0x7e, 0x60, 0x20, 0xa1, 0xbf, 0x203d, 0x2049,
    0x27, 0x22,
];

/// The list, then spacing modifier letters, combining diacritical marks, Hebrew punctuation and CJK symbols and
/// punctuation by code unit (`src/ts/util.ts:480-489`).
fn is_punctuation(unit: u16) -> bool {
    PUNCTUATION.contains(&unit)
        || (0x02b0..=0x02ff).contains(&unit)
        || (0x0300..=0x036f).contains(&unit)
        || (0x0590..=0x05cf).contains(&unit)
        || (0x3000..=0x303f).contains(&unit)
}

/// `trimUntilPunctuation(s)` (`src/ts/util.ts:494-500`): drops the last UTF-16 unit while the last unit of the trimmed
/// text is not punctuation. The whitespace after the last punctuation stays, and a text that trims to nothing is kept.
pub fn trim_until_punctuation(text: &Text, budget: &Budget) -> Result<Text, StageError> {
    budget.work(text.len_units())?;
    let units: Vec<u16> = text.units().collect();
    let mut end = units.len();
    while let Some(last) = units[..end].iter().rposition(|unit| !is_whitespace(*unit)) {
        if is_punctuation(units[last]) { break; }
        end = last;
    }
    if end == units.len() { return Ok(text.copy(budget)?); }
    Ok(Text::from_units(&units[..end], budget)?)
}

const OPEN: &[u16] = &[0x3c, 0x65, 0x6d, 0x6f, 0x74, 0x69, 0x6f, 0x6e, 0x3d, 0x22]; // <emotion="
const REPLACE_OPEN: &str = "{{emotion::";
const REPLACE_CLOSE: &str = "}}";

/// What `.` without the `s` flag does not match.
fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }

/// `<Emotion="` at `at`, ignoring ASCII case: without the `u` flag, case-insensitive matching never folds a non-ASCII
/// unit onto an ASCII letter.
fn opens(units: &[u16], at: usize) -> bool {
    units.len() - at >= OPEN.len()
        && OPEN.iter().zip(&units[at..]).all(|(expected, unit)| {
            *unit == *expected || ((0x61..=0x7a).contains(expected) && *unit == *expected - 0x20)
        })
}

/// `data.replace(/<Emotion="(.+?)">/gi, '{{emotion::$1}}')` (`src/ts/process/inlayScreen.ts:9-11`). The lazy group
/// takes the fewest units, at least one and none a line terminator, that a `">` follows.
pub fn inlay_emotion(text: &Text, budget: &Budget) -> Result<Text, StageError> {
    budget.work(text.len_units())?;
    let units: Vec<u16> = text.units().collect();
    let mut output: Vec<u16> = Vec::new();
    let (mut copied, mut at) = (0, 0);
    while at < units.len() {
        if units[at] == 0x3c && opens(&units, at) {
            let start = at + OPEN.len();
            let mut end = start;
            let mut found = None;
            while end < units.len() && !is_line_terminator(units[end]) {
                budget.work(1)?;
                end += 1;
                if units.get(end) == Some(&0x22) && units.get(end + 1) == Some(&0x3e) { found = Some(end); break; }
            }
            if let Some(end) = found {
                budget.work(end - copied + REPLACE_OPEN.len() + REPLACE_CLOSE.len())?;
                output.extend_from_slice(&units[copied..at]);
                output.extend(REPLACE_OPEN.encode_utf16());
                output.extend_from_slice(&units[start..end]);
                output.extend(REPLACE_CLOSE.encode_utf16());
                at = end + 2;
                copied = at;
                continue;
            }
        }
        at += 1;
    }
    if copied == 0 { return Ok(text.copy(budget)?); }
    output.extend_from_slice(&units[copied..]);
    Ok(Text::from_units(&output, budget)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
            max_depth: 64, max_work: 1 << 30 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn units(text: &Text) -> Vec<u16> { text.units().collect() }
    fn text(units: &[u16], budget: &Budget) -> Text { Text::from_units(units, budget).unwrap() }

    #[test]
    fn trim_keeps_text_that_trims_to_nothing_and_drops_lone_surrogates() {
        let budget = budget();
        let blank = text(&[0x20, 0x0a, 0x20], &budget);
        assert_eq!(units(&trim_until_punctuation(&blank, &budget).unwrap()), vec![0x20, 0x0a, 0x20]);
        // `Hi.` then a lone high surrogate and a space: the trimmed last unit is the surrogate, which goes.
        let tail = text(&[0x48, 0x69, 0x2e, 0x20, 0xd800, 0x20], &budget);
        assert_eq!(units(&trim_until_punctuation(&tail, &budget).unwrap()), vec![0x48, 0x69, 0x2e, 0x20]);
        // A combining mark ends a sentence.
        let mark = text(&[0x61, 0x0301], &budget);
        assert_eq!(units(&trim_until_punctuation(&mark, &budget).unwrap()), vec![0x61, 0x0301]);
    }

    #[test]
    fn inlay_folds_only_ascii_case() {
        let budget = budget();
        // `<EMOTİON="x">` with U+0130 is no match, and a group cannot cross a line break.
        let input: Vec<u16> = "<EMOT\u{130}ON=\"x\"> <eMoTiOn=\"a\nb\"> <emotion=\"y\">".encode_utf16().collect();
        let output = inlay_emotion(&text(&input, &budget), &budget).unwrap();
        let expected: Vec<u16> = "<EMOT\u{130}ON=\"x\"> <eMoTiOn=\"a\nb\"> {{emotion::y}}".encode_utf16().collect();
        assert_eq!(units(&output), expected);
    }
}
