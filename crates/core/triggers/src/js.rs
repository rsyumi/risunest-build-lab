//! The JS built-ins the trigger effects call, over `Text` units and parsed JSON values.

use risunest_cbs_experimental::cbs::processor::CaseTables;
use risunest_cbs_experimental::cbs::ws;
use risunest_ir::budget::{Budget, Error, OwnedVec};
use risunest_ir::num::number_to_text;
use risunest_ir::stage::StageError;
use risunest_ir::text::{unicode16_case_differs, Text, TextBuilder};
use risunest_ir::value::Value;
use std::borrow::Cow;

pub(crate) fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }

pub(crate) fn units(text: &Text) -> Vec<u16> {
    match text.as_unicode() {
        Some(value) => value.encode_utf16().collect(),
        None => text.units().collect(),
    }
}

pub(crate) fn text(units: &[u16], budget: &Budget) -> Result<Text, Error> {
    match String::from_utf16(units) {
        Ok(value) => Text::from_str(&value, budget),
        Err(_) => Text::from_units(units, budget),
    }
}

pub(crate) fn literal(value: &str, budget: &Budget) -> Result<Text, Error> { Text::from_str(value, budget) }

pub(crate) fn concat(left: &Text, right: &Text, budget: &Budget) -> Result<Text, Error> {
    let mut output = left.copy(budget)?;
    output.append(right)?;
    Ok(output)
}

/// `String(value)` of a non-negative count.
pub(crate) fn count_text(count: usize, budget: &Budget) -> Result<Text, Error> { number_to_text(count as f64, budget) }

/// A deep copy; `Value` is not `Clone`.
pub(crate) fn copy_value(value: &Value, budget: &Budget) -> Result<Value, Error> {
    budget.work(1)?;
    Ok(match value {
        Value::Undefined => Value::Undefined,
        Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(value) => Value::Number(*value),
        Value::Text(value) => Value::Text(value.copy(budget)?),
        Value::Array(values) => {
            let mut copy = OwnedVec::new(budget)?;
            for value in values.as_slice() { copy.push(copy_value(value, budget)?)?; }
            Value::Array(copy)
        }
        Value::Object(entries) => {
            let mut copy = OwnedVec::new(budget)?;
            for (key, value) in entries.as_slice() { copy.push((key.copy(budget)?, copy_value(value, budget)?))?; }
            Value::Object(copy)
        }
    })
}

pub(crate) fn array(values: Vec<Value>, budget: &Budget) -> Result<Value, Error> {
    let mut array = OwnedVec::new(budget)?;
    for value in values { array.push(value)?; }
    Ok(Value::Array(array))
}

/// The elements of an array value, moved out.
pub(crate) fn into_elements(value: Value) -> Vec<Value> {
    match value {
        Value::Array(mut values) => values.as_mut_slice().iter_mut().map(|value| std::mem::replace(value, Value::Null)).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn text_array(values: Vec<Text>, budget: &Budget) -> Result<Value, Error> {
    array(values.into_iter().map(Value::Text).collect(), budget)
}

/// `JSON.stringify` of a value that is never `undefined`.
pub(crate) fn stringify(value: &Value, budget: &Budget) -> Result<Text, StageError> {
    risunest_ir::json::stringify(value, budget)?.ok_or(StageError::Invalid(Cow::Borrowed("json-stringify")))
}

/// `ToIntegerOrInfinity`.
pub(crate) fn integer(value: f64) -> f64 { if value.is_nan() { 0.0 } else { value.trunc() } }

/// A relative `slice`/`splice` position over `length` elements.
pub(crate) fn relative(value: f64, length: usize) -> usize {
    let value = integer(value);
    if value < 0.0 { (length as f64 + value).max(0.0) as usize } else { value.min(length as f64) as usize }
}

/// The array index a numeric property key names: an integer below 2^32 - 1, `-0` included.
pub(crate) fn number_index(value: f64) -> Option<usize> {
    (value.fract() == 0.0 && value >= 0.0 && value < 4_294_967_295.0).then_some(value as usize)
}

/// The array index a string property key names: canonical decimal digits below 2^32 - 1.
pub(crate) fn key_index(key: &[u16]) -> Option<usize> {
    if key.is_empty() || key.len() > 10 { return None; }
    if key[0] == b'0' as u16 { return (key.len() == 1).then_some(0); }
    let mut value: u64 = 0;
    for unit in key {
        if !(b'0' as u16..=b'9' as u16).contains(unit) { return None; }
        value = value * 10 + (*unit - b'0' as u16) as u64;
    }
    (value < 4_294_967_295).then_some(value as usize)
}

pub(crate) fn equals(text: &Text, literal: &str) -> bool {
    if literal.is_ascii() { return text.equals_ascii(literal.as_bytes()); }
    text.len_units() == literal.encode_utf16().count() && text.units().eq(literal.encode_utf16())
}

/// `left.includes(right)` / `left.indexOf(right)` over UTF-16 units.
pub(crate) fn index_of(haystack: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() { return Some(0); }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// `source.split(delimiter)` with a string delimiter.
pub(crate) fn split(source: &[u16], delimiter: &[u16], budget: &Budget) -> Result<Vec<Text>, Error> {
    budget.work(source.len())?;
    let mut parts = Vec::new();
    if delimiter.is_empty() {
        for unit in source { parts.push(text(std::slice::from_ref(unit), budget)?); }
        return Ok(parts);
    }
    let mut start = 0;
    let mut at = 0;
    while at + delimiter.len() <= source.len() {
        if source[at..at + delimiter.len()] == *delimiter {
            parts.push(text(&source[start..at], budget)?);
            at += delimiter.len();
            start = at;
        } else {
            at += 1;
        }
    }
    parts.push(text(&source[start..], budget)?);
    Ok(parts)
}

/// `toLowerCase()` / `toUpperCase()`: the default case mapping of each well-formed run, isolated surrogates kept.
/// Rust's tables map it, so text the engine's `tables` could map otherwise declines as `trigger-case-table`.
pub(crate) fn case(source: &[u16], upper: bool, tables: CaseTables, budget: &Budget) -> Result<Text, StageError> {
    budget.work(source.len())?;
    let differs = match tables {
        CaseTables::Foreign => source.iter().any(|unit| *unit >= 0x80),
        CaseTables::Unicode16 => unicode16_case_differs(source),
        CaseTables::Native => false,
    };
    if differs { return Err(unsupported("trigger-case-table")); }
    let mut output = TextBuilder::new(budget)?;
    let mut run = String::new();
    let flush = |run: &mut String, output: &mut TextBuilder| -> Result<(), Error> {
        let mapped = if upper { run.to_uppercase() } else { run.to_lowercase() };
        for unit in mapped.encode_utf16() { output.unit(unit)?; }
        run.clear();
        Ok(())
    };
    for decoded in char::decode_utf16(source.iter().copied()) {
        match decoded {
            Ok(character) => run.push(character),
            Err(isolated) => {
                flush(&mut run, &mut output)?;
                output.unit(isolated.unpaired_surrogate())?;
            }
        }
    }
    flush(&mut run, &mut output)?;
    Ok(output.finish()?)
}

/// `[...source]`: code points, an isolated surrogate on its own.
pub(crate) fn code_points(source: &[u16]) -> Vec<&[u16]> {
    let mut points = Vec::new();
    let mut at = 0;
    while at < source.len() {
        let pair = (0xd800..=0xdbff).contains(&source[at]) && source.get(at + 1).is_some_and(|next| (0xdc00..=0xdfff).contains(next));
        let width = if pair { 2 } else { 1 };
        points.push(&source[at..at + width]);
        at += width;
    }
    points
}

// Own properties of the prototype objects a JSON value reads through. A read of one is a function or a getter.
const OBJECT_PROTOTYPE: &[&str] = &["constructor", "__defineGetter__", "__defineSetter__", "hasOwnProperty", "__lookupGetter__",
    "__lookupSetter__", "isPrototypeOf", "propertyIsEnumerable", "toString", "valueOf", "__proto__", "toLocaleString"];
const ARRAY_PROTOTYPE: &[&str] = &["length", "at", "concat", "copyWithin", "fill", "find", "findIndex", "findLast", "findLastIndex",
    "lastIndexOf", "pop", "push", "reverse", "shift", "unshift", "slice", "sort", "splice", "includes", "indexOf", "join", "keys",
    "entries", "values", "forEach", "filter", "flat", "flatMap", "map", "every", "some", "reduce", "reduceRight", "toReversed",
    "toSorted", "toSpliced", "with"];
const STRING_PROTOTYPE: &[&str] = &["length", "anchor", "at", "big", "blink", "bold", "charAt", "charCodeAt", "codePointAt", "concat",
    "endsWith", "fontcolor", "fontsize", "fixed", "includes", "indexOf", "isWellFormed", "italics", "lastIndexOf", "link",
    "localeCompare", "match", "matchAll", "normalize", "padEnd", "padStart", "repeat", "replace", "replaceAll", "search", "slice",
    "small", "split", "strike", "sub", "substr", "substring", "sup", "startsWith", "toWellFormed", "trim", "trimStart", "trimLeft",
    "trimEnd", "trimRight", "toLocaleLowerCase", "toLocaleUpperCase", "toLowerCase", "toUpperCase"];
const NUMBER_PROTOTYPE: &[&str] = &["toExponential", "toFixed", "toPrecision"];

fn named(key: &[u16], names: &[&str]) -> bool {
    names.iter().any(|name| name.len() == key.len() && name.bytes().zip(key).all(|(byte, unit)| byte as u16 == *unit))
}

/// Whether `key` reads an `Object.prototype` property from an ordinary object.
pub(crate) fn object_prototype_key(key: &[u16]) -> bool { named(key, OBJECT_PROTOTYPE) }

/// `value[key]` on a parsed JSON value.
pub(crate) enum Property<'a> {
    Missing,
    Value(&'a Value),
    /// A string's code unit.
    Unit(u16),
    /// An array's or a string's `length`.
    Length(usize),
    /// A function or getter read through a prototype.
    Other,
    /// `null[key]` throws.
    Throws,
}

pub(crate) fn property<'a>(value: &'a Value, key: &[u16], budget: &Budget) -> Result<Property<'a>, Error> {
    budget.work(key.len().saturating_add(1))?;
    Ok(match value {
        Value::Undefined | Value::Null => Property::Throws,
        Value::Object(entries) => match entries.as_slice().iter().find(|(name, _)| name.len_units() == key.len() && name.units().eq(key.iter().copied())) {
            Some((_, value)) => Property::Value(value),
            None if object_prototype_key(key) => Property::Other,
            None => Property::Missing,
        },
        Value::Array(values) => match key_index(key) {
            Some(index) => values.as_slice().get(index).map_or(Property::Missing, Property::Value),
            None if named(key, &["length"]) => Property::Length(values.as_slice().len()),
            None if object_prototype_key(key) || named(key, ARRAY_PROTOTYPE) => Property::Other,
            None => Property::Missing,
        },
        Value::Text(text) => match key_index(key) {
            Some(index) => text.units().nth(index).map_or(Property::Missing, Property::Unit),
            None if named(key, &["length"]) => Property::Length(text.len_units()),
            None if object_prototype_key(key) || named(key, STRING_PROTOTYPE) => Property::Other,
            None => Property::Missing,
        },
        Value::Number(_) | Value::Bool(_) => {
            if object_prototype_key(key) || named(key, NUMBER_PROTOTYPE) { Property::Other } else { Property::Missing }
        }
    })
}

/// `Object.hasOwn(value, key)`; `None` where it throws.
pub(crate) fn has_own(value: &Value, key: &[u16], budget: &Budget) -> Result<Option<bool>, Error> {
    budget.work(key.len().saturating_add(1))?;
    let length = [b'l', b'e', b'n', b'g', b't', b'h'].map(u16::from);
    Ok(match value {
        Value::Undefined | Value::Null => None,
        Value::Object(entries) => Some(entries.as_slice().iter().any(|(name, _)| name.len_units() == key.len() && name.units().eq(key.iter().copied()))),
        Value::Array(values) => Some(key == length || key_index(key).is_some_and(|index| index < values.as_slice().len())),
        Value::Text(text) => Some(key == length || key_index(key).is_some_and(|index| index < text.len_units())),
        Value::Number(_) | Value::Bool(_) => Some(false),
    })
}

/// `Object.keys(value)` and `Object.values(value)`; `None` where they throw.
pub(crate) fn own_entries(value: &Value, budget: &Budget) -> Result<Option<Vec<(Text, Value)>>, Error> {
    budget.work(1)?;
    Ok(Some(match value {
        Value::Undefined | Value::Null => return Ok(None),
        Value::Object(entries) => {
            let mut own = Vec::new();
            for (key, value) in entries.as_slice() { own.push((key.copy(budget)?, copy_value(value, budget)?)); }
            own
        }
        Value::Array(values) => {
            let mut own = Vec::new();
            for (index, value) in values.as_slice().iter().enumerate() { own.push((count_text(index, budget)?, copy_value(value, budget)?)); }
            own
        }
        Value::Text(source) => {
            let mut own = Vec::new();
            for (index, unit) in source.units().enumerate() { own.push((count_text(index, budget)?, Value::Text(text(&[unit], budget)?))); }
            own
        }
        Value::Number(_) | Value::Bool(_) => Vec::new(),
    }))
}

/// `value.toString()` on a property value; `None` where it throws.
pub(crate) fn to_string(value: &Value, budget: &Budget) -> Result<Option<Text>, Error> {
    match value {
        Value::Undefined | Value::Null => Ok(None),
        other => match other.to_text(budget) {
            Ok(text) => Ok(Some(text)),
            Err(Error::Type { .. }) => Ok(None),
            Err(error) => Err(error),
        },
    }
}

/// `array.join(delimiter)`; `None` where an element's conversion throws.
pub(crate) fn join<'v>(values: impl IntoIterator<Item = &'v Value>, delimiter: &Text, budget: &Budget) -> Result<Option<Text>, Error> {
    let mut output = literal("", budget)?;
    for (index, value) in values.into_iter().enumerate() {
        budget.work(1)?;
        if index != 0 { output.append(delimiter)?; }
        if matches!(value, Value::Undefined | Value::Null) { continue; }
        match value.to_text(budget) {
            Ok(text) => output.append(&text)?,
            Err(Error::Type { .. }) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(Some(output))
}

/// `left === right` for values that are not the same array or object: no two arrays or objects here are.
pub(crate) fn strict(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left == right,
        (Value::Text(left), Value::Text(right)) => left.equals(right),
        _ => false,
    }
}

/// SameValueZero, which `includes` uses: strict equality where `NaN` equals itself.
pub(crate) fn same_value_zero(left: &Value, right: &Value) -> bool {
    matches!((left, right), (Value::Number(left), Value::Number(right)) if left.is_nan() && right.is_nan()) || strict(left, right)
}

/// `Number(value)`; an array or object converts through its string, which throws for an own `toString`.
pub(crate) fn number_of(value: &Value, budget: &Budget) -> Result<f64, Error> {
    Ok(match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(value) => f64::from(u8::from(*value)),
        Value::Number(value) => *value,
        Value::Text(text) => risunest_ir::num::to_number(text, budget)?,
        Value::Array(_) | Value::Object(_) => risunest_ir::num::to_number(&value.to_text(budget)?, budget)?,
    })
}

/// `JSON.parse(text)`: `None` for the `SyntaxError` a `try` catches.
/// `parseFloat(text)`: the longest `StrDecimalLiteral` prefix after leading white space.
pub(crate) fn parse_float(source: &[u16]) -> f64 {
    let source = &source[source.iter().position(|unit| !ws(*unit)).unwrap_or(source.len())..];
    let sign = |unit: Option<&u16>| matches!(unit, Some(43 | 45));
    let digit = |unit: Option<&u16>| unit.is_some_and(|unit| (48..=57).contains(unit));
    let mut at = usize::from(sign(source.first()));
    if source[at..].iter().copied().take(8).eq("Infinity".encode_utf16()) {
        return if source.first() == Some(&45) { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let start = at;
    while digit(source.get(at)) { at += 1; }
    let mut digits = at - start;
    if source.get(at) == Some(&46) {
        let mut end = at + 1;
        while digit(source.get(end)) { end += 1; }
        digits += end - at - 1;
        if digits > 0 { at = end; }
    }
    if digits == 0 { return f64::NAN; }
    if matches!(source.get(at), Some(101 | 69)) {
        let mut end = at + 1;
        if sign(source.get(end)) { end += 1; }
        let exponent = end;
        while digit(source.get(end)) { end += 1; }
        if end > exponent { at = end; }
    }
    source[..at].iter().map(|unit| char::from(*unit as u8)).collect::<String>().parse().unwrap_or(f64::NAN)
}

pub(crate) fn parse(source: &Text, budget: &Budget) -> Result<Option<Value>, Error> {
    match risunest_ir::json::parse_json_text(source, budget) {
        Ok(value) => Ok(Some(value)),
        Err(Error::Syntax { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}
