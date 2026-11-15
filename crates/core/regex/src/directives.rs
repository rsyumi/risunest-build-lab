//! `@@` directives.
//!
//! `processScriptFull` (`src/ts/process/scripts.ts`) tests the pattern first when the replacement starts with `@@` or
//! the flag carries `<...>` actions. On a match it runs `@@emo `, then `@@inject` (only for a message, `chatID` not -1),
//! then `@@move_top`/`@@move_bottom`, and otherwise replaces with the directive text itself; without a match only
//! `@@repeat_back` runs.

use crate::flags::parse_int;
use crate::replacement::{tokenize, UnitMatch};
use crate::script::{ascii, find, starts_with, PlanEntry};
use risunest_ir::budget::Budget;
use risunest_ir::stage::StageError;
use risunest_ir::Text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    Emotion,
    Inject,
    MoveTop,
    MoveBottom,
    /// The replacement is applied as written, `@@` included.
    Replace,
}

/// The branch a matching entry takes.
pub fn on_match(entry: &PlanEntry, chat_index: i64) -> Directive {
    let replacement = &entry.replacement;
    if starts_with(replacement, "@@emo ") { return Directive::Emotion; }
    if (starts_with(replacement, "@@inject") || entry.has_action("inject")) && chat_index != -1 { return Directive::Inject; }
    if starts_with(replacement, "@@move_top") || starts_with(replacement, "@@move_bottom")
        || entry.has_action("move_top") || entry.has_action("move_bottom") {
        return if starts_with(replacement, "@@move_top") || entry.has_action("move_top") { Directive::MoveTop } else { Directive::MoveBottom };
    }
    Directive::Replace
}

/// Whether an entry that did not match runs `@@repeat_back`.
pub fn repeats_back(entry: &PlanEntry, chat_index: i64) -> bool {
    (starts_with(&entry.replacement, "@@repeat_back") || entry.has_action("repeat_back")) && chat_index != -1
}

/// `script.out.substring(6).trim()`.
pub fn emotion_name(entry: &PlanEntry, budget: &Budget) -> Result<Text, StageError> {
    Ok(Text::from_units(entry.out.get(6..).unwrap_or(&[]), budget)?.trim(budget)?)
}

/// `outScript.split(' ', 2)[1]`.
pub fn repeat_back_position(entry: &PlanEntry) -> Option<&[u16]> { entry.replacement.split(|unit| *unit == 32).nth(1) }

fn replace_first(value: &[u16], search: &str) -> Vec<u16> {
    let search = ascii(search);
    match find(value, &search, 0) {
        Some(index) => [&value[..index], &value[index + search.len()..]].concat(),
        None => value.to_vec(),
    }
}

/// One `matchAll` result as `@@move_top`/`@@move_bottom` reads it: the matched text and each capture,
/// `None` for a group that did not participate; `groups` is `None` when the pattern has no named groups.
pub struct MoveMatch {
    pub values: Vec<Option<Vec<u16>>>,
    pub groups: Option<Vec<(Vec<u16>, Option<Vec<u16>>)>>,
}

const UNDEFINED: &str = "undefined";

/// The text `@@move_top`/`@@move_bottom` adds for one match:
/// ```js
/// outScript.replace('@@move_top ', '').replace('@@move_bottom ', '')
///     .replace(/(?<!\$)\$[0-9]+/g, ...)   // matched[index], or the reference when out of range
///     .replace(/\$\&/g, inData)           // a string replacement, so inData's own `$` patterns apply
///     .replace(/(?<!\$)\$<([^>]+)>/g, ...) // groups[parseInt(name)], so only a group named NaN or Infinity
/// ```
pub fn move_output(replacement: &[u16], found: &MoveMatch) -> Vec<u16> {
    let stripped = replace_first(&replace_first(replacement, "@@move_top "), "@@move_bottom ");
    // `$` followed by digits and not preceded by `$`.
    let mut numbered = Vec::with_capacity(stripped.len());
    let mut index = 0;
    while index < stripped.len() {
        let digits = stripped[index + 1..].iter().take_while(|unit| (48..=57).contains(*unit)).count();
        if stripped[index] == 36 && digits > 0 && (index == 0 || stripped[index - 1] != 36) {
            let reference = &stripped[index..index + 1 + digits];
            let position = parse_int(&reference[1..]);
            if position < found.values.len() as f64 {
                match &found.values[position as usize] {
                    Some(value) => numbered.extend_from_slice(value),
                    None => numbered.extend(UNDEFINED.encode_utf16()),
                }
            } else { numbered.extend_from_slice(reference); }
            index += 1 + digits;
            continue;
        }
        numbered.push(stripped[index]);
        index += 1;
    }
    // Every `$&` takes `inData` as a template with no captures and no named groups.
    let whole = found.values[0].clone().unwrap_or_default();
    let tokens = tokenize(&whole, 0, false);
    let mut matched = Vec::with_capacity(numbered.len());
    let mut start = 0;
    while let Some(position) = find(&numbered, &ascii("$&"), start) {
        matched.extend_from_slice(&numbered[start..position]);
        let groups = [Some(position..position + 2)];
        UnitMatch { input: &numbered, groups: &groups, names: &[] }.substitute(&tokens, &mut matched);
        start = position + 2;
    }
    matched.extend_from_slice(&numbered[start..]);
    let Some(groups) = &found.groups else { return matched };
    let mut output = Vec::with_capacity(matched.len());
    let mut index = 0;
    while index < matched.len() {
        if matched[index] == 36 && matched.get(index + 1) == Some(&60) && (index == 0 || matched[index - 1] != 36) {
            if let Some(length) = matched[index + 2..].iter().position(|unit| *unit == 62).filter(|length| *length > 0) {
                let end = index + 2 + length;
                let key = parse_int(&matched[index + 2..end]);
                let key = if key.is_nan() { Some("NaN") } else if key == f64::INFINITY { Some("Infinity") } else { None };
                let value = key.and_then(|key| groups.iter().find(|(name, _)| *name == ascii(key)))
                    .and_then(|(_, value)| value.as_ref()).filter(|value| !value.is_empty());
                match value { Some(value) => output.extend_from_slice(value), None => output.extend_from_slice(&matched[index..=end]) }
                index = end + 1;
                continue;
            }
        }
        output.push(matched[index]);
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }
    fn text(value: &[u16]) -> String { String::from_utf16(value).unwrap() }

    #[test]
    fn move_output_matches_the_three_replace_passes() {
        let found = MoveMatch { values: vec![Some(units("a$`b")), Some(units("$&")), None], groups: None };
        let output = move_output(&units("@@move_top [$0|$1|$2|$3|$$1|$01|$&]"), &found);
        assert_eq!(text(&output), "[a$`b|a[a$`b|b|undefined|$3|$$1|a[a$`b|$&|undefined|$3|$$1|b|a[a$`b|$&|undefined|$3|$$1|$&|b]");
        let named = MoveMatch { values: vec![Some(units("z"))], groups: Some(vec![(units("NaN"), Some(units("n")))]) };
        assert_eq!(text(&move_output(&units("$<foo>.$<1>.$<>.$$<foo>"), &named)), "n.$<1>.$<>.$$<foo>");
    }
}
