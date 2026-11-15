//! `customscript` fields, ordering and `ableFlag`.

use crate::flags;
use risunest_ir::budget::{Budget, Charge, Error};
use risunest_ir::stage::{ScriptMode, StageError};
use risunest_ir::{Text, Value};
use std::borrow::Cow;

/// The `ScriptMode` string TS compares `customscript.type` with.
pub fn mode_name(mode: ScriptMode) -> &'static str {
    match mode {
        ScriptMode::EditInput => "editinput",
        ScriptMode::EditOutput => "editoutput",
        ScriptMode::EditProcess => "editprocess",
        ScriptMode::EditDisplay => "editdisplay",
    }
}

pub fn parse_mode(value: &Text) -> Option<ScriptMode> {
    [ScriptMode::EditInput, ScriptMode::EditOutput, ScriptMode::EditProcess, ScriptMode::EditDisplay]
        .into_iter().find(|mode| value.equals_ascii(mode_name(*mode).as_bytes()))
}

/// One stored `customscript` entry.
#[derive(Debug)]
pub struct CustomScript {
    /// `in`.
    pub pattern: Text,
    /// `out`.
    pub replacement: Text,
    /// `type`; `None` for a value that is not a `ScriptMode`, which no mode runs.
    pub mode: Option<ScriptMode>,
    /// `flag`; empty when absent, which every TS reader treats alike.
    pub flag: Text,
    /// `ableFlag`, as its truthiness.
    pub able_flag: bool,
}

impl CustomScript {
    pub fn new(pattern: Text, replacement: Text, mode: Option<ScriptMode>, flag: Text, able_flag: bool) -> Self {
        Self { pattern, replacement, mode, flag, able_flag }
    }

    /// Reads a `customscript` object as it is stored.
    pub fn from_value(value: &Value, budget: &Budget) -> Result<Self, StageError> {
        let text = |key: &'static str, field: &'static str| -> Result<Text, StageError> {
            match value.get(key.as_bytes(), budget)? {
                Some(Value::Text(text)) => Ok(text.copy(budget)?),
                _ => Err(StageError::Invalid(Cow::Borrowed(field))),
            }
        };
        let pattern = text("in", "customscript.in")?;
        let replacement = text("out", "customscript.out")?;
        let mode = match value.get(b"type", budget)? { Some(Value::Text(kind)) => parse_mode(kind), _ => None };
        let flag = match value.get(b"flag", budget)? {
            None | Some(Value::Undefined | Value::Null) => Text::from_str("", budget)?,
            Some(Value::Text(flag)) => flag.copy(budget)?,
            Some(_) => return Err(StageError::Invalid(Cow::Borrowed("customscript.flag"))),
        };
        let able_flag = value.get(b"ableFlag", budget)?.is_some_and(Value::truthy);
        Ok(Self { pattern, replacement, mode, flag, able_flag })
    }
}

/// One entry of `getRegexExecutionPlan` (`src/ts/process/regexExecutionPlan.ts`).
#[derive(Debug)]
pub struct PlanEntry {
    /// Index of the script in the list the runner was built with.
    pub source_index: usize,
    /// `script.in`.
    pub pattern: Vec<u16>,
    /// `script.out` after `$n`, `{{data}}` and the trailing-newline rewrite.
    pub replacement: Vec<u16>,
    /// `script.out` as stored, which `@@emo` reads.
    pub out: Vec<u16>,
    /// The normalized flags, a subset of `dgimsuvy` in first-seen order.
    pub flags: String,
    pub order: f64,
    pub actions: Vec<Vec<u16>>,
    /// The `<cbs>` action: the pattern goes through CBS before each use.
    pub dynamic_pattern: bool,
}

impl PlanEntry {
    pub fn has_action(&self, name: &str) -> bool { self.actions.iter().any(|action| equals(action, name)) }
}

/// `getRegexExecutionPlan(scripts, mode).entries`.
pub fn plan(scripts: &[CustomScript], mode: ScriptMode, budget: &Budget) -> Result<Vec<PlanEntry>, StageError> {
    let mut entries = Vec::new();
    for (source_index, script) in scripts.iter().enumerate() {
        if script.mode != Some(mode) { continue; }
        budget.work(script.pattern.len_units() + script.replacement.len_units() + script.flag.len_units() + 1)?;
        entries.push(flags::parse_entry(script, source_index, budget)?);
    }
    if entries.iter().any(|entry| entry.order != 0.0) { sort(&mut entries)?; }
    Ok(entries)
}

/// `(left, right) => right.order - left.order || left.sourceIndex - right.sourceIndex`.
fn compare(left: &PlanEntry, right: &PlanEntry) -> f64 {
    let order = right.order - left.order;
    if order != 0.0 && !order.is_nan() { order } else { left.source_index as f64 - right.source_index as f64 }
}

/// `Array.prototype.sort` with the plan comparator. A `NaN` order (`<order x>`) makes the comparator inconsistent, so
/// the result depends on V8's TimSort; below 64 entries that is one counted run finished by binary insertion, ported
/// here. Larger plans with a `NaN` order would need the merge phase as well.
fn sort(entries: &mut Vec<PlanEntry>) -> Result<(), StageError> {
    let length = entries.len();
    if length < 2 { return Ok(()); }
    if length >= 64 {
        if entries.iter().any(|entry| entry.order.is_nan()) { return Err(StageError::Unsupported(Cow::Borrowed("script-order-nan"))); }
        entries.sort_by(|left, right| compare(left, right).partial_cmp(&0.0).expect("finite comparator"));
        return Ok(());
    }
    // CountAndMakeRun.
    let mut run = 2;
    let descending = compare(&entries[1], &entries[0]) < 0.0;
    while run < length {
        let order = compare(&entries[run], &entries[run - 1]);
        if (descending && order >= 0.0) || (!descending && order < 0.0) { break; }
        run += 1;
    }
    if descending { entries[..run].reverse(); }
    // BinaryInsertionSort(low, low + run, high).
    for start in run..length {
        let (mut left, mut right) = (0, start);
        while left < right {
            let middle = left + ((right - left) >> 1);
            if compare(&entries[start], &entries[middle]) < 0.0 { right = middle; } else { left = middle + 1; }
        }
        entries[left..=start].rotate_right(1);
    }
    Ok(())
}

/// An owned-bytes charge that grows with what it covers; growing replaces it, so it is one charge.
#[derive(Debug)]
pub(crate) struct Held { bytes: usize, _charge: Charge }

impl Held {
    pub(crate) fn new(bytes: usize, budget: &Budget) -> Result<Self, Error> { Ok(Self { bytes, _charge: budget.reserve_owned(bytes)? }) }

    /// Covers `bytes` before they are allocated, growing by at least half when it grows.
    pub(crate) fn cover(&mut self, bytes: usize, budget: &Budget) -> Result<(), Error> {
        if bytes <= self.bytes { return Ok(()); }
        let bytes = bytes.max(self.bytes.saturating_add(self.bytes / 2));
        *self = Self::new(bytes, budget)?;
        Ok(())
    }
}

/// UTF-16 units charged to the budget while they live.
pub(crate) struct Units16 {
    pub(crate) data: Vec<u16>,
    held: Held,
}

impl Units16 {
    pub(crate) fn of(text: &Text, budget: &Budget) -> Result<Self, Error> {
        budget.work(text.len_units())?;
        let held = Held::new(text.len_units().saturating_mul(2), budget)?;
        Ok(Self { data: text.units().collect(), held })
    }

    pub(crate) fn from_vec(data: Vec<u16>, budget: &Budget) -> Result<Self, Error> {
        let held = Held::new(data.len().saturating_mul(2), budget)?;
        Ok(Self { data, held })
    }

    pub(crate) fn with_capacity(capacity: usize, budget: &Budget) -> Result<Self, Error> {
        let mut units = Self { data: Vec::new(), held: Held::new(0, budget)? };
        units.reserve(capacity, budget)?;
        Ok(units)
    }

    /// Makes room for `additional` more units, charging the grown capacity first.
    pub(crate) fn reserve(&mut self, additional: usize, budget: &Budget) -> Result<(), Error> {
        let needed = self.data.len().checked_add(additional).ok_or(Error::Limit("owned-bytes"))?;
        if needed <= self.data.capacity() { return Ok(()); }
        let target = needed.max(self.data.capacity().saturating_mul(2));
        self.held.cover(target.saturating_mul(2), budget)?;
        self.data.try_reserve_exact(target - self.data.len()).map_err(|_| Error::Allocation)
    }

    /// Appends `values`, charging their work and room.
    pub(crate) fn extend(&mut self, values: &[u16], budget: &Budget) -> Result<(), Error> {
        budget.work(values.len())?;
        self.reserve(values.len(), budget)?;
        self.data.extend_from_slice(values);
        Ok(())
    }

    pub(crate) fn into_vec(self) -> Vec<u16> { self.data }
}

pub(crate) fn ascii(value: &str) -> Vec<u16> { value.encode_utf16().collect() }

pub(crate) fn starts_with(value: &[u16], prefix: &str) -> bool {
    value.len() >= prefix.len() && value.iter().zip(prefix.bytes()).all(|(unit, byte)| *unit == byte as u16)
}

pub(crate) fn equals(value: &[u16], other: &str) -> bool { value.len() == other.len() && starts_with(value, other) }

/// `String.prototype.indexOf(needle, from)`.
pub(crate) fn find(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() { return Some(from.min(haystack.len())); }
    if needle.len() > haystack.len() { return None; }
    (from..=haystack.len() - needle.len()).find(|index| &haystack[*index..*index + needle.len()] == needle)
}

/// `String.prototype.trim`, through [`Text::trim`].
pub(crate) fn trim(value: &[u16], budget: &Budget) -> Result<Vec<u16>, StageError> {
    Ok(Text::from_units(value, budget)?.trim(budget)?.units().collect())
}

/// [`has_cbs_syntax`] on UTF-8, whose multi-byte sequences hold no ASCII byte.
pub(crate) fn has_cbs_syntax_str(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| match byte {
        b'{' => matches!(bytes.get(index + 1), Some(b'{' | b'#')),
        b'<' => [&b"user>"[..], b"char>", b"bot>"].iter()
            .any(|word| bytes.get(index + 1..index + 1 + word.len()).is_some_and(|slice| slice.eq_ignore_ascii_case(word))),
        _ => false,
    })
}

/// `/\{[{#]|<(?:user|char|bot)>/i`: `risuChatParser` returns any other text unchanged.
pub(crate) fn has_cbs_syntax(value: &[u16]) -> bool {
    let lower = |unit: u16| if (65..=90).contains(&unit) { unit + 32 } else { unit };
    value.iter().enumerate().any(|(index, unit)| match unit {
        123 => matches!(value.get(index + 1), Some(123 | 35)),
        60 => ["user>", "char>", "bot>"].iter().any(|word| {
            value.len() >= index + 1 + word.len()
                && value[index + 1..index + 1 + word.len()].iter().zip(word.bytes()).all(|(unit, byte)| lower(*unit) == byte as u16)
        }),
        _ => false,
    })
}
