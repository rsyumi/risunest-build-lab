//! Probability, insertion order, position, depth and the token budget.
//!
//! After selection TS sorts the active entries by `priority` (descending), keeps each one that still fits the token
//! budget, sorts the rest by `order` (descending), applies the `@@inject_lore` entries to their targets and returns
//! the reversed array (`src/ts/process/lorebook.svelte.ts:701-757`). Both sorts are `Array.prototype.sort` with a
//! subtracting comparator; a comparator result of `NaN` counts as `0`, which is not a consistent order, so the sort
//! is a port of V8's TimSort (`third_party/v8/builtins/array-sort.tq`) rather than any stable sort.

use crate::matching::index_of;
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::stage::StageError;

pub use risunest_ir::ordering::sort_descending;

/// `text.replace(pattern, replacement)` with a string pattern: the first occurrence, with `$$`, `$&`, `` $` `` and
/// `$'` expanded in the replacement (`GetSubstitution` without captures). Every unit written is charged, so a
/// replacement full of `` $` `` or `$'` is bounded by the budget.
pub fn replace_first(text: &[u16], pattern: &[u16], replacement: &[u16], budget: &Budget) -> Result<OwnedVec<u16>, StageError> {
    budget.work(replacement.len())?;
    let mut output = OwnedVec::new(budget)?;
    let Some(position) = index_of(text, pattern, budget)? else {
        output.extend(text)?;
        return Ok(output);
    };
    let end = position + pattern.len();
    output.extend(&text[..position])?;
    let dollar = b'$' as u16;
    let mut at = 0;
    while at < replacement.len() {
        let unit = replacement[at];
        if unit == dollar && at + 1 < replacement.len() {
            let next = replacement[at + 1];
            let expansion: Option<&[u16]> = match char::from_u32(next as u32) {
                Some('$') => Some(&replacement[at..at + 1]),
                Some('&') => Some(&text[position..end]),
                Some('`') => Some(&text[..position]),
                Some('\'') => Some(&text[end..]),
                _ => None,
            };
            if let Some(expansion) = expansion {
                output.extend(expansion)?;
                at += 2;
                continue;
            }
        }
        output.push(unit)?;
        at += 1;
    }
    output.extend(&text[end..])?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
            max_depth: 32, max_work: 1 << 30 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn units(text: &str) -> Vec<u16> { text.encode_utf16().collect() }

    #[test]
    fn a_consistent_comparator_sorts_stably_descending() {
        let budget = budget();
        let keys: Vec<f64> = (0..300).map(|index| ((index * 7919) % 13) as f64).collect();
        let mut items: Vec<usize> = (0..keys.len()).collect();
        sort_descending(&mut items, &|index| keys[index], &budget).unwrap();
        let mut expected: Vec<usize> = (0..keys.len()).collect();
        expected.sort_by(|a, b| keys[*b].partial_cmp(&keys[*a]).unwrap());
        assert_eq!(items, expected);
    }

    #[test]
    fn string_replacement_expands_dollar_patterns_without_captures() {
        let budget = budget();
        let replace = |text: &str, pattern: &str, replacement: &str| {
            String::from_utf16(replace_first(&units(text), &units(pattern), &units(replacement), &budget).unwrap().as_slice()).unwrap()
        };
        assert_eq!(replace("a-b-b", "b", "[$&|$`|$'|$$|$1|$<x>|$]"), "a-[b|a-|-b|$|$1|$<x>|$]-b");
        assert_eq!(replace("abc", "", "X"), "Xabc");
        assert_eq!(replace("abc", "z", "X"), "abc");
    }

    #[test]
    fn a_replacement_that_repeats_the_text_is_charged_for_its_output() {
        let limited = |work| Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
            max_values: 1 << 16, max_depth: 32, max_work: work }, Arc::new(AtomicBool::new(false))).unwrap();
        // The 1,000 units before the match, then 400 copies of them: 401,000 units from 1,801 units of input.
        let text = units(&format!("{}x", "a".repeat(1000)));
        let replacement = units(&"$`".repeat(400));
        let output = replace_first(&text, &units("x"), &replacement, &limited(1 << 20)).unwrap();
        assert_eq!(output.as_slice().len(), 401_000);
        assert_eq!(replace_first(&text, &units("x"), &replacement, &limited(100_000)).unwrap_err(),
            StageError::Limit(std::borrow::Cow::Borrowed("work")));
    }
}
