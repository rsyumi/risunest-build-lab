//! JavaScript object and string semantics the OpenAI-compatible path depends on.

use risunest_ir::{Budget, Error, Text, TextBuilder, Value};

pub(crate) fn text(value: &str, budget: &Budget) -> Result<Text, Error> { Text::from_str(value, budget) }

/// The JSON-shaped tree request bodies are built in. It is a plain `Vec` tree because bodies delete and reorder keys.
#[derive(Debug)]
pub(crate) enum J { Undefined, Null, Bool(bool), Num(f64), Str(Text), Arr(Vec<J>), Obj(Vec<(Text, J)>) }

pub(crate) fn new_object() -> J { J::Obj(Vec::new()) }

pub(crate) fn str_value(value: &str, budget: &Budget) -> Result<J, Error> { Ok(J::Str(Text::from_str(value, budget)?)) }

/// A property name that JavaScript enumerates as an array index (`0` to `2^32 - 2`, canonical decimal).
pub(crate) fn array_index(key: &Text) -> Option<u32> {
    let length = key.len_units();
    if length == 0 || length > 10 { return None; }
    let mut units = key.units();
    let first = units.next()?;
    if first == 48 { return (length == 1).then_some(0); }
    if !(49..=57).contains(&first) { return None; }
    let mut value = (first - 48) as u32;
    for unit in units {
        if !(48..=57).contains(&unit) { return None; }
        value = value.checked_mul(10)?.checked_add((unit - 48) as u32)?;
    }
    (value < u32::MAX).then_some(value)
}

pub(crate) fn find(entries: &[(Text, J)], key: &Text) -> Option<usize> { entries.iter().position(|(name, _)| name.equals(key)) }

/// `object[key] = value`: replaces in place, otherwise appends, with array-index keys enumerated first in ascending order.
pub(crate) fn set(object: &mut J, key: Text, value: J, budget: &Budget) -> Result<(), Error> {
    let J::Obj(entries) = object else { return Err(Error::Type { operation: "set-property" }) };
    budget.work(entries.len().saturating_add(1))?;
    if let Some(at) = find(entries, &key) { entries[at].1 = value; return Ok(()); }
    let mut position = entries.len();
    if let Some(index) = array_index(&key) {
        for (at, (name, _)) in entries.iter().enumerate() {
            if array_index(name).is_none_or(|old| old > index) { position = at; break; }
        }
    }
    entries.insert(position, (key, value));
    Ok(())
}

pub(crate) fn set_str(object: &mut J, key: &str, value: J, budget: &Budget) -> Result<(), Error> {
    set(object, text(key, budget)?, value, budget)
}

pub(crate) fn delete(object: &mut J, key: &str) {
    if let J::Obj(entries) = object {
        if let Some(at) = entries.iter().position(|(name, _)| name.equals_ascii(key.as_bytes())) { entries.remove(at); }
    }
}

pub(crate) fn delete_text(object: &mut J, key: &Text) {
    if let J::Obj(entries) = object {
        if let Some(at) = find(entries, key) { entries.remove(at); }
    }
}

pub(crate) fn get<'a>(object: &'a J, key: &str) -> Option<&'a J> {
    match object { J::Obj(entries) => entries.iter().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value), _ => None }
}

pub(crate) fn get_mut<'a>(object: &'a mut J, key: &str) -> Option<&'a mut J> {
    match object { J::Obj(entries) => entries.iter_mut().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value), _ => None }
}

/// `value` with JavaScript truthiness.
pub(crate) fn truthy(value: &J) -> bool {
    match value {
        J::Undefined | J::Null => false,
        J::Bool(flag) => *flag,
        J::Num(number) => *number != 0.0 && !number.is_nan(),
        J::Str(text) => text.len_units() != 0,
        J::Arr(_) | J::Obj(_) => true,
    }
}

/// Copies an `ir::Value` (JSON-shaped data from a parser or a caller) into the request tree.
pub(crate) fn from_value(value: &Value, budget: &Budget) -> Result<J, Error> {
    budget.work(1)?;
    Ok(match value {
        Value::Undefined => J::Undefined,
        Value::Null => J::Null,
        Value::Bool(flag) => J::Bool(*flag),
        Value::Number(number) => J::Num(*number),
        Value::Text(text) => J::Str(text.copy(budget)?),
        Value::Array(items) => J::Arr(items.as_slice().iter().map(|item| from_value(item, budget)).collect::<Result<_, _>>()?),
        Value::Object(entries) => J::Obj(entries.as_slice().iter().map(|(key, item)| Ok((key.copy(budget)?, from_value(item, budget)?))).collect::<Result<_, Error>>()?),
    })
}

/// A deep copy.
pub(crate) fn clone_j(value: &J, budget: &Budget) -> Result<J, Error> {
    budget.work(1)?;
    Ok(match value {
        J::Undefined => J::Undefined,
        J::Null => J::Null,
        J::Bool(flag) => J::Bool(*flag),
        J::Num(number) => J::Num(*number),
        J::Str(text) => J::Str(text.copy(budget)?),
        J::Arr(items) => J::Arr(items.iter().map(|item| clone_j(item, budget)).collect::<Result<_, _>>()?),
        J::Obj(entries) => J::Obj(entries.iter().map(|(key, item)| Ok((key.copy(budget)?, clone_j(item, budget)?))).collect::<Result<_, Error>>()?),
    })
}

/// What `JSON.parse(JSON.stringify(value))` leaves of a parsed value: a number that is not finite becomes `null` and
/// negative zero becomes `0`.
pub(crate) fn json_round_trip(value: J) -> J {
    match value {
        J::Num(number) if !number.is_finite() => J::Null,
        J::Num(0.0) => J::Num(0.0),
        J::Arr(items) => J::Arr(items.into_iter().map(json_round_trip).collect()),
        J::Obj(entries) => J::Obj(entries.into_iter().map(|(key, item)| (key, json_round_trip(item))).collect()),
        other => other,
    }
}

/// Names a plain object inherits from `Object.prototype`, plus the key the stream decoder keeps its tool calls under.
/// Assigning through one of them does not behave like a plain property, so the decoder does not model it.
pub(crate) fn reserved_object_key(key: &Text) -> bool {
    [
        "__proto__", "__tool_calls", "constructor", "hasOwnProperty", "isPrototypeOf", "propertyIsEnumerable", "toLocaleString",
        "toString", "valueOf", "__defineGetter__", "__defineSetter__", "__lookupGetter__", "__lookupSetter__",
    ].iter().any(|name| key.equals_ascii(name.as_bytes()))
}

pub(crate) fn concat(left: &Text, right: &Text, budget: &Budget) -> Result<Text, Error> {
    let mut output = left.copy(budget)?;
    output.append(right)?;
    Ok(output)
}

pub(crate) fn join(parts: &[&Text], separator: &str, budget: &Budget) -> Result<Text, Error> {
    let separator = text(separator, budget)?;
    let mut output = text("", budget)?;
    for (index, part) in parts.iter().enumerate() {
        if index != 0 { output.append(&separator)?; }
        output.append(part)?;
    }
    Ok(output)
}

pub(crate) fn starts_with(value: &Text, prefix: &str) -> bool {
    let mut units = value.units();
    prefix.encode_utf16().all(|unit| units.next() == Some(unit))
}

pub(crate) fn ends_with(value: &Text, suffix: &str) -> bool {
    let suffix: Vec<u16> = suffix.encode_utf16().collect();
    let length = value.len_units();
    length >= suffix.len() && value.units().skip(length - suffix.len()).eq(suffix.iter().copied())
}

pub(crate) fn units(value: &Text) -> Vec<u16> { value.units().collect() }

pub(crate) fn from_units(value: &[u16], budget: &Budget) -> Result<Text, Error> { Text::from_units(value, budget) }

/// Index of the first occurrence of `needle` at or after `from`, in UTF-16 units.
pub(crate) fn find_units(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() { return (from <= haystack.len()).then_some(from); }
    if haystack.len() < needle.len() { return None; }
    (from..=haystack.len() - needle.len()).find(|&at| &haystack[at..at + needle.len()] == needle)
}

pub(crate) fn utf16(value: &str) -> Vec<u16> { value.encode_utf16().collect() }

pub(crate) fn text_from_slice(value: &[u16], budget: &Budget) -> Result<Text, Error> {
    let mut builder = TextBuilder::new(budget)?;
    for &unit in value { builder.unit(unit)?; }
    builder.finish()
}

/// `String.prototype.replace(pattern, replacement)` for a string pattern: first occurrence, with the `$` patterns.
pub(crate) fn replace_first(value: &Text, pattern: &str, replacement: &Text, budget: &Budget) -> Result<Text, Error> {
    let haystack = units(value);
    let needle = utf16(pattern);
    let Some(at) = find_units(&haystack, &needle, 0) else { return value.copy(budget) };
    let replacement = units(replacement);
    let mut output: Vec<u16> = haystack[..at].to_vec();
    let mut index = 0;
    while index < replacement.len() {
        let unit = replacement[index];
        if unit == 36 && index + 1 < replacement.len() {
            match replacement[index + 1] {
                36 => { output.push(36); index += 2; continue; }
                38 => { output.extend_from_slice(&needle); index += 2; continue; }
                96 => { output.extend_from_slice(&haystack[..at]); index += 2; continue; }
                39 => { output.extend_from_slice(&haystack[at + needle.len()..]); index += 2; continue; }
                _ => {}
            }
        }
        output.push(unit);
        index += 1;
    }
    output.extend_from_slice(&haystack[at + needle.len()..]);
    text_from_slice(&output, budget)
}

fn is_js_space(unit: u16) -> bool { risunest_ir::text::is_whitespace(unit) }

/// `parseInt(string)` with no radix; `None` is NaN.
pub(crate) fn parse_int(value: &Text) -> Option<f64> {
    let all = units(value);
    let mut index = 0;
    while index < all.len() && is_js_space(all[index]) { index += 1; }
    let mut sign = 1.0;
    if index < all.len() && (all[index] == 43 || all[index] == 45) { if all[index] == 45 { sign = -1.0; } index += 1; }
    let mut radix = 10;
    if index + 1 < all.len() && all[index] == 48 && (all[index + 1] == 120 || all[index + 1] == 88) { radix = 16; index += 2; }
    let start = index;
    let mut number = 0.0f64;
    while index < all.len() {
        let digit = if all[index] < 128 { (all[index] as u8 as char).to_digit(radix) } else { None };
        let Some(digit) = digit else { break };
        number = number * radix as f64 + digit as f64;
        index += 1;
    }
    (index > start).then_some(sign * number)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use risunest_ir::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    pub(crate) fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 28, max_text_units: 1 << 28, max_values: 1 << 26, max_depth: 100, max_work: 1 << 40 }, Arc::new(AtomicBool::new(false))).unwrap()
    }

    fn number(source: &str) -> f64 { risunest_ir::num::to_number(&Text::from_str(source, &budget()).unwrap(), &budget()).unwrap() }

    #[test]
    fn number_conversion_follows_the_language() {
        assert_eq!(number(" 12 "), 12.0);
        assert_eq!(number(""), 0.0);
        assert_eq!(number("0x1F"), 31.0);
        assert_eq!(number("1e3"), 1000.0);
        assert_eq!(number("-.5"), -0.5);
        assert_eq!(number("-Infinity"), f64::NEG_INFINITY);
        assert!(number("-0x10").is_nan());
        assert!(number("1_0").is_nan());
        assert!(number("abc").is_nan());
        assert!(number("1e").is_nan());
        assert!(number(".").is_nan());
    }

    #[test]
    fn index_keys_enumerate_first() {
        let budget = budget();
        let mut object = new_object();
        for key in ["b", "20", "3", "a"] { set_str(&mut object, key, J::Null, &budget).unwrap(); }
        let J::Obj(entries) = &object else { unreachable!() };
        let keys: Vec<String> = entries.iter().map(|(key, _)| key.as_unicode().unwrap().to_string()).collect();
        assert_eq!(keys, ["3", "20", "b", "a"]);
    }

    #[test]
    fn replacement_patterns_expand_for_a_string_pattern() {
        let budget = budget();
        let value = Text::from_str("a{{slot}}b", &budget).unwrap();
        let replacement = Text::from_str("[$&|$$|$`|$']", &budget).unwrap();
        let output = replace_first(&value, "{{slot}}", &replacement, &budget).unwrap();
        assert_eq!(output.as_unicode().unwrap(), "a[{{slot}}|$|a|b]b");
    }
}
