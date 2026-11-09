//! The JS operations the mappings perform, over `Value`.

use risunest_ir::budget::{Budget, Error, OwnedVec};
use risunest_ir::json::stringify;
use risunest_ir::stage::StageError;
use risunest_ir::text::{Text, TextBuilder};
use risunest_ir::value::Value;
use std::borrow::Cow;

pub(crate) fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }
pub(crate) fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

/// `value.key` for a key no prototype defines: an object's own property, `None` (`undefined`) on anything else.
pub(crate) fn field<'a>(value: &'a Value, key: &str, budget: &Budget) -> Result<Option<&'a Value>, Error> {
    match value.get(key.as_bytes(), budget)? {
        Some(Value::Undefined) => Ok(None),
        found => Ok(found),
    }
}

/// `value === undefined || value === null`.
pub(crate) fn nullish(value: Option<&Value>) -> bool { matches!(value, None | Some(Value::Undefined | Value::Null)) }

pub(crate) fn truthy(value: Option<&Value>) -> bool { value.is_some_and(Value::truthy) }

/// A plain object, as opposed to an array or a primitive.
pub(crate) fn object(value: Option<&Value>) -> Option<&Value> { value.filter(|value| matches!(value, Value::Object(_))) }

/// A deep copy (`structuredClone` of JSON data); `Value` is not `Clone`.
pub(crate) fn copy(value: &Value, budget: &Budget) -> Result<Value, Error> { copy_at(value, 0, budget) }

fn copy_at(value: &Value, depth: usize, budget: &Budget) -> Result<Value, Error> {
    budget.work(1)?;
    if depth > budget.limits().max_depth { return Err(Error::Limit("json-depth")); }
    Ok(match value {
        Value::Undefined => Value::Undefined,
        Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(value) => Value::Number(*value),
        Value::Text(value) => Value::Text(value.copy(budget)?),
        Value::Array(values) => {
            let mut copy = OwnedVec::new(budget)?;
            for value in values.as_slice() { copy.push(copy_at(value, depth + 1, budget)?)?; }
            Value::Array(copy)
        }
        Value::Object(entries) => {
            let mut copy = OwnedVec::new(budget)?;
            for (key, value) in entries.as_slice() { copy.push((key.copy(budget)?, copy_at(value, depth + 1, budget)?))?; }
            Value::Object(copy)
        }
    })
}

/// A copy of a property read, `undefined` when it is absent.
pub(crate) fn copy_or_undefined(value: Option<&Value>, budget: &Budget) -> Result<Value, Error> {
    match value { Some(value) => copy(value, budget), None => Ok(Value::Undefined) }
}

/// `value ?? fallback`.
pub(crate) fn coalesce(value: Option<&Value>, fallback: impl FnOnce() -> Result<Value, Error>, budget: &Budget) -> Result<Value, Error> {
    match value {
        Some(value) if !nullish(Some(value)) => copy(value, budget),
        _ => fallback(),
    }
}

pub(crate) fn text(value: &str, budget: &Budget) -> Result<Value, Error> { Ok(Value::Text(Text::from_str(value, budget)?)) }
pub(crate) fn empty_array(budget: &Budget) -> Result<Value, Error> { Ok(Value::Array(OwnedVec::new(budget)?)) }
pub(crate) fn empty_object(budget: &Budget) -> Result<Value, Error> { Ok(Value::Object(OwnedVec::new(budget)?)) }

pub(crate) fn array(values: Vec<Value>, budget: &Budget) -> Result<Value, Error> {
    let mut array = OwnedVec::new(budget)?;
    for value in values { array.push(value)?; }
    Ok(Value::Array(array))
}

/// `object[key] = value` on an ordinary object.
pub(crate) fn set(object: &mut Value, key: &str, value: Value, budget: &Budget) -> Result<(), Error> {
    object.define(Text::from_str(key, budget)?, value, budget)
}

/// `delete object[key]`.
pub(crate) fn remove(object: &mut Value, key: &str, budget: &Budget) -> Result<(), Error> {
    object.take(key.as_bytes(), budget)?;
    Ok(())
}

pub(crate) fn push(array: &mut Value, value: Value) -> Result<(), Error> {
    match array {
        Value::Array(values) => values.push(value),
        _ => Err(Error::Type { operation: "array-push" }),
    }
}

/// `String(value)`, the conversion of a template substitution and of a `+` operand on JSON data. An object with an own
/// `toString` property makes TS throw, which is a decline here.
pub(crate) fn string(value: Option<&Value>, budget: &Budget) -> Result<Text, StageError> {
    let Some(value) = value else { return Ok(Text::from_str("undefined", budget)?) };
    value.to_text(budget).map_err(|error| match error {
        Error::Type { .. } => unsupported("toPrimitive"),
        other => other.into(),
    })
}

/// The concatenation of `parts` and `String(tail)`.
pub(crate) fn concat(parts: &[&Text], tail: Option<&Value>, budget: &Budget) -> Result<Text, StageError> {
    let tail = string(tail, budget)?;
    let mut all = parts.to_vec();
    all.push(&tail);
    Ok(concat_texts(&all, budget)?)
}

/// The concatenation of `parts`.
pub(crate) fn concat_texts(parts: &[&Text], budget: &Budget) -> Result<Text, Error> {
    let mut output = TextBuilder::new(budget)?;
    for part in parts {
        for unit in part.units() { output.unit(unit)?; }
    }
    output.finish()
}

/// `value.key` for writing: an object's own property.
pub(crate) fn field_mut<'a>(value: &'a mut Value, key: &str, budget: &Budget) -> Result<Option<&'a mut Value>, Error> {
    budget.work(1)?;
    let Value::Object(entries) = value else { return Ok(None) };
    Ok(entries.as_mut_slice().iter_mut().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value))
}

/// `Array.prototype.join(separator)`: `undefined` and `null` elements join as empty strings.
pub(crate) fn join(values: &[Value], separator: &str, budget: &Budget) -> Result<Text, StageError> {
    let mut output = TextBuilder::new(budget)?;
    for (index, value) in values.iter().enumerate() {
        budget.work(1)?;
        if index != 0 { for unit in separator.encode_utf16() { output.unit(unit)?; } }
        if !matches!(value, Value::Undefined | Value::Null) {
            for unit in string(Some(value), budget)?.units() { output.unit(unit)?; }
        }
    }
    Ok(output.finish()?)
}

pub(crate) fn starts_with(value: &Text, prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    value.len_units() >= prefix.len() && value.units().zip(&prefix).all(|(unit, expected)| unit == *expected)
}

/// `value.slice(start)` for a prefix of `start` units.
pub(crate) fn after(value: &Text, start: usize, budget: &Budget) -> Result<Text, Error> { value.slice(start, value.len_units(), budget) }

/// `value.split(',').map(part => part.trim())`.
pub(crate) fn split_trimmed(value: &Text, budget: &Budget) -> Result<Value, Error> {
    let mut parts = OwnedVec::new(budget)?;
    let mut part = Vec::new();
    let units: Vec<u16> = value.units().collect();
    budget.work(units.len())?;
    for unit in units.iter().copied().chain(std::iter::once(u16::from(b','))) {
        if unit == u16::from(b',') {
            parts.push(Value::Text(Text::from_units(&part, budget)?.trim(budget)?))?;
            part.clear();
        } else {
            part.push(unit);
        }
    }
    Ok(Value::Array(parts))
}

/// Whether any object in `value` has an own `__proto__` property. TS assignment and cloning treat that key apart from
/// ordinary ones, so the mappings decline it.
pub(crate) fn has_proto_key(value: &Value, budget: &Budget) -> Result<bool, Error> {
    budget.work(1)?;
    match value {
        Value::Array(values) => {
            for value in values.as_slice() { if has_proto_key(value, budget)? { return Ok(true); } }
            Ok(false)
        }
        Value::Object(entries) => {
            for (key, value) in entries.as_slice() {
                if key.equals_ascii(b"__proto__") || has_proto_key(value, budget)? { return Ok(true); }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// `values.splice(index, 0, value)` for an `index` within the array.
pub(crate) fn insert(values: &mut OwnedVec<Value>, index: usize, value: Value, budget: &Budget) -> Result<(), Error> {
    let mut inserted = OwnedVec::new(budget)?;
    let mut value = Some(value);
    for (at, item) in values.as_mut_slice().iter_mut().enumerate() {
        budget.work(1)?;
        if at == index { if let Some(value) = value.take() { inserted.push(value)?; } }
        inserted.push(std::mem::replace(item, Value::Undefined))?;
    }
    if let Some(value) = value { inserted.push(value)?; }
    *values = inserted;
    Ok(())
}

/// `Math.round`: the nearest integer, halves toward positive infinity.
pub(crate) fn round(value: f64) -> f64 {
    if !value.is_finite() { return value; }
    let floor = value.floor();
    if value - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// `typeof value` for JSON data, `None` (absent) reading as `undefined`.
pub(crate) fn type_of(value: Option<&Value>) -> &'static str {
    match value {
        None | Some(Value::Undefined) => "undefined",
        Some(Value::Null | Value::Array(_) | Value::Object(_)) => "object",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::Text(_)) => "string",
    }
}

/// `JSON.stringify(value, null, gap)` for a `gap` of 1 to 10 spaces: every key and leaf is [`stringify`]'s text, each
/// member starts on a new line indented by `gap` spaces per level, a key is followed by `": "`, and an empty array or an
/// object without members stays `[]` or `{}`. `None` where JS returns `undefined`.
pub(crate) fn stringify_indented(value: &Value, gap: usize, budget: &Budget) -> Result<Option<Text>, Error> {
    if matches!(value, Value::Undefined) { budget.work(1)?; return Ok(None); }
    let mut writer = Indented { out: Text::from_str("", budget)?, gap, lines: Vec::new(), budget };
    writer.value(value, 0)?;
    Ok(Some(writer.out))
}

struct Indented<'a> { out: Text, gap: usize, lines: Vec<Text>, budget: &'a Budget }
impl Indented<'_> {
    fn ascii(&mut self, value: &str) -> Result<(), Error> { self.out.append(&Text::from_str(value, self.budget)?) }
    // A new line and the indent of `depth`, built once per depth.
    fn line(&mut self, depth: usize) -> Result<(), Error> {
        while self.lines.len() <= depth {
            let line = format!("\n{}", " ".repeat(self.gap * self.lines.len()));
            self.lines.push(Text::from_str(&line, self.budget)?);
        }
        self.out.append(&self.lines[depth])
    }
    fn leaf(&mut self, value: &Value) -> Result<(), Error> {
        let text = stringify(value, self.budget)?.ok_or(Error::Invariant("json-leaf"))?;
        self.out.append(&text)
    }
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), Error> {
        self.budget.work(1)?;
        if depth > self.budget.limits().max_depth { return Err(Error::Limit("json-depth")); }
        match value {
            Value::Array(values) if values.as_slice().is_empty() => self.ascii("[]"),
            Value::Array(values) => {
                self.ascii("[")?;
                for (index, value) in values.as_slice().iter().enumerate() {
                    if index != 0 { self.ascii(",")?; }
                    self.line(depth + 1)?;
                    // An undefined element is written as null.
                    if matches!(value, Value::Undefined) { self.ascii("null")?; } else { self.value(value, depth + 1)?; }
                }
                self.line(depth)?;
                self.ascii("]")
            }
            Value::Object(entries) => {
                let mut any = false;
                for (key, value) in entries.as_slice() {
                    self.budget.work(1)?;
                    if matches!(value, Value::Undefined) { continue; }
                    self.ascii(if any { "," } else { "{" })?;
                    any = true;
                    self.line(depth + 1)?;
                    self.leaf(&Value::Text(key.copy(self.budget)?))?;
                    self.ascii(": ")?;
                    self.value(value, depth + 1)?;
                }
                if !any { return self.ascii("{}"); }
                self.line(depth)?;
                self.ascii("}")
            }
            leaf => self.leaf(leaf),
        }
    }
}
