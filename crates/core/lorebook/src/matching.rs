//! Keys, secondary keys, regex keys, case and full-word rules.
//!
//! Keyword matching is always case-insensitive: keys and scanned text are lowercased (`toLocaleLowerCase`, ported
//! with the root locale), `{{//...}}` and `{{comment:...}}` are removed from the scanned text, and a key matches
//! when it occurs with every U+0020 removed from both sides, or, under full-word matching, when it equals one
//! U+0020-separated word. A regex key `/source/flags` is tested against the raw message text instead.
//! `extentions.risu_case_sensitive` is not read.
//!
//! `toLocaleLowerCase` follows the default locale. Under a tailored one (`tr`, `az` or `lt`; `el` tailors only
//! upper case) a text holding a unit [`tailored_difference`] finds is declined with `case-locale` instead.

use crate::decorators::trim_units;
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use std::borrow::Cow;

const SPACE: u16 = b' ' as u16;

/// The case tables of the engine whose lore is being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseTables { Native, Unicode16, Foreign }

/// One parsed key (`SearchKey`, `src/ts/process/lorebook.svelte.ts:92`).
#[derive(Debug)]
pub struct SearchKey {
    /// The trimmed key as written.
    pub raw: Text,
    pub raw_units: Vec<u16>,
    pub lower: Vec<u16>,
    pub no_space: Vec<u16>,
}

/// `parseSearchKeys`: trims each key, drops empty ones and lowercases the rest.
pub fn parse_search_keys(keys: &[Vec<u16>], tailored: bool, budget: &Budget) -> Result<Vec<SearchKey>, StageError> {
    parse_search_keys_with_tables(keys, tailored, CaseTables::Native, budget)
}

pub(crate) fn parse_search_keys_with_tables(keys: &[Vec<u16>], tailored: bool, tables: CaseTables, budget: &Budget)
    -> Result<Vec<SearchKey>, StageError> {
    let mut parsed = Vec::new();
    for key in keys {
        budget.work(key.len().saturating_add(1))?;
        let raw = trim_units(key);
        if raw.is_empty() { continue; }
        let lower = lowercase_with_tables(raw, tailored, tables, budget)?;
        let no_space = lower.iter().copied().filter(|unit| *unit != SPACE).collect();
        parsed.push(SearchKey { raw: Text::from_units(raw, budget)?, raw_units: raw.to_vec(), lower, no_space });
    }
    Ok(parsed)
}

/// `key.split(',')`.
pub fn split_keys(key: &Text, budget: &Budget) -> Result<Vec<Vec<u16>>, StageError> {
    let units: Vec<u16> = key.units().collect();
    budget.work(units.len())?;
    Ok(units.split(|unit| *unit == b',' as u16).map(<[u16]>::to_vec).collect())
}

/// Whether `toLocaleLowerCase` maps `units` differently under a tailored locale than under the root mapping: `I`
/// and `İ` (`tr`, `az`), `Ì`, `Í` and `Ĩ` (`lt`), and `J` or `Į` followed by a unit from U+0300 on, which may be a
/// combining mark above (`lt`). `I` followed by such a mark is already covered by `I`.
pub fn tailored_difference(units: &[u16]) -> bool {
    units.iter().enumerate().any(|(at, unit)| match unit {
        0x49 | 0x130 | 0xcc | 0xcd | 0x128 => true,
        0x4a | 0x12e => units.get(at + 1).is_some_and(|next| *next >= 0x300),
        _ => false,
    })
}

/// `String.prototype.toLocaleLowerCase` over UTF-16 units with the root mapping; `tailored` declines a text a
/// tailored default locale would map differently. An isolated surrogate is kept and splits the text into runs, which
/// leaves the final-sigma context as JS sees it.
pub fn lowercase(units: &[u16], tailored: bool, budget: &Budget) -> Result<Vec<u16>, StageError> {
    lowercase_with_tables(units, tailored, CaseTables::Native, budget)
}

pub(crate) fn lowercase_with_tables(units: &[u16], tailored: bool, tables: CaseTables, budget: &Budget)
    -> Result<Vec<u16>, StageError> {
    budget.work(units.len())?;
    if tailored && tailored_difference(units) { return Err(StageError::Unsupported(Cow::Borrowed("case-locale"))); }
    let differs = match tables {
        CaseTables::Native => false,
        CaseTables::Unicode16 => risunest_ir::text::unicode16_case_differs(units),
        CaseTables::Foreign => units.iter().any(|unit| *unit >= 128),
    };
    if differs { return Err(StageError::Unsupported(Cow::Borrowed("case-table"))); }
    let mut output = Vec::with_capacity(units.len());
    if units.iter().all(|unit| *unit < 0x80) {
        output.extend(units.iter().map(|unit| if (b'A' as u16..=b'Z' as u16).contains(unit) { unit + 32 } else { *unit }));
        return Ok(output);
    }
    let mut run = String::new();
    let flush = |run: &mut String, output: &mut Vec<u16>| {
        if !run.is_empty() { output.extend(run.to_lowercase().encode_utf16()); run.clear(); }
    };
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok(character) => run.push(character),
            Err(error) => { flush(&mut run, &mut output); output.push(error.unpaired_surrogate()); }
        }
    }
    flush(&mut run, &mut output);
    Ok(output)
}

fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }

/// Work charged as it is done, in batches, so a scan stops at the work limit instead of finishing first.
pub(crate) struct Steps<'a> { budget: &'a Budget, pending: usize }

impl<'a> Steps<'a> {
    pub(crate) fn new(budget: &'a Budget) -> Self { Self { budget, pending: 0 } }
    pub(crate) fn add(&mut self, steps: usize) -> Result<(), StageError> {
        self.pending = self.pending.saturating_add(steps);
        if self.pending >= 1024 { self.budget.work(std::mem::take(&mut self.pending))?; }
        Ok(())
    }
    pub(crate) fn finish(self) -> Result<(), StageError> { Ok(self.budget.work(self.pending)?) }
}

/// `text.replace(/<prefix>(.+?)\}\}/g, '')` for a literal prefix. The next `}}` and the next line terminator are
/// found by positions that only move forward, so many unclosed prefixes on one line stay linear.
fn strip_macro(units: &[u16], prefix: &[u16], budget: &Budget) -> Result<Vec<u16>, StageError> {
    let close = b'}' as u16;
    let length = units.len();
    let mut steps = Steps::new(budget);
    let mut output = Vec::with_capacity(length);
    // The first `}}` and the first line terminator at or after the latest body start; `length` when there is none.
    let (mut close_at, mut line_at) = (0, 0);
    let mut at = 0;
    'scan: while at < length {
        steps.add(1)?;
        if units[at..].starts_with(prefix) {
            steps.add(prefix.len())?;
            let body = at + prefix.len();
            if body < length && !is_line_terminator(units[body]) {
                // `.+?` takes at least one unit, so the closing `}}` starts after the body's first unit.
                let from = body + 1;
                if close_at < from {
                    close_at = from;
                    while close_at + 1 < length && !(units[close_at] == close && units[close_at + 1] == close) {
                        close_at += 1;
                        steps.add(1)?;
                    }
                    if close_at + 1 >= length { close_at = length; }
                }
                if line_at < from {
                    line_at = from;
                    while line_at < length && !is_line_terminator(units[line_at]) {
                        line_at += 1;
                        steps.add(1)?;
                    }
                }
                if close_at < line_at {
                    at = close_at + 2;
                    continue 'scan;
                }
            }
        }
        output.push(units[at]);
        at += 1;
    }
    steps.finish()?;
    Ok(output)
}

/// The normalization `createMessageView` applies: lowercase, then remove `{{//...}}`, then `{{comment:...}}`.
pub fn normalize(units: &[u16], tailored: bool, budget: &Budget) -> Result<Vec<u16>, StageError> {
    normalize_with_tables(units, tailored, CaseTables::Native, budget)
}

fn normalize_with_tables(units: &[u16], tailored: bool, tables: CaseTables, budget: &Budget) -> Result<Vec<u16>, StageError> {
    let lower = lowercase_with_tables(units, tailored, tables, budget)?;
    let without_comments = strip_macro(&lower, &"{{//".encode_utf16().collect::<Vec<_>>(), budget)?;
    strip_macro(&without_comments, &"{{comment:".encode_utf16().collect::<Vec<_>>(), budget)
}

/// One scanned text (`MessageView`, `src/ts/process/lorebook.svelte.ts:97`). Every buffer is charged to the budget
/// for as long as the view lives.
#[derive(Debug)]
pub struct MessageView {
    pub source: Text,
    pub prompt: Text,
    pub data: Text,
    pub normalized_prompt: Text,
    pub normalized_data: OwnedVec<u16>,
    /// `normalizedData` with every U+0020 removed.
    pub no_space_data: OwnedVec<u16>,
    /// Where each word of `normalizedData.split(' ')` ends in `normalized_data`; the next one starts after the space.
    word_ends: OwnedVec<usize>,
}

impl MessageView {
    /// `createMessageView(source, prompt, data)`.
    pub fn new(source: Text, prompt: Text, data: Text, tailored: bool, budget: &Budget) -> Result<Self, StageError> {
        Self::with_case_tables(source, prompt, data, tailored, CaseTables::Native, budget)
    }

    pub(crate) fn with_case_tables(source: Text, prompt: Text, data: Text, tailored: bool, tables: CaseTables, budget: &Budget)
        -> Result<Self, StageError> {
        let prompt_units: Vec<u16> = prompt.units().collect();
        let data_units: Vec<u16> = data.units().collect();
        let normalized_prompt = Text::from_units(&normalize_with_tables(&prompt_units, tailored, tables, budget)?, budget)?;
        let normalized = normalize_with_tables(&data_units, tailored, tables, budget)?;
        let mut normalized_data = OwnedVec::new(budget)?;
        normalized_data.extend(&normalized)?;
        let mut no_space_data = OwnedVec::new(budget)?;
        let mut word_ends = OwnedVec::new(budget)?;
        for (at, unit) in normalized.iter().enumerate() {
            if *unit == SPACE { word_ends.push(at)?; } else { no_space_data.push(*unit)?; }
        }
        word_ends.push(normalized.len())?;
        Ok(Self { source, prompt, data, normalized_prompt, normalized_data, no_space_data, word_ends })
    }

    /// `words.includes(word)`.
    pub fn has_word(&self, word: &[u16], budget: &Budget) -> Result<bool, StageError> {
        let data = self.normalized_data.as_slice();
        let mut steps = Steps::new(budget);
        let mut start = 0;
        for end in self.word_ends.as_slice() {
            let candidate = &data[start..*end];
            steps.add(if candidate.len() == word.len() { word.len().saturating_add(1) } else { 1 })?;
            if candidate == word {
                steps.finish()?;
                return Ok(true);
            }
            start = end + 1;
        }
        steps.finish()?;
        Ok(false)
    }
}

/// `String.prototype.indexOf` over UTF-16 units, by Knuth-Morris-Pratt so the scan stays linear.
pub fn index_of(haystack: &[u16], needle: &[u16], budget: &Budget) -> Result<Option<usize>, StageError> {
    if needle.is_empty() { return Ok(Some(0)); }
    if needle.len() > haystack.len() { return Ok(None); }
    let _charge = budget.reserve_owned(needle.len().saturating_mul(std::mem::size_of::<usize>()))?;
    let mut steps = Steps::new(budget);
    // `fallback[at]`: the length of the longest proper prefix of `needle[..=at]` that is also its suffix.
    let mut fallback = vec![0usize; needle.len()];
    let mut matched = 0;
    for at in 1..needle.len() {
        steps.add(1)?;
        while matched > 0 && needle[at] != needle[matched] {
            matched = fallback[matched - 1];
            steps.add(1)?;
        }
        if needle[at] == needle[matched] { matched += 1; }
        fallback[at] = matched;
    }
    matched = 0;
    for (at, unit) in haystack.iter().enumerate() {
        steps.add(1)?;
        while matched > 0 && *unit != needle[matched] {
            matched = fallback[matched - 1];
            steps.add(1)?;
        }
        if *unit == needle[matched] {
            matched += 1;
            if matched == needle.len() {
                steps.finish()?;
                return Ok(Some(at + 1 - needle.len()));
            }
        }
    }
    steps.finish()?;
    Ok(None)
}

/// `String.prototype.includes` over UTF-16 units.
pub fn contains(haystack: &[u16], needle: &[u16], budget: &Budget) -> Result<bool, StageError> {
    Ok(index_of(haystack, needle, budget)?.is_some())
}

/// A `/source/flags` key as `^\/([\s\S]*)\/([dgimsuvy]*)$` splits it: the last `/` followed only by flag letters.
pub fn regex_literal(raw: &[u16]) -> Option<(&[u16], &[u16])> {
    let slash = b'/' as u16;
    if raw.first() != Some(&slash) { return None; }
    let flag = |unit: &u16| matches!(char::from_u32(*unit as u32), Some('d' | 'g' | 'i' | 'm' | 's' | 'u' | 'v' | 'y'));
    (1..raw.len()).rev().find(|at| raw[*at] == slash && raw[at + 1..].iter().all(flag)).map(|at| (&raw[1..at], &raw[at + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
            max_depth: 32, max_work: 1 << 28 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn units(text: &str) -> Vec<u16> { text.encode_utf16().collect() }

    #[test]
    fn normalization_lowercases_then_removes_comment_macros_on_one_line() {
        let budget = budget();
        let cases = [("A{{//X}}b", "ab"), ("{{//}}c", "{{//}}c"), ("{{//a\nb}}c", "{{//a\nb}}c"), ("{{COMMENT:Hi}}}x", "}x"),
            ("{{//{{comment:a}}}}z", "}}z"), ("ΣΑΣ Σ", "σας σ")];
        for (input, expected) in cases {
            assert_eq!(normalize(&units(input), false, &budget).unwrap(), units(expected), "{input}");
        }
    }

    #[test]
    fn a_tailored_locale_declines_text_it_lowercases_differently() {
        let budget = budget();
        let declined = Err(StageError::Unsupported(Cow::Borrowed("case-locale")));
        for text in ["I", "\u{130}", "\u{cc}", "\u{cd}", "\u{128}", "J\u{301}", "\u{12e}\u{300}", "aJ\u{483}"] {
            assert!(tailored_difference(&units(text)), "{text}");
            assert_eq!(lowercase(&units(text), true, &budget), declined, "{text}");
        }
        for text in ["i", "J", "J\u{fc}rgen", "\u{12e}", "\u{131}", "\u{3a3}\u{391}\u{3a3}"] {
            assert!(!tailored_difference(&units(text)), "{text}");
            assert_eq!(lowercase(&units(text), true, &budget).unwrap(), lowercase(&units(text), false, &budget).unwrap());
        }
    }

    #[test]
    fn captured_tables_guard_the_consumed_lowercase_input() {
        let budget = budget();
        let table = Err(StageError::Unsupported(Cow::Borrowed("case-table")));
        assert_eq!(lowercase_with_tables(&units("ASCII"), false, CaseTables::Foreign, &budget).unwrap(), units("ascii"));
        assert_eq!(lowercase_with_tables(&units("ÉΣ"), false, CaseTables::Unicode16, &budget).unwrap(), units("éς"));
        assert_eq!(lowercase_with_tables(&units("É"), false, CaseTables::Foreign, &budget), table);
        assert_eq!(lowercase_with_tables(&units("\u{1acf}"), false, CaseTables::Unicode16, &budget), table);
        assert_eq!(lowercase_with_tables(&units("\u{1acf}"), false, CaseTables::Native, &budget).unwrap(), units("\u{1acf}"));
        assert_eq!(lowercase_with_tables(&units("Ié"), true, CaseTables::Foreign, &budget),
            Err(StageError::Unsupported(Cow::Borrowed("case-locale"))));
        assert_eq!(lowercase_with_tables(&[0xd800, 65], false, CaseTables::Unicode16, &budget).unwrap(), [0xd800, 97]);
        assert_eq!(normalize_with_tables(&units("{{//\u{1acf}}}"), false, CaseTables::Unicode16, &budget), table);
    }

    #[test]
    fn unclosed_macros_and_near_miss_keys_scan_in_linear_work() {
        // A rescan per position would take about 10^9 steps for the line and 5 * 10^8 for the key.
        let budget = Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
            max_values: 1 << 16, max_depth: 32, max_work: 1 << 21 }, Arc::new(AtomicBool::new(false))).unwrap();
        let line = units(&"{{//a".repeat(20_000));
        assert_eq!(normalize(&line, false, &budget).unwrap(), line);
        let mut needle = units(&"a".repeat(5_000));
        needle.push(b'b' as u16);
        assert!(!contains(&units(&"a".repeat(100_000)), &needle, &budget).unwrap());
        assert_eq!(index_of(&units("xaaab"), &units("aab"), &budget).unwrap(), Some(2));
        assert_eq!(index_of(&units("abcabcabd"), &units("abcabd"), &budget).unwrap(), Some(3));
        assert_eq!(index_of(&units("abc"), &units(""), &budget).unwrap(), Some(0));
    }

    #[test]
    fn a_message_view_charges_its_buffers_while_it_lives() {
        let budget = budget();
        let before = budget.owned_bytes();
        let data = "word ".repeat(1000);
        let text = |value: &str| Text::from_str(value, &budget).unwrap();
        let view = MessageView::new(text("s"), text(&data), text(&data), false, &budget).unwrap();
        // The prompt, data and normalized prompt (one byte per ASCII unit), then the normalized data, its copy
        // without spaces and the word ends.
        let charged = 3 * 5000 + 5000 * 2 + 4000 * 2 + 1001 * std::mem::size_of::<usize>();
        assert!(budget.owned_bytes() - before >= charged, "{} bytes charged", budget.owned_bytes() - before);
        assert_eq!(view.no_space_data.as_slice(), units(&"word".repeat(1000)));
        assert!(view.has_word(&units("word"), &budget).unwrap());
        assert!(!view.has_word(&units("wor"), &budget).unwrap());
        // `split(' ')` leaves an empty last word after the trailing space.
        assert!(view.has_word(&[], &budget).unwrap());
        drop(view);
        assert_eq!(budget.owned_bytes(), before);
    }

    #[test]
    fn regex_keys_split_at_the_last_slash_followed_only_by_flags() {
        let key = units("/a/b/gi");
        assert_eq!(regex_literal(&key), Some((&units("a/b")[..], &units("gi")[..])));
        assert_eq!(regex_literal(&units("/abc/xyz")), None);
        assert_eq!(regex_literal(&units("//")), Some((&[][..], &[][..])));
        assert_eq!(regex_literal(&units("/")), None);
        assert_eq!(regex_literal(&units("abc/")), None);
    }
}
