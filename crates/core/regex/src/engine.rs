//! Engine selection and the `PatternEngine` adapter.
//!
//! Every pattern compiles on the regress fork, which parses JavaScript syntax and backtracks the way V8 does, and
//! runs on UTF-16 units. A pattern in the safe IR subset ([`crate::safe_ir`]) also compiles on the `regex` crate,
//! which runs `test` and `replace` whenever the input and the template are well-formed UTF-16.

use crate::replacement::{tokenize, Token, UnitMatch};
use crate::safe_ir::{self, SafeRegex};
use crate::script::Units16;
use regex_compat_experimental::js::{CompileFailure, JsRegex, MatchAbort, MatchBudget, MatchLimits};
use risunest_ir::budget::{Budget, Error};
use risunest_ir::profile::EngineClass;
use risunest_ir::stage::{Pattern, PatternEngine, PatternMatch, StageError};
use risunest_ir::Text;
use std::borrow::Cow;
use std::cell::Cell;
use std::ops::Range;

pub type Groups = Vec<Option<Range<usize>>>;

/// The `EngineClass` name of the regress fork.
pub const REGRESS: &str = "regress";

pub fn regress_engine() -> EngineClass { EngineClass::NativeRuntime(Cow::Borrowed(REGRESS)) }

/// Per-search limits of the general engine. JavaScript has none; a search past them fails its rule, which the runner
/// skips.
pub const DEFAULT_MATCH_LIMITS: MatchLimits = MatchLimits { work_units: 50_000_000, allocation_bytes: 64 << 20, lookaround_depth: 256 };

/// Search steps between charges to the run's [`Budget`], which also polls its cancellation.
const PULSE_WORK: u64 = 4096;

/// Size limits on a pattern before and after it compiles. Compilation takes no budget, so these bound its time,
/// stack and memory instead. JavaScript runs a pattern past them, so a pattern past them is declined. The defaults
/// assume a stage thread with at least an 8 MiB stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompileLimits {
    pub max_pattern_units: usize,
    /// [`alternation_depth`]: the fork compiles an alternation as a chain one level deeper per alternative, through
    /// recursive passes, so a long one overflows the native stack and takes time quadratic in its length. A release
    /// build overflows a 1 MiB stack between 3000 and 4000 alternatives and a 2 MiB one between 8000 and 12000; a
    /// debug build overflows a 1 MiB stack at 1000 and a 2 MiB one between 2000 and 3000.
    pub max_alternation_depth: usize,
    pub max_compiled_bytes: usize,
}

pub const DEFAULT_COMPILE_LIMITS: CompileLimits =
    CompileLimits { max_pattern_units: 1 << 18, max_alternation_depth: 512, max_compiled_bytes: 32 << 20 };

/// The most `|` separators that enclose or precede one point of the pattern, counting every open group, outside
/// classes and escapes. This bounds the depth of the fork's alternation chains.
pub fn alternation_depth(pattern: &[u16]) -> usize {
    let mut levels = vec![0usize];
    let (mut running, mut deepest, mut index, mut class) = (0usize, 0usize, 0, false);
    while index < pattern.len() {
        match pattern[index] {
            92 => index += 1,
            91 if !class => class = true,
            93 if class => class = false,
            40 if !class => levels.push(0),
            41 if !class && levels.len() > 1 => running -= levels.pop().expect("level"),
            124 if !class => {
                *levels.last_mut().expect("level") += 1;
                running += 1;
                deepest = deepest.max(running);
            }
            _ => {}
        }
        index += 1;
    }
    deepest
}

/// Why `new RegExp(pattern, flags)` produced no pattern here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileError {
    /// The `SyntaxError` the RegExp constructor throws.
    Syntax,
    /// JavaScript runs the pattern, but this engine does not run it with JavaScript semantics or within
    /// [`CompileLimits`].
    Unsupported(&'static str),
}

impl From<CompileFailure> for CompileError {
    fn from(failure: CompileFailure) -> Self {
        match failure {
            CompileFailure::Syntax => Self::Syntax,
            CompileFailure::Unsupported(name) => Self::Unsupported(name),
        }
    }
}

fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }

fn abort(error: MatchAbort) -> StageError {
    match error {
        MatchAbort::Cancelled => StageError::Cancelled,
        MatchAbort::DeadlineExceeded => StageError::Limit(Cow::Borrowed("regex-deadline")),
        MatchAbort::WorkLimit => StageError::Limit(Cow::Borrowed("regex-work")),
        MatchAbort::AllocationBudget | MatchAbort::AllocationFailure => StageError::Limit(Cow::Borrowed("regex-allocation")),
        MatchAbort::LookaroundDepth => StageError::Limit(Cow::Borrowed("regex-lookaround-depth")),
    }
}

/// `AdvanceStringIndex`.
fn advance(input: &[u16], index: usize, unicode: bool) -> usize {
    if !unicode || index + 1 >= input.len() { return index + 1; }
    if (0xd800..=0xdbff).contains(&input[index]) && (0xdc00..=0xdfff).contains(&input[index + 1]) { index + 2 } else { index + 1 }
}

fn well_formed(units: &[u16]) -> bool { char::decode_utf16(units.iter().copied()).all(|point| point.is_ok()) }

/// `new RegExp(pattern, flags)` with both engines.
#[derive(Debug)]
pub struct CompiledPattern {
    regex: JsRegex,
    safe: Option<SafeRegex>,
    global: bool,
    sticky: bool,
    unicode: bool,
    names: Vec<(Vec<u16>, usize)>,
}

impl CompiledPattern {
    /// `Syntax` is the `SyntaxError` the RegExp constructor throws.
    pub fn compile(pattern: &[u16], flags: &str) -> Result<Self, CompileFailure> {
        let regex = JsRegex::compile(pattern, flags)?;
        let names = (1..=regex.capture_count())
            .filter_map(|index| regex.capture_name(index).map(|name| (name.encode_utf16().collect(), index))).collect();
        Ok(Self {
            safe: safe_ir::compile(pattern, flags),
            global: flags.contains('g'),
            sticky: flags.contains('y'),
            unicode: regex.unicode(),
            names,
            regex,
        })
    }

    /// [`Self::compile`] within `limits`: the pattern length is checked first and the compiled size after.
    pub fn compile_bounded(pattern: &[u16], flags: &str, limits: CompileLimits) -> Result<Self, CompileError> {
        if pattern.len() > limits.max_pattern_units { return Err(CompileError::Unsupported("regex-pattern-units")); }
        if alternation_depth(pattern) > limits.max_alternation_depth { return Err(CompileError::Unsupported("regex-alternation-depth")); }
        let compiled = Self::compile(pattern, flags)?;
        if compiled.compiled_bytes() > limits.max_compiled_bytes { return Err(CompileError::Unsupported("regex-compiled-bytes")); }
        Ok(compiled)
    }

    /// An estimate of the general engine's compiled program and the group names; the safe engine stays within the
    /// `regex` crate's default size limits.
    pub fn compiled_bytes(&self) -> usize {
        self.regex.compiled_bytes() + self.names.iter().map(|(name, _)| 2 * name.len() + std::mem::size_of::<(Vec<u16>, usize)>()).sum::<usize>()
    }

    pub fn global(&self) -> bool { self.global }
    pub fn captures(&self) -> usize { self.regex.capture_count() }
    /// `(name, group)` for every named group in pattern order.
    pub fn names(&self) -> &[(Vec<u16>, usize)] { &self.names }
    pub fn has_safe_plan(&self) -> bool { self.safe.is_some() }

    /// The safe engine and the UTF-8 view of `input`, when it can run on them.
    pub fn safe_for<'a>(&'a self, input: &'a Text) -> Option<(&'a SafeRegex, &'a str)> { Some((self.safe.as_ref()?, input.as_unicode()?)) }

    /// All UTF-16 inputs are supported by the general engine.
    pub fn admit(&self, _input: &[u16]) -> Result<(), StageError> { Ok(()) }

    /// `RegExpBuiltinExec` with `lastIndex` = `last_index`, which is ignored unless the pattern is global or sticky.
    pub fn exec(&self, input: &[u16], last_index: usize, limits: MatchLimits, budget: &Budget) -> Result<Option<Groups>, StageError> {
        let start = if self.global || self.sticky { last_index } else { 0 };
        if start > input.len() { return Ok(None); }
        budget.check()?;
        // The search charges the run's budget as it goes, so cancellation and the run's work limit stop it within
        // one pulse rather than at the per-search limit.
        let charged = Cell::new(0u64);
        let failure: Cell<Option<Error>> = Cell::new(None);
        let pulse = |work: u64| {
            let result = if work - charged.get() >= PULSE_WORK {
                let delta = work - charged.replace(work);
                budget.work(usize::try_from(delta).unwrap_or(usize::MAX))
            } else { budget.check() };
            result.map_err(|error| failure.set(Some(error))).is_ok()
        };
        let mut control = MatchBudget::new(limits, None, None).with_pulse(&pulse);
        // A sticky search tries `lastIndex` only.
        let found = if self.sticky { self.regex.find_at(input, start, &mut control) } else { self.regex.find_from(input, start, &mut control) };
        let found = match (found, failure.take()) {
            (Ok(found), _) => found,
            (Err(MatchAbort::Cancelled), Some(error)) => return Err(error.into()),
            (Err(error), _) => return Err(abort(error)),
        };
        let rest = control.usage().work_units - charged.get();
        budget.work(usize::try_from(rest).unwrap_or(usize::MAX).max(1))?;
        Ok(found.filter(|groups| !self.sticky || groups[0].as_ref().is_some_and(|whole| whole.start == start)))
    }

    /// The results `replace`, `match` and `matchAll` collect: one search unless the pattern is global, then every
    /// match from index 0, stepping over empty matches by code point under `u`.
    pub fn each_match(&self, input: &[u16], limits: MatchLimits, budget: &Budget,
        mut visit: impl FnMut(&Groups) -> Result<(), StageError>) -> Result<(), StageError> {
        let mut last_index = 0;
        while let Some(groups) = self.exec(input, last_index, limits, budget)? {
            visit(&groups)?;
            if !self.global { break; }
            let whole = groups[0].clone().expect("whole match");
            last_index = if whole.is_empty() { advance(input, whole.end, self.unicode) } else { whole.end };
        }
        Ok(())
    }

    /// `regex.test(input)` on a fresh RegExp.
    pub fn test(&self, input: &[u16], limits: MatchLimits, budget: &Budget) -> Result<bool, StageError> {
        Ok(self.exec(input, 0, limits, budget)?.is_some())
    }

    /// `input.replace(regex, template)` for a string template.
    pub fn replace(&self, input: &[u16], template: &[u16], limits: MatchLimits, budget: &Budget) -> Result<Vec<u16>, StageError> {
        Ok(self.replace_units(input, template, limits, budget)?.into_vec())
    }

    /// [`Self::replace`] into a buffer whose growth is charged to owned bytes before it is allocated.
    pub(crate) fn replace_units(&self, input: &[u16], template: &[u16], limits: MatchLimits, budget: &Budget) -> Result<Units16, StageError> {
        let tokens = tokenize(template, self.captures(), !self.names.is_empty());
        let max_units = budget.limits().max_text_units;
        let mut output = Units16::with_capacity(input.len(), budget)?;
        let mut next = 0;
        self.each_match(input, limits, budget, |groups| {
            let whole = groups[0].clone().expect("whole match");
            let found = UnitMatch { input, groups, names: &self.names };
            let added = whole.start - next + found.substituted_len(&tokens);
            budget.work(added)?;
            if output.data.len() + added > max_units { return Err(StageError::Limit(Cow::Borrowed("text-units"))); }
            output.reserve(added, budget)?;
            output.data.extend_from_slice(&input[next..whole.start]);
            found.substitute(&tokens, &mut output.data);
            next = whole.end;
            Ok(())
        })?;
        output.reserve(input.len() - next, budget)?;
        output.data.extend_from_slice(&input[next..]);
        Ok(output)
    }

    /// The safe engine's `replace`, when the template is well-formed.
    pub fn safe_replace(safe: &SafeRegex, input: &str, template: &[u16], budget: &Budget) -> Result<Option<String>, StageError> {
        if !well_formed(template) { return Ok(None); }
        budget.work(input.len() + template.len())?;
        let tokens: Vec<Token> = tokenize(template, safe.captures(), false);
        let output = safe.replace(input, &tokens);
        budget.work(output.len())?;
        Ok(Some(output))
    }
}

/// [`PatternEngine`] over [`CompiledPattern`].
#[derive(Debug)]
pub struct JsPatternEngine { limits: MatchLimits }

impl JsPatternEngine {
    pub fn new(limits: MatchLimits) -> Self { Self { limits } }
}

impl Default for JsPatternEngine {
    fn default() -> Self { Self::new(DEFAULT_MATCH_LIMITS) }
}

pub struct JsPattern {
    compiled: CompiledPattern,
    names: Vec<(Text, usize)>,
    limits: MatchLimits,
}

impl Pattern for JsPattern {
    fn exec(&self, input: &Text, last_index: usize, budget: &Budget) -> Result<Option<PatternMatch>, StageError> {
        budget.work(input.len_units())?;
        let _charge = budget.reserve_owned(input.len_units().saturating_mul(2))?;
        let units: Vec<u16> = input.units().collect();
        self.compiled.admit(&units)?;
        Ok(self.compiled.exec(&units, last_index, self.limits, budget)?.map(|groups| PatternMatch { groups }))
    }

    fn group_names(&self) -> &[(Text, usize)] { &self.names }
}

impl PatternEngine for JsPatternEngine {
    fn compile(&mut self, source: &Text, flags: &Text, budget: &Budget) -> Result<Box<dyn Pattern + Send>, StageError> {
        budget.work(source.len_units() + flags.len_units())?;
        let flags: String = char::decode_utf16(flags.units()).map(|flag| flag.unwrap_or(char::REPLACEMENT_CHARACTER)).collect();
        let pattern: Vec<u16> = source.units().collect();
        let compiled = match CompiledPattern::compile_bounded(&pattern, &flags, DEFAULT_COMPILE_LIMITS) {
            Ok(compiled) => compiled,
            Err(CompileError::Syntax) => return Err(StageError::Invalid(Cow::Borrowed("regex-syntax"))),
            Err(CompileError::Unsupported(name)) => return Err(unsupported(name)),
        };
        let names = compiled.names.iter()
            .map(|(name, group)| Ok((Text::from_units(name, budget)?, *group))).collect::<Result<_, StageError>>()?;
        Ok(Box::new(JsPattern { compiled, names, limits: self.limits }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }
    pub(crate) fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 26, max_json_bytes: 1 << 24, max_text_units: 1 << 22, max_values: 1 << 20,
            max_depth: 64, max_work: 1 << 40 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn replace(pattern: &str, flags: &str, input: &[u16], template: &str) -> Vec<u16> {
        let compiled = CompiledPattern::compile(&units(pattern), flags).unwrap();
        compiled.replace(input, &units(template), DEFAULT_MATCH_LIMITS, &budget()).unwrap()
    }
    fn replace_str(pattern: &str, flags: &str, input: &str, template: &str) -> String {
        String::from_utf16(&replace(pattern, flags, &units(input), template)).unwrap()
    }

    #[test]
    fn replace_follows_global_sticky_and_empty_match_rules() {
        assert_eq!(replace_str("a", "g", "banana", "o"), "bonono");
        assert_eq!(replace_str("a", "", "banana", "o"), "bonana");
        assert_eq!(replace_str("a", "gy", "aab", "x"), "xxb");
        assert_eq!(replace_str("a", "y", "ba", "x"), "ba");
        assert_eq!(replace_str("", "g", "ab", "-"), "-a-b-");
        let pair = [0xd83d, 0xde00, 0x61];
        assert_eq!(replace("", "gu", &pair, "-"), [45, 0xd83d, 0xde00, 45, 0x61, 45]);
        assert_eq!(replace("", "g", &pair, "-"), [45, 0xd83d, 45, 0xde00, 45, 0x61, 45]);
        assert_eq!(replace_str("(?<w>b)(c)?", "g", "abc ab", "[$<w>|$2|$&|$`]"), "a[b|c|bc|a] a[b||b|abc a]");
    }

    #[test]
    fn legacy_ignore_case_uses_full_uppercase_canonicalization() {
        for (pattern, input, expected) in [
            ("[a-z]+", "sSſ kKK iIİı", "_ſ _K _İı"),
            ("[\\u017f\\u212a\\u0131]", "sSſ kKK iIİı", "sS_ kK_ iIİ_"),
            ("[À-ž]", "iIİıéÉ", "iI____"),
            ("ᾀ", "ᾀᾈ", "_ᾈ"),
            ("[ᾀ]", "ᾀᾈ", "_ᾈ"),
            ("(ᾀ)\\1", "ᾀᾈ ᾀᾀ", "ᾀᾈ _"),
            ("(s)\\1", "sſ sS", "sſ _"),
            ("(?i:k)", "kKK", "__K"),
            ("[^\\w]", "sſK-", "s___"),
            ("[\\W]", "sſK-", "s___"),
            ("[^\\W]", "sſK-", "_ſK-"),
            ("[\\W-z]", "sſK-z", "s____"),
        ] {
            assert_eq!(replace_str(pattern, "gi", input, "_"), expected, "{pattern}");
            let compiled = CompiledPattern::compile(&units(pattern), "gi").unwrap();
            assert!(compiled.admit(&units(input)).is_ok());
        }
        assert_eq!(replace_str("[\\W]", "giu", "sſK-", "_"), "sſK_");
        assert_eq!(replace_str("[^\\W]", "giu", "sſK-", "_"), "___-");
        assert_eq!(replace_str("(?i:[\\W])", "gu", "sſK-", "_"), "sſK_");
        assert_eq!(replace_str("[ᾀ]", "giu", "ᾀᾈ", "_"), "__");
    }

    #[test]
    fn pattern_engine_follows_exec_and_reports_names() {
        let budget = budget();
        let mut engine = JsPatternEngine::default();
        let text = |value: &str| Text::from_str(value, &budget).unwrap();
        assert!(matches!(engine.compile(&text("("), &text("g"), &budget), Err(StageError::Invalid(_))));
        assert!(matches!(engine.compile(&text("a"), &text("v"), &budget), Err(StageError::Unsupported(_))));
        let global = engine.compile(&text("(?<x>a)(b)?"), &text("g"), &budget).unwrap();
        let names: Vec<_> = global.group_names().iter().map(|(name, group)| (name.units().collect::<Vec<_>>(), *group)).collect();
        assert_eq!(names, [(units("x"), 1)]);
        let input = text("aab");
        assert_eq!(global.exec(&input, 1, &budget).unwrap().unwrap().groups, [Some(1..3), Some(1..2), Some(2..3)]);
        assert_eq!(global.exec(&input, 4, &budget).unwrap(), None);
        let plain = engine.compile(&text("a"), &text(""), &budget).unwrap();
        assert_eq!(plain.exec(&input, 1, &budget).unwrap().unwrap().groups, [Some(0..1)]);
        let sticky = engine.compile(&text("b"), &text("y"), &budget).unwrap();
        assert_eq!(sticky.exec(&input, 1, &budget).unwrap(), None);
        assert_eq!(sticky.exec(&input, 2, &budget).unwrap().unwrap().groups, [Some(2..3)]);
    }

    #[test]
    fn safe_plan_covers_the_subset_and_agrees_with_the_general_engine() {
        let budget = budget();
        for (pattern, flags, input, template) in [("ab|c", "g", "abcab", "<$&>"), ("(a)(b|d)c", "g", "adcabc", "$2$1"),
            ("[a-c]{1,3}x", "gu", "abx cx", "$&$&"), ("(?:ab){2}", "g", "ababab", "-"), ("\\.", "u", "a.b.", "$'")] {
            let compiled = CompiledPattern::compile(&units(pattern), flags).unwrap();
            assert!(compiled.has_safe_plan(), "{pattern}");
            let text = Text::from_str(input, &budget).unwrap();
            let (safe, view) = compiled.safe_for(&text).unwrap();
            let fast = CompiledPattern::safe_replace(safe, view, &units(template), &budget).unwrap().unwrap();
            assert_eq!(fast, String::from_utf16(&compiled.replace(&units(input), &units(template), DEFAULT_MATCH_LIMITS, &budget).unwrap()).unwrap());
        }
        for (pattern, flags) in [("a*", "g"), ("a|", "g"), ("(a)+", "g"), ("(a)(b)?c", "g"), ("(?<n>a)", "g"), ("a", "gi"), ("a", "gy"), ("é", "g"),
            ("[^a]", "g"), ("a{2,}", "g"), ("a?", "g"), ("\\d", "g"), ("\\w", "g"), ("a.b", "g")] {
            assert!(!CompiledPattern::compile(&units(pattern), flags).unwrap().has_safe_plan(), "{pattern}");
        }
    }

    #[test]
    fn sticky_search_tries_last_index_only() {
        let budget = budget();
        let limits = MatchLimits { work_units: 1_000_000, ..DEFAULT_MATCH_LIMITS };
        let sticky = CompiledPattern::compile(&units("a.*b"), "y").unwrap();
        // Trying every start would cost about n * n / 2 steps; the one attempt costs a few steps per unit.
        for size in [8192usize, 65536] { assert_eq!(sticky.exec(&vec![97; size], 0, limits, &budget).unwrap(), None, "{size}"); }
        assert_eq!(sticky.exec(&units("xab"), 1, limits, &budget).unwrap(), Some(vec![Some(1..3)]));
        assert_eq!(sticky.exec(&units("xab"), 0, limits, &budget).unwrap(), None);
    }

    /// xorshift64.
    struct Rng(u64);
    impl Rng {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }
        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str { items[self.below(items.len())] }
    }

    /// A pattern from the grammar the safe IR admits, plus nullable alternatives and quantified groups it refuses.
    fn disjunction(rng: &mut Rng, depth: usize, quantified: bool, output: &mut String) {
        for index in 0..1 + rng.below(if depth == 0 { 3 } else { 2 }) {
            if index > 0 { output.push('|'); }
            for _ in 0..1 + rng.below(3) { term(rng, depth, quantified, output); }
        }
    }

    fn term(rng: &mut Rng, depth: usize, quantified: bool, output: &mut String) {
        let quantifier = if quantified { None } else { [None, None, Some("?"), Some("{2}"), Some("{0,2}"), Some("{1,3}")][rng.below(6)] };
        let inner = quantified || quantifier.is_some();
        match rng.below(if depth >= 2 { 3 } else { 5 }) {
            0 => output.push_str(rng.pick(&["a", "b", "c", "x", "-", ",", "\\.", "\\(", "\\|", "\\*", "\\$"])),
            1 => output.push_str(rng.pick(&["a", "b", "ab", "ba"])),
            2 => output.push_str(rng.pick(&["[ab]", "[a-c]", "[\\.x]", "[b-cx]", "[\\-a]"])),
            choice => {
                output.push_str(if choice == 3 || inner { "(?:" } else { "(" });
                disjunction(rng, depth + 1, inner, output);
                output.push(')');
            }
        }
        if let Some(quantifier) = quantifier { output.push_str(quantifier); }
    }

    #[test]
    fn safe_plan_agrees_with_the_general_engine_on_random_patterns() {
        let budget = budget();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let (mut admitted, mut compared, mut tries) = (0, 0, 0);
        let mut mismatches = Vec::new();
        while admitted < 3000 && tries < 40_000 {
            tries += 1;
            let mut pattern = String::new();
            disjunction(&mut rng, 0, false, &mut pattern);
            let flags = rng.pick(&["g", "u", "gu"]);
            let Ok(compiled) = CompiledPattern::compile(&units(&pattern), flags) else { continue };
            if !compiled.has_safe_plan() { continue; }
            admitted += 1;
            let template = units(rng.pick(&["<$&>", "[$1|$2]", "$`|$'", "$$-$&$1"]));
            for _ in 0..4 {
                let input: String = (0..rng.below(12)).map(|_| rng.pick(&["a", "b", "c", "x", ".", "-", "(", "|", "ab"])).collect();
                let text = Text::from_str(&input, &budget).unwrap();
                let (safe, view) = compiled.safe_for(&text).unwrap();
                let general = compiled.replace(&units(&input), &template, DEFAULT_MATCH_LIMITS, &budget).unwrap();
                let fast = CompiledPattern::safe_replace(safe, view, &template, &budget).unwrap().unwrap();
                let matched = (safe.is_match(view), compiled.test(&units(&input), DEFAULT_MATCH_LIMITS, &budget).unwrap());
                if fast != String::from_utf16(&general).unwrap() || matched.0 != matched.1 {
                    mismatches.push(format!("/{pattern}/{flags} on {input:?}: {fast:?} vs {:?}", String::from_utf16_lossy(&general)));
                }
                compared += 1;
            }
        }
        assert!(mismatches.is_empty(), "{} mismatches of {compared}:\n{}", mismatches.len(), mismatches[..mismatches.len().min(20)].join("\n"));
        assert!(admitted == 3000 && compared == 12_000, "{admitted} admitted of {tries}");
    }

    fn best<T>(runs: usize, mut work: impl FnMut() -> T) -> f64 {
        (0..runs).map(|_| {
            let start = std::time::Instant::now();
            std::hint::black_box(work());
            start.elapsed().as_secs_f64() * 1e6
        }).fold(f64::INFINITY, f64::min)
    }

    /// Times the UTF-16 copies around a general-engine search against the searches themselves, in microseconds.
    /// Run with `cargo test --release -p risunest-regex utf16_boundary_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn utf16_boundary_cost() {
        let budget = Budget::new(Limits { max_owned_bytes: 1 << 31, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 20,
            max_depth: 64, max_work: 1 << 50 }, Arc::new(AtomicBool::new(false))).unwrap();
        let limits = MatchLimits { work_units: 1 << 40, allocation_bytes: 1 << 34, lookaround_depth: 256 };
        let compile = |pattern: &str, flags: &str| CompiledPattern::compile(&units(pattern), flags).unwrap();
        let (literal, class, word) = (compile("zqx", "g"), compile("\\[([^\\]]*)\\]", "g"), compile("fox", "g"));
        let template = units("cat");
        // `unpulsed` is the literal miss without the budget pulse; `std` is `String::from_utf16`, for reference.
        println!("units utf8-bytes | collect from_units std | regress: literal-miss unpulsed class-miss replace | safe: literal-miss replace");
        for (target, runs) in [(1usize << 10, 200), (1 << 16, 50), (1 << 20, 10), (1 << 23, 3)] {
            let mut source = String::new();
            let mut count = 0;
            for character in "The quick fox 안녕하세요 jumps 😀 over, ".chars().cycle() {
                if count + character.len_utf16() > target { break; }
                count += character.len_utf16();
                source.push(character);
            }
            let text = Text::from_str(&source, &budget).unwrap();
            let data: Vec<u16> = text.units().collect();
            let (safe, view) = word.safe_for(&text).unwrap();
            let literal_safe = literal.safe_for(&text).unwrap().0;
            println!("{count} {} | {:.1} {:.1} {:.1} | {:.1} {:.1} {:.1} {:.1} | {:.1} {:.1}", source.len(),
                best(runs, || text.units().collect::<Vec<u16>>()),
                best(runs, || Text::from_units(&data, &budget).unwrap()),
                best(runs, || String::from_utf16(&data).unwrap()),
                best(runs, || literal.test(&data, limits, &budget).unwrap()),
                best(runs, || literal.regex.find_from(&data, 0, &mut MatchBudget::new(limits, None, None)).unwrap()),
                best(runs, || class.test(&data, limits, &budget).unwrap()),
                best(runs, || word.replace(&data, &template, limits, &budget).unwrap()),
                best(runs, || literal_safe.is_match(view)),
                best(runs, || CompiledPattern::safe_replace(safe, view, &template, &budget).unwrap()));
        }
    }
}
