//! The runtime values of `runtime.ts` and the JS conversions the interpreter applies to them.
//!
//! Each variant stands for a JS runtime class; `.value` is the payload. Arrays and objects are never mutated after
//! creation, so they are shared through `Rc` and `Rc::ptr_eq` is JS object identity. Namespaces are mutable through
//! `set ns.attr = ...`, so every alias sees the write.

use crate::ast::{Node, Special};
use crate::error::{runtime, too_deep, JinjaError};
use crate::units::{self, s, Str};
use risunest_ir::Budget;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub(crate) enum V<'a> {
    Int(f64),
    Float(f64),
    Str(Str),
    Bool(bool),
    Null,
    Undefined,
    Array(Rc<Vec<V<'a>>>),
    /// `TupleValue`, an `ArrayValue` subclass with its own `type`.
    Tuple(Rc<Vec<V<'a>>>),
    Object(Rc<Map<'a>>),
    /// `KeywordArgumentsValue`, an `ObjectValue` subclass with its own `type`.
    Kwargs(Rc<Map<'a>>),
    Namespace(Rc<RefCell<Map<'a>>>),
    Func(Rc<Func<'a>>),
}

/// Values nest as deep as a template builds them (`{% set a = [a] %}` in a loop), so the last owner of a container
/// takes its children apart iteratively instead of dropping them recursively.
impl Drop for V<'_> {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        take_children(self, &mut pending);
        while let Some(mut value) = pending.pop() { take_children(&mut value, &mut pending); }
    }
}

fn take_children<'a>(value: &mut V<'a>, out: &mut Vec<V<'a>>) {
    let drain = |map: &mut Map<'a>, out: &mut Vec<V<'a>>| out.extend(map.entries.drain(..).map(|(_, value)| value));
    match value {
        V::Array(items) | V::Tuple(items) => if let Some(items) = Rc::get_mut(items) { out.append(items) },
        V::Object(map) | V::Kwargs(map) => if let Some(map) = Rc::get_mut(map) { drain(map, out) },
        // The interpreter keeps weak handles to namespaces, so the last strong owner drains through the cell.
        V::Namespace(map) => if Rc::strong_count(map) == 1 { if let Ok(mut map) = map.try_borrow_mut() { drain(&mut map, out) } },
        V::Func(function) => if let Some(function) = Rc::get_mut(function) {
            match function {
                Func::Object(_, map) => if let Some(map) = Rc::get_mut(map) { drain(map, out) },
                Func::Caller { env, .. } => if let Some(scope) = Rc::get_mut(env) { out.extend(scope.drain()) },
                _ => {}
            }
        },
        _ => {}
    }
}

/// A JS `Map<string, AnyRuntimeValue>`: insertion order, a repeated `set` keeping the first position.
#[derive(Clone, Default)]
pub(crate) struct Map<'a> {
    pub(crate) entries: Vec<(Str, V<'a>)>,
}

impl<'a> Map<'a> {
    pub(crate) fn get(&self, key: &[u16]) -> Option<&V<'a>> {
        self.entries.iter().find(|(name, _)| name[..] == *key).map(|(_, value)| value)
    }
    pub(crate) fn has(&self, key: &[u16]) -> bool { self.get(key).is_some() }
    pub(crate) fn set(&mut self, key: Str, value: V<'a>) {
        match self.entries.iter_mut().find(|(name, _)| name[..] == key[..]) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }
    pub(crate) fn delete(&mut self, key: &[u16]) {
        if let Some(index) = self.entries.iter().position(|(name, _)| name[..] == *key) { self.entries.remove(index); }
    }
    pub(crate) fn len(&self) -> usize { self.entries.len() }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StrMethod { Upper, Lower, Strip, Title, Capitalize, Rstrip, Lstrip, Startswith, Endswith, Split, Replace }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ObjMethod { Get, Items, Keys, Values, Dictsort }

pub(crate) enum Func<'a> {
    Str(StrMethod, Str),
    Object(ObjMethod, Rc<Map<'a>>),
    Namespace,
    RaiseException,
    Range,
    StrftimeNow,
    /// A `{% macro %}`: it runs in a scope under the caller's scope, as the JS closure receives it.
    Macro { name: Str, args: &'a [Node], body: &'a [Node], special: Special },
    /// The `caller` of a `{% call %}` block, scoped under the block's own environment.
    Caller { args: &'a [Node], body: &'a [Node], special: Special, env: crate::runtime::Env<'a> },
}

impl<'a> V<'a> {
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            V::Int(_) => "IntegerValue",
            V::Float(_) => "FloatValue",
            V::Str(_) => "StringValue",
            V::Bool(_) => "BooleanValue",
            V::Null => "NullValue",
            V::Undefined => "UndefinedValue",
            V::Array(_) => "ArrayValue",
            V::Tuple(_) => "TupleValue",
            V::Object(_) => "ObjectValue",
            V::Kwargs(_) => "KeywordArgumentsValue",
            V::Namespace(_) => "NamespaceValue",
            V::Func(_) => "FunctionValue",
        }
    }
    pub(crate) fn str(value: Vec<u16>, budget: &Budget) -> Result<Self, JinjaError> { Ok(V::Str(units::finish(value, budget)?)) }
    /// `instanceof ArrayValue`.
    pub(crate) fn items(&self) -> Option<&Rc<Vec<V<'a>>>> { match self { V::Array(items) | V::Tuple(items) => Some(items), _ => None } }
    /// `instanceof ObjectValue`.
    pub(crate) fn object(&self) -> Option<&Rc<Map<'a>>> { match self { V::Object(map) | V::Kwargs(map) => Some(map), _ => None } }
    /// `isAttributeContainer`: an object or a namespace.
    pub(crate) fn is_container(&self) -> bool { matches!(self, V::Object(_) | V::Kwargs(_) | V::Namespace(_)) }
    pub(crate) fn number(&self) -> Option<f64> { match self { V::Int(value) | V::Float(value) => Some(*value), _ => None } }
    /// `isNumericLikeValue` with `getNumericValue`.
    pub(crate) fn numeric_like(&self) -> Option<f64> {
        match self { V::Int(value) | V::Float(value) => Some(*value), V::Bool(value) => Some(f64::from(u8::from(*value))), _ => None }
    }

    /// `__bool__()`.
    pub(crate) fn truthy(&self) -> bool {
        match self {
            V::Array(items) | V::Tuple(items) => !items.is_empty(),
            V::Object(map) | V::Kwargs(map) => map.len() > 0,
            other => other.js_truthy(),
        }
    }
    /// `!!this.value`: JS truthiness of the payload, where every array, map and function is truthy.
    pub(crate) fn js_truthy(&self) -> bool {
        match self {
            V::Int(value) | V::Float(value) => *value != 0.0 && !value.is_nan(),
            V::Str(value) => !value.is_empty(),
            V::Bool(value) => *value,
            V::Null | V::Undefined => false,
            _ => true,
        }
    }
}

pub(crate) fn object_value<'a>(map: Map<'a>) -> V<'a> { V::Object(Rc::new(map)) }
pub(crate) fn array_value(items: Vec<V<'_>>) -> V<'_> { V::Array(Rc::new(items)) }

/// `value.toString()` of the runtime value.
pub(crate) fn to_string(value: &V<'_>, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    budget.work(1)?;
    match value {
        V::Int(number) => units::number(*number, budget),
        V::Float(number) => units::float(*number, budget),
        V::Str(text) => { budget.work(text.len())?; Ok(text.to_vec()) }
        V::Bool(flag) => Ok(s(if *flag { "true" } else { "false" })),
        V::Null => Ok(s("null")),
        V::Undefined => Ok(s("undefined")),
        V::Array(_) | V::Tuple(_) | V::Object(_) | V::Kwargs(_) | V::Namespace(_) => to_json(value, &JsonOptions::default(), 0, false, budget),
        // String(fn) is the closure's JS source text.
        V::Func(_) => Err(JinjaError::Unsupported("function-to-string")),
    }
}

/// `String(value.value)`: the JS string conversion of the payload. An array joins its elements' runtime strings
/// with commas, and every map is `[object Map]`.
pub(crate) fn payload_string(value: &V<'_>, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    budget.work(1)?;
    match value {
        V::Int(number) | V::Float(number) => units::number(*number, budget),
        V::Array(items) | V::Tuple(items) => {
            let parts = items.iter().map(|item| to_string(item, budget)).collect::<Result<Vec<_>, _>>()?;
            units::join(&parts, &s(","), budget)
        }
        V::Object(_) | V::Kwargs(_) | V::Namespace(_) => Ok(s("[object Map]")),
        other => to_string(other, budget),
    }
}

/// `Array.prototype.join` of payloads: `null` and `undefined` payloads are empty.
pub(crate) fn join_payload(value: &V<'_>, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    match value {
        V::Null | V::Undefined => Ok(Vec::new()),
        other => payload_string(other, budget),
    }
}

#[derive(Default)]
pub(crate) struct JsonOptions {
    /// The `indent` payload: a number, or `None` for null.
    pub(crate) indent: Option<f64>,
    pub(crate) ensure_ascii: bool,
    pub(crate) separators: Option<(Vec<u16>, Vec<u16>)>,
    pub(crate) sort_keys: bool,
}

fn ascii_safe(json: Vec<u16>) -> Vec<u16> {
    // /[\x7f-￿]/g on the JSON text, one UTF-16 unit at a time.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(json.len());
    for unit in json {
        if unit >= 0x7f {
            out.extend([0x5c, 0x75]);
            for shift in [12, 8, 4, 0] { out.push(HEX[((unit >> shift) & 0xf) as usize] as u16); }
        } else {
            out.push(unit);
        }
    }
    out
}

fn json_string(text: &[u16], ensure_ascii: bool, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    let mut out = Vec::with_capacity(text.len() + 2);
    units::json_quote(text, &mut out, budget)?;
    if ensure_ascii { budget.work(out.len())?; out = ascii_safe(out); }
    Ok(out)
}

fn json_number(number: f64, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    if number.is_finite() { units::number(number, budget) } else { Ok(s("null")) }
}

/// `String.prototype.localeCompare` under the root collation, for keys made of `a-z`, `0-9` and `_`, where
/// punctuation sorts before digits and digits before letters. Other keys decline.
fn collation_key(key: &[u16]) -> Option<Vec<u8>> {
    key.iter().map(|unit| match *unit {
        0x5f => Some(0),
        0x30..=0x39 => Some(1 + (*unit - 0x30) as u8),
        0x61..=0x7a => Some(11 + (*unit - 0x61) as u8),
        _ => None,
    }).collect()
}

/// `toJSON(input, options, depth, convertUndefinedToNull)`.
pub(crate) fn to_json(input: &V<'_>, options: &JsonOptions, depth: usize, undefined_to_null: bool, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    budget.work(1)?;
    let indent = options.indent.filter(|indent| *indent != 0.0 && !indent.is_nan());
    let (item_separator, key_separator) = match (&options.separators, indent) {
        (Some((item, key)), _) => (item.clone(), key.clone()),
        (None, Some(_)) => (s(","), s(": ")),
        (None, None) => (s(", "), s(": ")),
    };
    match input {
        V::Null => Ok(s("null")),
        V::Undefined => Ok(s(if undefined_to_null { "null" } else { "undefined" })),
        V::Int(number) | V::Float(number) => json_number(*number, budget),
        V::Bool(flag) => Ok(s(if *flag { "true" } else { "false" })),
        V::Str(text) => json_string(text, options.ensure_ascii, budget),
        V::Array(_) | V::Namespace(_) | V::Object(_) => {
            // JS recurses until its stack runs out, which a namespace that holds itself reaches.
            if depth >= budget.limits().max_depth { return Err(too_deep()); }
            let indent_value = match indent {
                Some(count) => {
                    // " ".repeat(n) throws a RangeError for a negative or infinite count.
                    if count < 0.0 || count.is_infinite() { return Err(runtime("Invalid count value")); }
                    let count = count.trunc() as usize;
                    budget.work(count)?;
                    vec![0x20; count]
                }
                None => Vec::new(),
            };
            let mut base_padding = vec![0x0a];
            budget.work(indent_value.len().saturating_mul(depth))?;
            for _ in 0..depth { base_padding.extend_from_slice(&indent_value); }
            let mut children_padding = base_padding.clone();
            children_padding.extend_from_slice(&indent_value);
            if let V::Array(items) = input {
                let core = items.iter().map(|item| to_json(item, options, depth + 1, undefined_to_null, budget)).collect::<Result<Vec<_>, _>>()?;
                return if indent.is_some() {
                    let separator = units::concat(&[&item_separator, &children_padding], budget)?;
                    let joined = units::join(&core, &separator, budget)?;
                    units::concat(&[&s("["), &children_padding, &joined, &base_padding, &s("]")], budget)
                } else {
                    let joined = units::join(&core, &item_separator, budget)?;
                    units::concat(&[&s("["), &joined, &s("]")], budget)
                };
            }
            let mut entries: Vec<(Str, V<'_>)> = match input {
                V::Namespace(map) => map.borrow().entries.clone(),
                V::Object(map) => map.entries.clone(),
                _ => unreachable!(),
            };
            if options.sort_keys && entries.len() > 1 {
                let mut keyed = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    budget.work(key.len())?;
                    let Some(collation) = collation_key(&key) else { return Err(JinjaError::Unsupported("tojson-sort-keys-collation")) };
                    keyed.push((collation, key, value));
                }
                budget.work(keyed.len().saturating_mul(keyed.len().max(1).ilog2() as usize + 1))?;
                keyed.sort_by(|a, b| a.0.cmp(&b.0));
                entries = keyed.into_iter().map(|(_, key, value)| (key, value)).collect();
            }
            let mut core = Vec::with_capacity(entries.len());
            for (key, value) in &entries {
                let key = json_string(key, options.ensure_ascii, budget)?;
                let value = to_json(value, options, depth + 1, undefined_to_null, budget)?;
                let entry = units::concat(&[&key, &key_separator, &value], budget)?;
                core.push(if indent.is_some() { units::concat(&[&children_padding, &entry], budget)? } else { entry });
            }
            let joined = units::join(&core, &item_separator, budget)?;
            if indent.is_some() { units::concat(&[&s("{"), &joined, &base_padding, &s("}")], budget) } else { units::concat(&[&s("{"), &joined, &s("}")], budget) }
        }
        // TupleValue and KeywordArgumentsValue have their own `type`, which the switch does not list.
        V::Tuple(_) | V::Kwargs(_) | V::Func(_) => Err(runtime(format!("Cannot convert to JSON: {}", input.type_name()))),
    }
}

/// A JS primitive or object identity, the operand of `==`, `===` and SameValueZero on payloads.
enum Payload<'v, 'a> {
    Number(f64),
    Str(&'v [u16]),
    Bool(bool),
    Null,
    Undefined,
    Object(&'v V<'a>),
}

fn payload<'v, 'a>(value: &'v V<'a>) -> Payload<'v, 'a> {
    match value {
        V::Int(number) | V::Float(number) => Payload::Number(*number),
        V::Str(text) => Payload::Str(text),
        V::Bool(flag) => Payload::Bool(*flag),
        V::Null => Payload::Null,
        V::Undefined => Payload::Undefined,
        other => Payload::Object(other),
    }
}

fn same_object<'a>(left: &V<'a>, right: &V<'a>) -> Result<bool, JinjaError> {
    Ok(match (left, right) {
        (V::Array(a) | V::Tuple(a), V::Array(b) | V::Tuple(b)) => Rc::ptr_eq(a, b),
        (V::Object(a) | V::Kwargs(a), V::Object(b) | V::Kwargs(b)) => Rc::ptr_eq(a, b),
        (V::Namespace(a), V::Namespace(b)) => Rc::ptr_eq(a, b),
        // JS caches a bound builtin per receiver, which this port does not model.
        (V::Func(_), V::Func(_)) => return Err(JinjaError::Unsupported("function-identity")),
        _ => false,
    })
}

/// `a.value === b.value`.
pub(crate) fn strict_equals<'a>(left: &V<'a>, right: &V<'a>) -> Result<bool, JinjaError> {
    Ok(match (payload(left), payload(right)) {
        (Payload::Number(a), Payload::Number(b)) => a == b,
        (Payload::Str(a), Payload::Str(b)) => a == b,
        (Payload::Bool(a), Payload::Bool(b)) => a == b,
        (Payload::Null, Payload::Null) | (Payload::Undefined, Payload::Undefined) => true,
        (Payload::Object(a), Payload::Object(b)) => same_object(a, b)?,
        _ => false,
    })
}

/// SameValueZero on payloads, as a `Set` compares them.
pub(crate) fn same_value_zero<'a>(left: &V<'a>, right: &V<'a>) -> Result<bool, JinjaError> {
    if let (Payload::Number(a), Payload::Number(b)) = (payload(left), payload(right)) { return Ok(a == b || (a.is_nan() && b.is_nan())); }
    strict_equals(left, right)
}

/// `a.value == b.value`, with JS loose equality.
pub(crate) fn loose_equals<'a>(left: &V<'a>, right: &V<'a>, budget: &Budget) -> Result<bool, JinjaError> {
    budget.work(1)?;
    // ToPrimitive of an object operand: arrays join, maps are "[object Map]", functions decline.
    fn primitive(value: &V<'_>, budget: &Budget) -> Result<Vec<u16>, JinjaError> { payload_string(value, budget) }
    fn number_of(value: &Payload<'_, '_>, budget: &Budget) -> Result<f64, JinjaError> {
        match value {
            Payload::Number(number) => Ok(*number),
            Payload::Bool(flag) => Ok(f64::from(u8::from(*flag))),
            Payload::Str(text) => units::string_to_number(text, budget),
            _ => Ok(f64::NAN),
        }
    }
    let (a, b) = (payload(left), payload(right));
    Ok(match (&a, &b) {
        (Payload::Null | Payload::Undefined, Payload::Null | Payload::Undefined) => true,
        (Payload::Null | Payload::Undefined, _) | (_, Payload::Null | Payload::Undefined) => false,
        (Payload::Number(x), Payload::Number(y)) => x == y,
        (Payload::Str(x), Payload::Str(y)) => x == y,
        (Payload::Bool(x), Payload::Bool(y)) => x == y,
        (Payload::Object(x), Payload::Object(y)) => same_object(x, y)?,
        (Payload::Object(object), Payload::Str(text)) | (Payload::Str(text), Payload::Object(object)) => {
            if matches!(object, V::Func(_)) { return Err(JinjaError::Unsupported("function-to-string")); }
            primitive(object, budget)?[..] == **text
        }
        (Payload::Object(object), other) | (other, Payload::Object(object)) => {
            // A function's source text is never numeric, so it equals no number or boolean.
            if matches!(object, V::Func(_)) { return Ok(false); }
            let text = primitive(object, budget)?;
            units::string_to_number(&text, budget)? == number_of(other, budget)?
        }
        (x, y) => number_of(x, budget)? == number_of(y, budget)?,
    })
}
