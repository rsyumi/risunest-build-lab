//! Standalone UTF-16 replacement experiment. Not connected to product routing.
//!
//! Cancellation and deadlines are cooperative between engine calls. Regress
//! compilation and individual searches cannot be interrupted by this API.
pub mod js;

use regress::{Flags, Match, Regex};
use std::{sync::atomic::{AtomicBool, Ordering}, time::Instant};

#[derive(Clone, Debug)]
pub struct RuleSpec {
    pub pattern: Vec<u16>,
    pub flags: String,
    pub replacement: Vec<u16>,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub rules: usize,
    pub pattern_units: usize,
    pub source_units: usize,
    pub input_units: usize,
    pub output_units: usize,
    pub matches: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { rules: 128, pattern_units: 4096, source_units: 65536,
            input_units: 1_048_576, output_units: 4_194_304, matches: 100_000 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind { Unsupported, EngineRejected, ResourceLimit, Cancelled, DeadlineExceeded }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub code: &'static str,
    pub rule_index: Option<usize>,
}

fn failure(kind: FailureKind, code: &'static str) -> Failure {
    Failure { kind, code, rule_index: None }
}

#[derive(Default)]
pub struct ExecutionControl<'a> {
    pub cancelled: Option<&'a AtomicBool>,
    pub deadline: Option<Instant>,
}

impl ExecutionControl<'_> {
    fn check(&self) -> Result<(), Failure> {
        if self.cancelled.is_some_and(|value| value.load(Ordering::Relaxed)) {
            return Err(failure(FailureKind::Cancelled, "cancelled"));
        }
        if self.deadline.is_some_and(|value| Instant::now() >= value) {
            return Err(failure(FailureKind::DeadlineExceeded, "deadline"));
        }
        Ok(())
    }
}

struct CompiledRule {
    regex: Regex,
    global: bool,
    sticky: bool,
    unicode: bool,
    replacement: Vec<u16>,
}

pub struct CompiledPlan {
    rules: Vec<CompiledRule>,
    limits: Limits,
    source_units: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ExecutionResult {
    pub output: Vec<u16>,
    pub matches: usize,
}

fn parse_flags(flags: &str) -> Result<(Flags, bool, bool), Failure> {
    let mut seen = String::new();
    for flag in flags.chars() {
        if !"dgimsuvy".contains(flag) || seen.contains(flag) {
            return Err(failure(FailureKind::EngineRejected, "invalid_flags"));
        }
        seen.push(flag);
    }
    if flags.contains('u') && flags.contains('v') {
        return Err(failure(FailureKind::EngineRejected, "invalid_flags"));
    }
    if flags.contains('v') {
        return Err(failure(FailureKind::Unsupported, "unicode_sets"));
    }
    Ok((Flags::from(flags), flags.contains('g'), flags.contains('y')))
}

impl CompiledPlan {
    pub fn compile(specs: &[RuleSpec], limits: Limits, control: &ExecutionControl<'_>) -> Result<Self, Failure> {
        control.check()?;
        if specs.len() > limits.rules {
            return Err(failure(FailureKind::ResourceLimit, "rules"));
        }
        let mut source_units = 0usize;
        for (index, spec) in specs.iter().enumerate() {
            if spec.pattern.len() > limits.pattern_units {
                return Err(Failure { rule_index: Some(index), ..failure(FailureKind::ResourceLimit, "pattern_units") });
            }
            source_units = source_units.saturating_add(spec.pattern.len())
                .saturating_add(spec.replacement.len()).saturating_add(spec.flags.len());
            if source_units > limits.source_units {
                return Err(failure(FailureKind::ResourceLimit, "source_units"));
            }
        }
        let mut rules = Vec::with_capacity(specs.len());
        for (index, spec) in specs.iter().enumerate() {
            let compile = || -> Result<CompiledRule, Failure> {
                control.check()?;
                let (flags, global, sticky) = parse_flags(&spec.flags)?;
                // Unicode mode combines valid pairs and keeps lone surrogates.
                // Legacy mode presents every UTF-16 unit as a distinct value.
                let points: Vec<u32> = if flags.unicode {
                    char::decode_utf16(spec.pattern.iter().copied())
                        .map(|point| point.map(u32::from).unwrap_or_else(|error| u32::from(error.unpaired_surrogate())))
                        .collect()
                } else { spec.pattern.iter().copied().map(u32::from).collect() };
                let regex = Regex::from_unicode(points.into_iter(), flags)
                    .map_err(|_| failure(FailureKind::EngineRejected, "pattern"))?;
                control.check()?;
                Ok(CompiledRule { regex, global, sticky, unicode: flags.unicode, replacement: spec.replacement.clone() })
            };
            rules.push(compile().map_err(|error| Failure { rule_index: Some(index), ..error })?);
        }
        Ok(Self { rules, limits, source_units })
    }

    pub fn rule_count(&self) -> usize { self.rules.len() }
    pub fn source_units(&self) -> usize { self.source_units }

    pub fn execute(&self, input: &[u16], control: &ExecutionControl<'_>) -> Result<ExecutionResult, Failure> {
        control.check()?;
        if input.len() > self.limits.input_units {
            return Err(failure(FailureKind::ResourceLimit, "input_units"));
        }
        if input.len() > self.limits.output_units {
            return Err(failure(FailureKind::ResourceLimit, "output_units"));
        }
        let mut output = input.to_vec();
        let mut matches = 0;
        for (index, rule) in self.rules.iter().enumerate() {
            output = apply(rule, &output, &mut matches, self.limits, control)
                .map_err(|error| Failure { rule_index: Some(index), ..error })?;
        }
        control.check()?;
        Ok(ExecutionResult { output, matches })
    }
}

fn push(out: &mut Vec<u16>, units: &[u16], limit: usize) -> Result<(), Failure> {
    if units.len() > limit.saturating_sub(out.len()) {
        return Err(failure(FailureKind::ResourceLimit, "output_units"));
    }
    out.extend_from_slice(units);
    Ok(())
}

fn substitute(out: &mut Vec<u16>, template: &[u16], input: &[u16], found: &Match, limit: usize) -> Result<(), Failure> {
    let mut index = 0;
    while index < template.len() {
        if template[index] != 36 || index + 1 == template.len() {
            push(out, &template[index..index + 1], limit)?;
            index += 1;
            continue;
        }
        match template[index + 1] {
            36 => { push(out, &[36], limit)?; index += 2; }
            38 => { push(out, &input[found.range()], limit)?; index += 2; }
            96 => { push(out, &input[..found.start()], limit)?; index += 2; }
            39 => { push(out, &input[found.end()..], limit)?; index += 2; }
            digit @ 48..=57 => {
                let mut number = (digit - 48) as usize;
                let mut consumed = 2;
                if let Some(next @ 48..=57) = template.get(index + 2) {
                    let both = number * 10 + usize::from(next - 48);
                    if both > 0 && both <= found.captures.len() { number = both; consumed = 3; }
                }
                if number > 0 && number <= found.captures.len() {
                    if let Some(range) = found.group(number) { push(out, &input[range], limit)?; }
                    index += consumed;
                } else { push(out, &[36], limit)?; index += 1; }
            }
            60 if found.named_groups().next().is_some() => {
                if let Some(end) = template[index + 2..].iter().position(|&unit| unit == 62) {
                    if let Ok(name) = String::from_utf16(&template[index + 2..index + 2 + end]) {
                        if let Some((_, Some(range))) = found.named_groups().find(|(key, _)| *key == name) {
                            push(out, &input[range], limit)?;
                        }
                    }
                    index += end + 3;
                } else { push(out, &[36], limit)?; index += 1; }
            }
            _ => { push(out, &[36], limit)?; index += 1; }
        }
    }
    Ok(())
}

fn advance(input: &[u16], index: usize, unicode: bool) -> usize {
    if unicode && index + 1 < input.len() && (0xd800..=0xdbff).contains(&input[index])
        && (0xdc00..=0xdfff).contains(&input[index + 1]) { index + 2 } else { index + 1 }
}

fn apply(rule: &CompiledRule, input: &[u16], matches: &mut usize, limits: Limits, control: &ExecutionControl<'_>) -> Result<Vec<u16>, Failure> {
    let mut output = Vec::new();
    let mut end = 0;
    let mut search = 0;
    while search <= input.len() {
        control.check()?;
        // Take only one result so the adapter owns ECMAScript empty advancement.
        let found = if rule.unicode { rule.regex.find_from_utf16(input, search).next() }
            else { rule.regex.find_from_ucs2(input, search).next() };
        control.check()?;
        let Some(found) = found else { break };
        if rule.sticky && found.start() != search { break; }
        if *matches >= limits.matches {
            return Err(failure(FailureKind::ResourceLimit, "matches"));
        }
        *matches += 1;
        push(&mut output, &input[end..found.start()], limits.output_units)?;
        substitute(&mut output, &rule.replacement, input, &found, limits.output_units)?;
        end = found.end();
        if !rule.global { break; }
        search = if found.start() == found.end() { advance(input, end, rule.unicode) } else { end };
    }
    push(&mut output, &input[end..], limits.output_units)?;
    control.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rule(pattern: &str, flags: &str, replacement: &str) -> RuleSpec {
        RuleSpec { pattern: pattern.encode_utf16().collect(), flags: flags.into(), replacement: replacement.encode_utf16().collect() }
    }
    fn plan(rules: &[RuleSpec], limits: Limits) -> CompiledPlan {
        CompiledPlan::compile(rules, limits, &ExecutionControl::default()).unwrap()
    }
    #[test]
    fn retains_utf16_and_order_across_repeated_execution() {
        let input = [0xd800, 97, 97, 0xdc00];
        let plan = plan(&[rule("(a)\\1", "g", "$1b"), rule("(?<=a)b", "g", "$`:$&:$'")], Limits::default());
        let expected = [0xd800, 97, 0xd800, 97, 58, 98, 58, 0xdc00, 0xdc00];
        for _ in 0..2 { assert_eq!(plan.execute(&input, &ExecutionControl::default()).unwrap().output, expected); }
    }
    #[test]
    fn global_empty_matches_advance_by_the_selected_encoding() {
        for (flags, expected) in [("g", vec![124, 0xd83d, 124, 0xde00, 124]), ("gu", vec![124, 0xd83d, 0xde00, 124]), ("gyu", vec![124, 0xd83d, 0xde00, 124])] {
            assert_eq!(plan(&[rule("(?:)", flags, "|")], Limits::default()).execute(&[0xd83d, 0xde00], &ExecutionControl::default()).unwrap().output, expected);
        }
        assert_eq!(plan(&[rule("a*", "g", "X")], Limits::default()).execute(&[97], &ExecutionControl::default()).unwrap().output, vec![88,88]);
    }
    #[test]
    fn source_and_runtime_limits_return_no_partial_output() {
        let control = ExecutionControl::default();
        let specs = [rule("a", "g", "XX")];
        for limits in [Limits { rules: 0, ..Limits::default() }, Limits { pattern_units: 0, ..Limits::default() }, Limits { source_units: 0, ..Limits::default() }] {
            assert_eq!(CompiledPlan::compile(&specs, limits, &control).err().unwrap().kind, FailureKind::ResourceLimit);
        }
        for limits in [Limits { input_units: 1, ..Limits::default() }, Limits { output_units: 3, ..Limits::default() }, Limits { matches: 1, ..Limits::default() }] {
            assert_eq!(plan(&specs, limits).execute(&[97,97], &control).unwrap_err().kind, FailureKind::ResourceLimit);
        }
        let ordered = plan(&[rule("a", "g", "b"), rule("b", "g", "c")], Limits { matches: 1, ..Limits::default() });
        assert_eq!(ordered.execute(&[97], &control).unwrap_err().rule_index, Some(1));
    }
    #[test]
    fn cancellation_and_expired_deadline_are_checked_before_work() {
        let compiled = plan(&[rule("a", "g", "b")], Limits::default());
        let cancel = AtomicBool::new(true);
        for (control, kind) in [(ExecutionControl { cancelled: Some(&cancel), deadline: None }, FailureKind::Cancelled), (ExecutionControl { cancelled: None, deadline: Some(Instant::now()) }, FailureKind::DeadlineExceeded)] {
            assert_eq!(compiled.execute(&[97], &control).unwrap_err().kind, kind);
            assert_eq!(CompiledPlan::compile(&[], Limits::default(), &control).err().unwrap().kind, kind);
        }
    }
    #[test]
    fn unicode_escape_rules_execute_natively() {
        let cases = [
            (r"\u{61}", "g", vec![97], vec![97]),
            (r"\u{61}", "g", vec![117; 61], vec![120]),
            (r"\uD83D\uDE00", "g", vec![0xD83D, 0xDE00], vec![120]),
            (r"[\uD83D\uDE00]", "g", vec![0xD83D, 0xDE00], vec![120, 120]),
            (r"\uD800\u0041", "gu", vec![0xD800, 65], vec![120]),
            (r"\uD800\uD800", "gu", vec![0xD800, 0xD800], vec![120]),
            (r"\uD800\u{41}", "gu", vec![0xD800, 65], vec![120]),
            (r"\u{61}", "gu", vec![97], vec![120]),
        ];
        for (pattern, flags, input, expected) in cases {
            let compiled = plan(&[rule(pattern, flags, "x")], Limits::default());
            assert_eq!(compiled.execute(&input, &ExecutionControl::default()).unwrap().output, expected, "{pattern} / {flags}");
        }
        let compiled = plan(&[rule(r"(?<a\u003e>z)", "g", "$<a>|$&|$1")], Limits::default());
        assert_eq!(compiled.execute(&[62, 122], &ExecutionControl::default()).unwrap().output, ">z|>z|>z".encode_utf16().collect::<Vec<_>>());
        let rejected = CompiledPlan::compile(&[rule(r"\uZZZZ", "gu", "x")], Limits::default(), &ExecutionControl::default()).err().unwrap();
        assert_eq!(rejected.kind, FailureKind::EngineRejected);
    }
    #[test]
    fn legacy_case_is_admitted_and_unsupported_flags_are_rejected() {
        for (flags, expected) in [("i", "xé éÉ sſ sS"), ("gim", "xx xx sſ x")] {
            let compiled = plan(&[rule("[é]|(s)\\1", flags, "x")], Limits::default());
            let input: Vec<u16> = "Éé éÉ sſ sS".encode_utf16().collect();
            assert_eq!(compiled.execute(&input, &ExecutionControl::default()).unwrap().output,
                expected.encode_utf16().collect::<Vec<_>>());
        }
        assert_eq!(CompiledPlan::compile(&[rule("a", "v", "x")], Limits::default(), &ExecutionControl::default()).err().unwrap().kind, FailureKind::Unsupported);
        for flags in ["g", "gu"] {
            let compiled = plan(&[rule(r"\\u0061", flags, "x")], Limits::default());
            assert_eq!(compiled.execute(&r"\u0061".encode_utf16().collect::<Vec<_>>(), &ExecutionControl::default()).unwrap().output, vec![120]);
        }
        for flags in ["gg", "z", "uv"] {
            assert_eq!(CompiledPlan::compile(&[rule("a", flags, "x")], Limits::default(), &ExecutionControl::default()).err().unwrap().kind, FailureKind::EngineRejected);
        }
    }
}
