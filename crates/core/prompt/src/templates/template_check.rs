//! `templateCheck` (`src/ts/process/templates/templateCheck.ts`): the warnings the prompt template editor shows for
//! `db.promptTemplate`.

use risunest_ir::num::number_to_text;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};
use std::borrow::Cow;

/// A range bound as `includes` and `join` see it.
enum Bound {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Text(Vec<u16>),
}

impl Bound {
    fn read(value: Option<&Value>) -> Result<Self, StageError> {
        Ok(match value {
            None | Some(Value::Undefined) => Bound::Undefined,
            Some(Value::Null) => Bound::Null,
            Some(Value::Bool(flag)) => Bound::Bool(*flag),
            Some(Value::Number(number)) => Bound::Number(*number),
            Some(Value::Text(text)) => Bound::Text(text.units().collect()),
            // `includes` compares an object by identity and `join` calls its `toString`.
            Some(Value::Array(_) | Value::Object(_)) => return Err(StageError::Unsupported(Cow::Borrowed("promptTemplate.range"))),
        })
    }
    /// SameValueZero, as `Array.prototype.includes` compares.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Bound::Undefined, Bound::Undefined) | (Bound::Null, Bound::Null) => true,
            (Bound::Bool(a), Bound::Bool(b)) => a == b,
            (Bound::Number(a), Bound::Number(b)) => a == b || (a.is_nan() && b.is_nan()),
            (Bound::Text(a), Bound::Text(b)) => a == b,
            _ => false,
        }
    }
    /// `Array.prototype.join` of one element: `null` and `undefined` are empty.
    fn join_into(&self, out: &mut Vec<u16>, budget: &Budget) -> Result<(), StageError> {
        match self {
            Bound::Undefined | Bound::Null => {}
            Bound::Bool(flag) => out.extend(if *flag { "true" } else { "false" }.encode_utf16()),
            Bound::Number(number) => out.extend(number_to_text(*number, budget)?.units()),
            Bound::Text(text) => out.extend_from_slice(text),
        }
        Ok(())
    }
}

fn is_text(value: Option<&Value>, ascii: &str) -> bool { matches!(value, Some(Value::Text(text)) if text.equals_ascii(ascii.as_bytes())) }

/// The warnings `templateCheck(db)` returns for `db.promptTemplate`, in its order.
pub fn template_check(prompt_template: &Value, budget: &Budget) -> Result<Vec<Text>, StageError> {
    if !prompt_template.truthy() { return Ok(Vec::new()); }
    // A truthy template that is not an array has no cards for the loop's `temp[i]`, or a `length` TS would read.
    let Value::Array(cards) = prompt_template else { return Err(StageError::Unsupported(Cow::Borrowed("promptTemplate"))) };
    let (mut main, mut notes) = (0usize, 0usize);
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
    let (mut description, mut lorebook, mut reach_end) = (false, false, false);
    for card in cards.as_slice() {
        budget.work(1)?;
        // `c.type` on `null` or `undefined` throws; on another primitive it is undefined.
        if matches!(card, Value::Null | Value::Undefined) { return Err(StageError::Invalid(Cow::Borrowed("promptTemplate"))); }
        let field = |key: &str| -> Result<Option<&Value>, StageError> {
            match card { Value::Object(_) => Ok(card.get(key.as_bytes(), budget)?), _ => Ok(None) }
        };
        let kind = field("type")?;
        if is_text(kind, "jailbreak") || is_text(kind, "plain") {
            let type2 = field("type2")?;
            if is_text(type2, "globalNote") { notes += 1; }
            if is_text(type2, "main") { main += 1; }
        } else if is_text(kind, "chat") {
            let start = field("rangeStart")?;
            if !matches!(start, Some(Value::Number(number)) if *number == -1000.0) {
                if !matches!(start, Some(Value::Number(number)) if *number == 0.0) { starts.push(Bound::read(start)?); }
                let end = field("rangeEnd")?;
                if is_text(end, "end") { reach_end = true; } else { ends.push(Bound::read(end)?); }
            }
        } else if is_text(kind, "description") {
            description = true;
        } else if is_text(kind, "lorebook") {
            lorebook = true;
        }
    }
    budget.work(starts.len().saturating_mul(ends.len()).saturating_mul(2))?;
    let unresolved: Vec<&Bound> = starts.iter().filter(|start| !ends.iter().any(|end| end.same(start)))
        .chain(ends.iter().filter(|end| !starts.iter().any(|start| start.same(end)))).collect();

    let mut warnings = Vec::new();
    let mut warn = |message: &str| -> Result<(), StageError> { warnings.push(Text::from_str(message, budget)?); Ok(()) };
    if main == 0 { warn("No main prompt entry found")?; }
    if main > 1 { warn("Multiple main prompt entries found, this can result in unexpected behavior")?; }
    if notes == 0 { warn("No global notes entry found")?; }
    if notes > 1 { warn("Multiple global notes entries found, this can result in unexpected behavior")?; }
    if !description { warn("No description entry found")?; }
    if !lorebook { warn("No lorebook entry found")?; }
    if !reach_end { warn("No chat entry found with range end set to \"Until chat end\"")?; }
    if !unresolved.is_empty() {
        let mut units: Vec<u16> = "Chat are not connected: [".encode_utf16().collect();
        for (index, bound) in unresolved.iter().enumerate() {
            if index > 0 { units.extend(", ".encode_utf16()); }
            bound.join_into(&mut units, budget)?;
        }
        units.push(u16::from(b']'));
        warnings.push(Text::from_units(&units, budget)?);
    }
    Ok(warnings)
}
