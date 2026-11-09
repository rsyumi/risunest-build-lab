//! `new RegExp(source, flags)` and first-match search over UTF-16 input.
//!
//! The caller owns `lastIndex`, `g`, `y` and empty-match advancement. Matching is bounded by a [`MatchBudget`];
//! compilation is not, so callers bound the pattern size and read [`JsRegex::compiled_bytes`]. Non-Unicode ignore
//! case is compiled as the engine implements it, which differs from JavaScript for some non-ASCII characters, so
//! callers decide where it is admitted.
use regress::{backends, Flags, Regex};
use std::ops::Range;

pub use regress::{MatchAbort, MatchBudget, MatchLimits, MatchUsage};

type Groups = Vec<Option<Range<usize>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileFailure {
    /// The flags or the pattern are a `SyntaxError` in JavaScript, as the engine parser reports it.
    Syntax,
    /// JavaScript accepts the pattern, but this engine does not run it with JavaScript semantics.
    Unsupported(&'static str),
}

#[derive(Debug)]
pub struct JsRegex {
    regex: Regex,
    unicode: bool,
    ignore_case: bool,
    /// One entry per capture group; `None` for an unnamed group.
    names: Vec<Option<String>>,
    compiled_bytes: usize,
}

/// The parser's nesting limit, which JavaScript does not have.
const NESTING_ERROR: &str = "Regular expression is too deeply nested";

impl JsRegex {
    /// `flags` is the whole JavaScript flag string; `d`, `g` and `y` do not change the compiled pattern.
    pub fn compile(pattern: &[u16], flags: &str) -> Result<Self, CompileFailure> {
        let mut seen = String::new();
        for flag in flags.chars() {
            if !"dgimsuvy".contains(flag) || seen.contains(flag) { return Err(CompileFailure::Syntax); }
            seen.push(flag);
        }
        let unicode = flags.contains('u');
        if unicode && flags.contains('v') { return Err(CompileFailure::Syntax); }
        if flags.contains('v') { return Err(CompileFailure::Unsupported("unicode_sets")); }
        let parsed = Flags::from(flags);
        // Unicode mode combines valid pairs and keeps lone surrogates; legacy mode reads every unit alone.
        let points: Vec<u32> = if unicode {
            char::decode_utf16(pattern.iter().copied())
                .map(|point| point.map(u32::from).unwrap_or_else(|error| u32::from(error.unpaired_surrogate())))
                .collect()
        } else { pattern.iter().copied().map(u32::from).collect() };
        let mut tree = backends::try_parse(points.into_iter(), parsed).map_err(|error|
            if error.text == NESTING_ERROR { CompileFailure::Unsupported("regex_nesting_depth") } else { CompileFailure::Syntax })?;
        if !parsed.no_opt { backends::optimize(&mut tree); }
        let compiled = backends::emit(&tree);
        let names: Vec<Option<String>> = (0..compiled.groups as usize)
            .map(|index| compiled.group_names.get(index).filter(|name| !name.is_empty()).map(|name| name.to_string()))
            .collect();
        let regex = Regex::from(compiled);
        let compiled_bytes = regex.heap_size_estimate() + names.iter().flatten().map(String::len).sum::<usize>()
            + names.capacity() * std::mem::size_of::<Option<String>>();
        Ok(Self { regex, unicode, ignore_case: parsed.icase, names, compiled_bytes })
    }

    /// An estimate of the heap the compiled pattern holds.
    pub fn compiled_bytes(&self) -> usize { self.compiled_bytes }

    pub fn unicode(&self) -> bool { self.unicode }
    pub fn ignore_case(&self) -> bool { self.ignore_case }
    pub fn capture_count(&self) -> usize { self.names.len() }
    /// The name of capture group `index` (1-based).
    pub fn capture_name(&self, index: usize) -> Option<&str> { self.names.get(index.checked_sub(1)?)?.as_deref() }
    pub fn has_named_groups(&self) -> bool { self.names.iter().any(Option::is_some) }

    /// The first match that starts at or after `start`: the whole range, then one entry per capture group.
    pub fn find_from(&self, input: &[u16], start: usize, budget: &mut MatchBudget<'_>) -> Result<Option<Groups>, MatchAbort> {
        let found = if self.unicode { self.regex.find_first_from_utf16_controlled(input, start, budget)? }
            else { self.regex.find_first_from_ucs2_controlled(input, start, budget)? };
        Ok(found.map(groups))
    }

    /// The match that starts at `start`, as a sticky search tries it.
    pub fn find_at(&self, input: &[u16], start: usize, budget: &mut MatchBudget<'_>) -> Result<Option<Groups>, MatchAbort> {
        let found = if self.unicode { self.regex.find_at_utf16_controlled(input, start, budget)? }
            else { self.regex.find_at_ucs2_controlled(input, start, budget)? };
        Ok(found.map(groups))
    }
}

fn groups(found: regress::ControlledMatch<'_>) -> Groups {
    let mut groups = Vec::with_capacity(found.captures().len() + 1);
    groups.push(Some(found.range()));
    groups.extend(found.captures().iter().cloned());
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }
    fn budget() -> MatchBudget<'static> {
        MatchBudget::new(MatchLimits { work_units: 1_000_000, allocation_bytes: 1 << 20, lookaround_depth: 16 }, None, None)
    }

    #[test]
    fn reports_groups_names_and_flag_failures() {
        let regex = JsRegex::compile(&units("(?<x>a)(b)?"), "gu").unwrap();
        assert_eq!((regex.capture_count(), regex.capture_name(1), regex.capture_name(2)), (2, Some("x"), None));
        assert_eq!(regex.find_from(&units("zab"), 0, &mut budget()).unwrap(), Some(vec![Some(1..3), Some(1..2), Some(2..3)]));
        assert_eq!(regex.find_from(&units("za"), 2, &mut budget()).unwrap(), None);
        for (flags, failure) in [("gg", CompileFailure::Syntax), ("uv", CompileFailure::Syntax), ("x", CompileFailure::Syntax),
            ("v", CompileFailure::Unsupported("unicode_sets"))] {
            assert_eq!(JsRegex::compile(&units("a"), flags).unwrap_err(), failure);
        }
        assert_eq!(JsRegex::compile(&units("a{2,1}"), "g").unwrap_err(), CompileFailure::Syntax);
        // JavaScript nests groups without this limit.
        let nested = format!("{}a{}", "(?:".repeat(300), ")".repeat(300));
        assert_eq!(JsRegex::compile(&units(&nested), "g").unwrap_err(), CompileFailure::Unsupported("regex_nesting_depth"));
    }

    #[test]
    fn find_at_tries_only_the_start_and_size_grows_with_the_program() {
        let regex = JsRegex::compile(&units("a.*b"), "g").unwrap();
        assert_eq!(regex.find_at(&units("xab"), 0, &mut budget()).unwrap(), None);
        assert_eq!(regex.find_at(&units("xab"), 1, &mut budget()).unwrap(), Some(vec![Some(1..3)]));
        assert_eq!(regex.find_from(&units("xab"), 0, &mut budget()).unwrap(), Some(vec![Some(1..3)]));
        let small = JsRegex::compile(&units("a"), "g").unwrap().compiled_bytes();
        let words = (0..1000).map(|index| format!("w{index}")).collect::<Vec<_>>().join("|");
        let large = JsRegex::compile(&units(&words), "g").unwrap().compiled_bytes();
        assert!(small > 0 && large > 20 * words.len(), "{small} {large}");
    }

    #[test]
    fn unicode_mode_reads_pairs_and_legacy_mode_reads_units() {
        let pair = [0xd83d, 0xde00];
        assert_eq!(JsRegex::compile(&units("^."), "u").unwrap().find_from(&pair, 0, &mut budget()).unwrap(), Some(vec![Some(0..2)]));
        assert_eq!(JsRegex::compile(&units("^."), "").unwrap().find_from(&pair, 0, &mut budget()).unwrap(), Some(vec![Some(0..1)]));
    }
}
