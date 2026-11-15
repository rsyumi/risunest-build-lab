// Runtime registration table of src/ts/cbs.ts and the matcher() dispatch of parser.svelte.ts.
// Native entries reproduce the TypeScript callbacks; snapshot entries serve host reads from the
// invocation Context; host entries read the captured database and are refused without one; every other
// runtime key is refused so the caller falls back.
use super::host;
use super::parser::{ArgState, CaseTables, Options};
use super::{
    aggregates, array_ops, calc, deterministic_hash, json_depth_exceeds, json_parse, json_string, number_string,
    replace_all, replace_literal, scalar, split, split_slices, text, trim, units, ws, Budget, Context, Json, Units,
};
use risunest_ir::rng::Rng;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::OnceLock;

pub(super) enum Out {
    Text(Units),
    // matcher() returned null or the callback threw: the parser keeps `{{dat}}`.
    Preserve,
    // A {text, var} result: the parser checks the forced return after appending it.
    Object(Units),
}

// What getChatVar returns. Through a trigger scope a local or pending write keeps a non-string as it was written.
pub(super) enum ChatVar {
    Text(Units),
    // Its `!value`, and whether it is an object whose `var` is neither null nor undefined.
    Other { truthy: bool, holds_var: bool },
}

pub(super) trait Scope {
    fn get(&mut self, key: Option<&[u16]>) -> Result<ChatVar, String>;
    // `Number(value)` of the value `get` reads, which only a function that adds to it converts; `None` when the
    // conversion throws.
    fn number(&mut self, key: Option<&[u16]>) -> Result<Option<f64>, String>;
    fn set(&mut self, key: Option<&[u16]>, value: Option<&[u16]>) -> Result<(), String>;
    fn trigger_id(&self) -> Option<Units>;
}

pub(super) struct Env<'a> {
    pub context: &'a mut Context,
    pub budget: &'a mut Budget,
    pub options: Options,
    // Math.random; without one the verification LCG in the context draws.
    pub rng: Option<&'a mut dyn Rng>,
    pub scope: Option<&'a mut dyn Scope>,
}
impl Env<'_> {
    pub fn chat_var(&mut self, key: Option<&[u16]>) -> Result<ChatVar, String> {
        match self.scope.as_deref_mut() {
            Some(scope) => scope.get(key),
            None => chat_var(self.context, key).map(ChatVar::Text),
        }
    }
    pub fn chat_var_number(&mut self, key: Option<&[u16]>) -> Result<Option<f64>, String> {
        match self.scope.as_deref_mut() {
            Some(scope) => scope.number(key),
            None => {
                let value = chat_var(self.context, key)?;
                scalar::number(Some(&value), Some(&mut *self.budget)).map(Some)
            }
        }
    }
}

// Own property names of Object, Array, String, Number and Boolean.prototype, where a product lookup reads an
// engine value; the engine refuses them. Each list is the union of Object.getOwnPropertyNames on the oracle
// page (Chromium 153's V8 with the core-js/actual polyfills the app also loads) and the string keys ECMA-262
// 2025 and its Annex B define on that prototype (the ES2026 draft and finished proposals add none), so an
// engine that lacks a name, or has a standard name V8 lacks, still falls back.
pub(super) const OBJECT_PROTO: &[&str] = &[
    "__defineGetter__", "__defineSetter__", "__lookupGetter__", "__lookupSetter__", "__proto__", "constructor",
    "hasOwnProperty", "isPrototypeOf", "propertyIsEnumerable", "toLocaleString", "toString", "valueOf",
];
pub(super) const ARRAY_PROTO: &[&str] = &[
    "at", "concat", "constructor", "copyWithin", "entries", "every", "fill", "filter", "find", "findIndex", "findLast",
    "findLastIndex", "flat", "flatMap", "forEach", "group", "groupBy", "groupByToMap", "groupToMap", "includes",
    "indexOf", "join", "keys", "lastIndexOf", "length", "map", "pop", "push", "reduce", "reduceRight", "reverse",
    "shift", "slice", "some", "sort", "splice", "toLocaleString", "toReversed", "toSorted", "toSpliced", "toString",
    "unshift", "values", "with",
];
pub(super) const STRING_PROTO: &[&str] = &[
    "anchor", "at", "big", "blink", "bold", "charAt", "charCodeAt", "codePointAt", "concat", "constructor", "endsWith",
    "fixed", "fontcolor", "fontsize", "includes", "indexOf", "isWellFormed", "italics", "lastIndexOf", "length", "link",
    "localeCompare", "match", "matchAll", "normalize", "padEnd", "padStart", "repeat", "replace", "replaceAll", "search",
    "slice", "small", "split", "startsWith", "strike", "sub", "substr", "substring", "sup", "toLocaleLowerCase",
    "toLocaleUpperCase", "toLowerCase", "toString", "toUpperCase", "toWellFormed", "trim", "trimEnd", "trimLeft",
    "trimRight", "trimStart", "valueOf",
];
pub(super) const NUMBER_PROTO: &[&str] =
    &["constructor", "toExponential", "toFixed", "toLocaleString", "toPrecision", "toString", "valueOf"];
pub(super) const BOOLEAN_PROTO: &[&str] = &["constructor", "toString", "valueOf"];

// The names are ASCII, so a name matches only a key of its byte length.
fn listed(list: &[&str], key: &[u16]) -> bool {
    list.iter().any(|name| name.len() == key.len() && name.encode_utf16().eq(key.iter().copied()))
}
const UNDEFINED: &[u16] = &[117, 110, 100, 101, 102, 105, 110, 101, 100];

const FORCE_RETURN: &str = "__force_return__";
const RETURN: &str = "__return__";

// The invocation-local tempVar object; a value of None is a property holding undefined.
#[derive(Default)]
pub(super) struct Temp(HashMap<Units, Option<Units>>);
impl Temp {
    fn key(key: &[u16]) -> Result<Units, String> {
        if listed(OBJECT_PROTO, key) {
            return Err("temp-prototype-key".into());
        }
        Ok(key.to_vec())
    }
    // tempVar['__force_return__'] is truthy: the parser returns tempVar['__return__'] ?? 'null'.
    pub fn forced_return(&self) -> Option<Units> {
        match self.0.get(&units(FORCE_RETURN)) {
            Some(Some(v)) if !v.is_empty() => Some(self.0.get(&units(RETURN)).cloned().flatten().unwrap_or_else(|| units("null"))),
            _ => None,
        }
    }
}

pub(super) struct Call<'a, 'e> {
    pub dat: &'a [u16],
    pub args: &'a [Units],
    pub env: &'a mut Env<'e>,
    pub temp: &'a mut Temp,
    // nested[0] at call time, which bkspc and erase rewrite.
    pub root: &'a mut Units,
    // callStack and functions of the parse's matcherArg, which a callback's risuChatParser call reuses.
    pub matcher: &'a mut ArgState,
}
impl Call<'_, '_> {
    fn arg(&self, i: usize) -> Option<&Units> {
        self.args.get(i)
    }
    // Template literals and property keys render undefined as "undefined".
    fn shown(&self, i: usize) -> Units {
        self.args.get(i).cloned().unwrap_or_else(|| units("undefined"))
    }
    pub(super) fn number(&mut self, i: usize) -> Result<f64, String> {
        scalar::number(self.args.get(i), Some(self.env.budget))
    }
}

type Callback = fn(&mut Call<'_, '_>) -> Result<Out, String>;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Status {
    Native,
    Snapshot,
    // Served only from a captured database.
    Host,
    Refused,
}
pub(super) struct Entry {
    pub id: &'static str,
    pub keys: &'static [&'static str],
    pub status: Status,
    pub call: Option<Callback>,
    // Stateless natives: a literal macro has one value regardless of context, temps or output.
    pub foldable: bool,
}

fn ok(v: Units) -> Result<Out, String> {
    Ok(Out::Text(v))
}
fn s(v: &str) -> Result<Out, String> {
    Ok(Out::Text(units(v)))
}
fn bit(v: bool) -> Result<Out, String> {
    s(if v { "1" } else { "0" })
}
fn cat(parts: &[&[u16]]) -> Units {
    parts.concat()
}

pub(super) fn parse_json(s: &[u16]) -> Result<Option<Json>, String> {
    if json_depth_exceeds(s, 64) {
        return Err("json-depth".into());
    }
    Ok(json_parse(s))
}
// parseArray: a JSON array, otherwise the text split on '§'.
pub(super) fn parse_array(s: &[u16]) -> Result<Vec<Json>, String> {
    Ok(match parse_json(s)? {
        Some(Json::Arr(a)) => a,
        _ => split(s, &[167]).into_iter().map(Json::Str).collect(),
    })
}
// parseDict: JSON.parse, or {} when it throws.
fn parse_dict(s: &[u16]) -> Result<Json, String> {
    Ok(parse_json(s)?.unwrap_or(Json::Obj(Vec::new())))
}
fn make_array(values: Vec<Json>) -> Units {
    array_ops::make_array(values)
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Num(n) => *n != 0.0 && !n.is_nan(),
        Json::Str(s) => !s.is_empty(),
        Json::Arr(_) | Json::Obj(_) => true,
    }
}
// typeof element === 'object' ? JSON.stringify(element) : String(element), with null already 'null'.
fn element_text(v: &Json) -> Units {
    match v {
        Json::Arr(_) | Json::Obj(_) => json_string(v),
        Json::Null => units("null"),
        Json::Str(s) => s.clone(),
        Json::Num(n) => number_string(*n),
        Json::Bool(b) => units(if *b { "true" } else { "false" }),
    }
}
// Canonical array index ("0" or a decimal without leading zeros below 2^32 - 1).
fn index_key(key: &[u16]) -> Option<usize> {
    if key.is_empty() || key.len() > 10 || !key.iter().all(|c| (48..=57).contains(c)) || (key.len() > 1 && key[0] == 48) {
        return None;
    }
    let n: u64 = key.iter().fold(0, |n, &c| n * 10 + u64::from(c - 48));
    (n < 4_294_967_295).then_some(n as usize)
}
enum Lookup {
    Value(Json),
    Undefined,
    // The key resolves on a prototype: a function or the prototype object itself.
    Proto,
}
fn lookup(target: &Json, key: &[u16]) -> Lookup {
    let length = units("length");
    let (own, list): (Option<Json>, &[&str]) = match target {
        Json::Obj(fields) => (fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()), &[]),
        Json::Arr(a) => (
            if key == length { Some(Json::Num(a.len() as f64)) } else { index_key(key).and_then(|i| a.get(i).cloned()) },
            ARRAY_PROTO,
        ),
        Json::Str(s) => (
            if key == length { Some(Json::Num(s.len() as f64)) } else { index_key(key).and_then(|i| s.get(i).map(|&c| Json::Str(vec![c]))) },
            STRING_PROTO,
        ),
        Json::Num(_) => (None, NUMBER_PROTO),
        Json::Bool(_) => (None, BOOLEAN_PROTO),
        Json::Null => (None, &[]),
    };
    match own {
        Some(v) => Lookup::Value(v),
        None if listed(OBJECT_PROTO, key) || listed(list, key) => Lookup::Proto,
        None => Lookup::Undefined,
    }
}
// String(value) for parsed JSON; None where ToPrimitive throws (an own non-callable toString).
fn js_string_of(v: &Json, budget: &mut Budget) -> Result<Option<Units>, String> {
    let mut out = Vec::new();
    Ok(aggregates::append_string(v, &mut out, budget)?.then_some(out))
}
fn join_values(values: &[Json], sep: &[u16], budget: &mut Budget) -> Result<Option<Units>, String> {
    let mut out = Vec::new();
    for (i, v) in values.iter().enumerate() {
        budget.tick()?;
        if i > 0 {
            if sep.len() > budget.max_output.saturating_sub(out.len()) {
                return Err("output-limit".into());
            }
            out.extend_from_slice(sep);
        }
        if !matches!(v, Json::Null) && !aggregates::append_string(v, &mut out, budget)? {
            return Ok(None);
        }
    }
    Ok(Some(out))
}
fn strict_equal(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::Num(x), Json::Num(y)) => x == y,
        (Json::Str(x), Json::Str(y)) => x == y,
        _ => false,
    }
}
fn output_guard(len: usize, budget: &Budget) -> Result<(), String> {
    if len > budget.max_output {
        return Err("output-limit".into());
    }
    Ok(())
}
fn js_number(v: &Json, budget: &mut Budget) -> Result<Option<f64>, String> {
    aggregates::json_number(v, budget)
}

// --- callbacks -------------------------------------------------------------

fn tempvar(c: &mut Call) -> Result<Out, String> {
    let key = Temp::key(&c.shown(0))?;
    Ok(Out::Object(c.temp.0.get(&key).cloned().flatten().unwrap_or_default()))
}
fn settempvar(c: &mut Call) -> Result<Out, String> {
    let key = Temp::key(&c.shown(0))?;
    let value = c.arg(1).cloned();
    c.temp.0.insert(key, value);
    Ok(Out::Object(Vec::new()))
}
fn return_(c: &mut Call) -> Result<Out, String> {
    let value = c.arg(0).cloned();
    c.temp.0.insert(units(RETURN), value);
    c.temp.0.insert(units(FORCE_RETURN), Some(units("1")));
    Ok(Out::Object(Vec::new()))
}
fn calc(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    Ok(match calc::calc_string(&a, c.env.budget)? {
        Some(n) => Out::Text(calc::js_string(n)),
        None => Out::Preserve,
    })
}
fn button(c: &mut Call) -> Result<Out, String> {
    ok(cat(&[&units("<button class=\"button-default\" risu-trigger=\""), &c.shown(1), &units("\">"), &c.shown(0), &units("</button>")]))
}
fn risu(c: &mut Call) -> Result<Out, String> {
    let size = c.arg(0).filter(|a| !a.is_empty()).cloned().unwrap_or_else(|| units("45"));
    ok(cat(&[&units("<img src=\"/logo2.png\" style=\"height:"), &size, &units("px;width:"), &size, &units("px\" />")]))
}
fn compare(c: &mut Call, f: fn(f64, f64) -> bool) -> Result<Out, String> {
    let a = c.number(0)?;
    let b = c.number(1)?;
    bit(f(a, b))
}
fn one(c: &Call, i: usize) -> bool {
    c.arg(i).is_some_and(|a| a == &[49])
}
fn file(c: &mut Call) -> Result<Out, String> {
    if c.env.options.displaying {
        return ok(cat(&[&units("<br><div class=\"risu-file\">"), &c.shown(0), &units("</div><br>")]));
    }
    let Some(data) = c.arg(1) else { return Ok(Out::Preserve) };
    ok(text::buffer_utf8_slice(&text::base64_decode(data)))
}
fn string_test(c: &mut Call, f: fn(&[u16], &[u16]) -> bool) -> Result<Out, String> {
    let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
    bit(f(a, &c.shown(1)))
}
fn replace(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let limit = c.env.budget.max_output;
    ok(replace_all(&a, &c.shown(1), &c.shown(2), limit, c.env.budget, None)?)
}
fn split_(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
    ok(match c.arg(1) {
        None => array_ops::make_text_array([&a[..]]),
        Some(sep) => array_ops::make_text_array(split_slices(a, sep)),
    })
}
fn join(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let values = parse_array(&a)?;
    let sep = c.arg(1).cloned().unwrap_or_else(|| vec![44]);
    Ok(join_values(&values, &sep, c.env.budget)?.map_or(Out::Preserve, Out::Text))
}
fn spread(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let values = parse_array(&a)?;
    Ok(join_values(&values, &[58, 58], c.env.budget)?.map_or(Out::Preserve, Out::Text))
}
fn unary(c: &mut Call, f: fn(&[u16]) -> Units) -> Result<Out, String> {
    let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
    ok(f(a))
}
// toLocaleLowerCase and toLocaleUpperCase follow the language rules of a tailored default locale.
fn locale_case(c: &mut Call, f: fn(&[u16]) -> Units) -> Result<Out, String> {
    if c.env.options.tailored_case {
        return Err("case-locale".into());
    }
    if c.arg(0).is_some_and(|a| maps_differently(c.env.options.case_tables, a)) {
        return Err("case-table".into());
    }
    unary(c, f)
}
// Whether the engine's tables can map text beyond ASCII other than this crate does.
fn maps_differently(tables: CaseTables, text: &[u16]) -> bool {
    match tables {
        CaseTables::Foreign => text.iter().any(|&u| u >= 128),
        CaseTables::Unicode16 => text::case_differs(text),
        CaseTables::Native => false,
    }
}
// charAt(0).toUpperCase() + slice(1): one UTF-16 unit maps, and a surrogate maps to itself.
fn capitalize_call(c: &mut Call) -> Result<Out, String> {
    let first = c.arg(0).and_then(|a| a.first().copied());
    if first.is_some_and(|u| !(0xd800..0xe000).contains(&u) && maps_differently(c.env.options.case_tables, &[u])) {
        return Err("case-table".into());
    }
    unary(c, capitalize)
}
pub(super) fn capitalize(a: &[u16]) -> Units {
    let Some(&first) = a.first() else { return Vec::new() };
    let mut out = if (0xd800..0xe000).contains(&first) { vec![first] } else { text::upper(&[first]) };
    out.extend_from_slice(&a[1..]);
    out
}
fn scalar_op(c: &mut Call, name: &str, needs_first: bool) -> Result<Out, String> {
    if needs_first && c.args.is_empty() {
        return Ok(Out::Preserve);
    }
    Ok(scalar::evaluate(name, c.args, c.env.budget)?.map_or(Out::Preserve, Out::Text))
}
fn arrayelement(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let values = parse_array(&a)?;
    let n = c.number(1)?;
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let at = if n < 0.0 { values.len() as f64 + n } else { n };
    if at < 0.0 || at >= values.len() as f64 {
        return s("null");
    }
    ok(element_text(&values[at as usize]))
}
fn dictelement(c: &mut Call) -> Result<Out, String> {
    let dict = parse_dict(&c.shown(0))?;
    if matches!(dict, Json::Null) {
        return Ok(Out::Preserve);
    }
    match lookup(&dict, &c.shown(1)) {
        Lookup::Value(v) => ok(element_text(&v)),
        Lookup::Undefined => s("null"),
        Lookup::Proto => Err("prototype-property".into()),
    }
}
fn objectassert(c: &mut Call) -> Result<Out, String> {
    let mut dict = parse_dict(&c.shown(0))?;
    if matches!(dict, Json::Null) {
        return Ok(Out::Preserve);
    }
    let key = c.shown(1);
    let present = match lookup(&dict, &key) {
        Lookup::Value(v) => truthy(&v),
        Lookup::Proto => true,
        Lookup::Undefined => false,
    };
    if !present {
        let value = c.arg(2).cloned();
        match &mut dict {
            Json::Obj(fields) => match (fields.iter().position(|(k, _)| *k == key), value) {
                (Some(i), Some(v)) => fields[i].1 = Json::Str(v),
                (Some(i), None) => {
                    fields.remove(i);
                }
                (None, Some(v)) => fields.push((key, Json::Str(v))),
                (None, None) => {}
            },
            Json::Arr(a) => {
                if key == units("length") {
                    return Err("objectassert-array-length".into());
                }
                if let Some(i) = index_key(&key) {
                    if i >= a.len() + 65_536 {
                        return Err("array-hole-limit".into());
                    }
                    if i >= a.len() {
                        a.resize(i + 1, Json::Null);
                    }
                    a[i] = value.map_or(Json::Null, Json::Str);
                }
            }
            // Assigning to a property of a primitive throws in strict mode code.
            _ => return Ok(Out::Preserve),
        }
    }
    ok(json_string(&dict))
}
fn element(c: &mut Call) -> Result<Out, String> {
    let Some(first) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let mut current = Json::Str(first);
    for key in &c.args[1..] {
        c.env.budget.tick()?;
        let source = match &current {
            Json::Str(s) => s.clone(),
            other => match js_string_of(other, c.env.budget)? {
                Some(s) => s,
                None => return s("null"),
            },
        };
        let Some(parsed) = parse_json(&source)? else { return s("null") };
        if !matches!(parsed, Json::Obj(_) | Json::Arr(_)) {
            return s("null");
        }
        current = match lookup(&parsed, key) {
            Lookup::Value(v) => v,
            Lookup::Undefined => return s("null"),
            Lookup::Proto => return Err("prototype-property".into()),
        };
        if !truthy(&current) {
            return s("null");
        }
    }
    match current {
        Json::Str(s) => ok(s),
        // A non-string return reaches the {text, var} branch of the parser.
        _ => Err("element-non-string".into()),
    }
}
fn array_op(c: &mut Call, name: &str) -> Result<Out, String> {
    let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
    if json_depth_exceeds(a, 64) {
        return Err("json-depth".into());
    }
    ok(array_ops::evaluate(name, c.args)?)
}
fn arrayassert(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let mut values = parse_array(&a)?;
    let index = c.number(1)?;
    if index >= values.len() as f64 {
        if let Some(i) = index_key(&calc::js_string(index)) {
            if i >= values.len() + 65_536 {
                return Err("array-hole-limit".into());
            }
            values.resize(i, Json::Null);
            values.push(c.arg(2).cloned().map_or(Json::Null, Json::Str));
        }
    }
    ok(make_array(values))
}
fn makearray(c: &mut Call) -> Result<Out, String> {
    ok(make_array(c.args.iter().cloned().map(Json::Str).collect()))
}
fn makedict(c: &mut Call) -> Result<Out, String> {
    let mut fields: Vec<(Units, Json)> = Vec::new();
    for arg in c.args {
        let Some(eq) = arg.iter().position(|&u| u == 61) else { continue };
        let key = arg[..eq].to_vec();
        // Assigning a string to __proto__ leaves the object unchanged.
        if key == units("__proto__") {
            continue;
        }
        let value = Json::Str(arg[eq + 1..].to_vec());
        match fields.iter().position(|(k, _)| *k == key) {
            Some(i) => fields[i].1 = value,
            None => fields.push((key, value)),
        }
    }
    ok(json_string(&Json::Obj(fields)))
}
fn cbr(c: &mut Call) -> Result<Out, String> {
    if c.args.is_empty() {
        return s("\\n");
    }
    let n = c.number(0)?;
    let count = if n < 1.0 { 1.0 } else { n };
    let count = if count.is_nan() { 0.0 } else { count.trunc() };
    if count.is_infinite() {
        return Ok(Out::Preserve);
    }
    if count * c.dat.len() as f64 > c.env.budget.max_output as f64 {
        return Err("output-limit".into());
    }
    ok(c.dat.repeat(count as usize))
}
fn range(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let values = parse_array(&a)?;
    let b = &mut *c.env.budget;
    let get = |i: usize, b: &mut Budget| -> Result<Option<f64>, String> { js_number(&values[i], b) };
    let (start, end) = match values.len() {
        0 => (Some(0.0), Some(f64::NAN)),
        1 => (Some(0.0), get(0, b)?),
        _ => (get(0, b)?, get(1, b)?),
    };
    let step = if values.len() > 2 { get(2, b)? } else { Some(1.0) };
    let (Some(start), Some(end), Some(step)) = (start, end, step) else { return Ok(Out::Preserve) };
    let mut out = Vec::new();
    let mut size = 2;
    let mut i = start;
    while i < end {
        b.tick()?;
        let item = calc::js_string(i);
        size += item.len() + 3;
        output_guard(size, b)?;
        out.push(Json::Str(item));
        i += step;
    }
    ok(make_array(out))
}
fn filter(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let values = parse_array(&a)?;
    let mode = match c.arg(1).map(|m| String::from_utf16_lossy(m)) {
        Some(m) if m == "nonempty" => 1,
        Some(m) if m == "unique" => 2,
        _ => 0,
    };
    let empty = |v: &Json| matches!(v, Json::Str(s) if s.is_empty());
    let mut kept = Vec::new();
    for (i, v) in values.iter().enumerate() {
        c.env.budget.tick()?;
        let first = || -> Result<bool, String> {
            if matches!(v, Json::Arr(_) | Json::Obj(_)) {
                return Ok(true);
            }
            Ok(values[..i].iter().all(|w| !strict_equal(w, v)))
        };
        if i % 64 == 0 && mode != 1 {
            for _ in 0..i / 64 {
                c.env.budget.tick()?;
            }
        }
        let keep = match mode {
            0 => !empty(v) && first()?,
            1 => !empty(v),
            _ => first()?,
        };
        if keep {
            kept.push(v.clone());
        }
    }
    ok(make_array(kept))
}
fn every_some(c: &mut Call, all: bool) -> Result<Out, String> {
    let values = if c.args.len() > 1 {
        c.args.iter().cloned().map(Json::Str).collect()
    } else {
        let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
        parse_array(&a)?
    };
    let one = |v: &Json| matches!(v, Json::Str(s) if s == &[49]);
    bit(if all { values.iter().all(one) } else { values.iter().any(one) })
}
fn aggregate(c: &mut Call, name: &str) -> Result<Out, String> {
    Ok(aggregates::evaluate(name, c.args, c.env.budget)?.map_or(Out::Preserve, Out::Text))
}
fn hash(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    ok(deterministic_hash::evaluate(&a, c.env.budget)?)
}
fn fromhex(c: &mut Call) -> Result<Out, String> {
    ok(number_string(scalar::hexadecimal(c.args.first(), c.env.budget)?))
}
// Number.parseInt(value) with the radix taken from an optional 0x prefix.
fn parse_int(value: &[u16], budget: &mut Budget) -> Result<f64, String> {
    let mut v = text::trim_start(value);
    let negative = v.first() == Some(&45);
    if matches!(v.first(), Some(43 | 45)) {
        v = &v[1..];
    }
    let n = if v.first() == Some(&48) && matches!(v.get(1), Some(120 | 88)) {
        scalar::hexadecimal(Some(&v.to_vec()), budget)?
    } else {
        let digits = v.iter().take_while(|c| (48..=57).contains(*c)).count();
        if digits == 0 {
            f64::NAN
        } else {
            v[..digits].iter().map(|&c| c as u8 as char).collect::<String>().parse().unwrap_or(f64::NAN)
        }
    };
    Ok(if negative { -n } else { n })
}
// Number.prototype.toString(16) of an integer-valued double.
fn hex_string(n: f64) -> Units {
    if n.is_nan() || n.is_infinite() {
        return number_string(n);
    }
    let magnitude = n.abs();
    let digits = if magnitude < 18_446_744_073_709_551_616.0 {
        format!("{:x}", magnitude as u64)
    } else {
        let bits = magnitude.to_bits();
        let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
        let shift = ((bits >> 52) & 0x7ff) as usize - 1075;
        format!("{:x}{}", u128::from(mantissa) << (shift % 4), "0".repeat(shift / 4))
    };
    let mut out = if n < 0.0 { vec![45] } else { Vec::new() };
    out.extend(units(&digits));
    out
}
fn tohex(c: &mut Call) -> Result<Out, String> {
    let value = parse_int(&c.shown(0), c.env.budget)?;
    ok(hex_string(value))
}
fn iserror(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
    bit(text::lower(a).starts_with(&units("error:")))
}
fn xor(c: &mut Call) -> Result<Out, String> {
    let mut bytes = text::utf8_encode(c.arg(0).map_or(&[][..], |a| a));
    bytes.iter_mut().for_each(|b| *b ^= 0xff);
    ok(text::base64_encode(&bytes))
}
fn xordecrypt(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
    let mut bytes = text::base64_decode(a);
    bytes.iter_mut().for_each(|b| *b ^= 0xff);
    ok(text::utf8_decode(&bytes))
}
fn code_unit(value: f64) -> u16 {
    if value.is_finite() { value.trunc().rem_euclid(65536.0) as u16 } else { 0 }
}
fn crypt(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0).cloned() else { return Ok(Out::Preserve) };
    let mut shift = if c.arg(1).is_some_and(|s| !s.is_empty()) { c.number(1)? } else { 32768.0 };
    if shift.is_nan() {
        shift = 32768.0;
    }
    ok(a.iter()
        .map(|&u| {
            let mut shifted = f64::from(u) + shift;
            if shifted > 65535.0 {
                shifted -= 65536.0;
            }
            code_unit(shifted)
        })
        .collect())
}
fn reverse(c: &mut Call) -> Result<Out, String> {
    let a = c.arg(0).cloned().unwrap_or_default();
    ok(text::code_points(&a).into_iter().rev().flatten().copied().collect())
}
fn comment(c: &mut Call) -> Result<Out, String> {
    if !c.env.options.displaying {
        return s("");
    }
    ok(cat(&[&units("<div class=\"risu-comment\">"), &c.shown(0), &units("</div>")]))
}
fn ruby(c: &mut Call) -> Result<Out, String> {
    ok(cat(&[&units("<ruby>"), &c.shown(0), &units("<rp> (</rp><rt>"), &c.shown(1), &units("</rt><rp>) </rp></ruby>")]))
}
fn codeblock(c: &mut Call) -> Result<Out, String> {
    let Some(last) = c.args.last() else { return Ok(Out::Preserve) };
    let mut code = last.clone();
    for (from, to) in [("\"", "&quot;"), ("'", "&#39;"), ("<", "&lt;"), (">", "&gt;")] {
        code = replace_literal(&code, &units(from), &units(to));
    }
    if c.args.len() > 1 {
        return ok(cat(&[&units("<pre-hljs-placeholder lang=\""), &c.args[0], &units("\">"), &code, &units("</pre-hljs-placeholder>")]));
    }
    ok(cat(&[&units("<pre><code>"), &code, &units("</code></pre>")]))
}
fn bkspc(c: &mut Call) -> Result<Out, String> {
    if c.root.is_empty() {
        return s("");
    }
    let root = text::trim_end(c.root).to_vec();
    let mut at = root.len() as isize - 1;
    while at >= 0 {
        if at == 0 || matches!(root[at as usize], 32 | 10 | 9) {
            break;
        }
        at -= 1;
    }
    let at = at.max(0) as usize;
    *c.root = text::trim_end(&root[..at]).to_vec();
    s("")
}
fn erase(c: &mut Call) -> Result<Out, String> {
    if c.root.is_empty() {
        return s("");
    }
    let root = text::trim_end(c.root).to_vec();
    let mut at = root.len() as isize - 1;
    let mut found = false;
    while at >= 0 {
        if matches!(root[at as usize], 46 | 33 | 63 | 10) {
            found = true;
            break;
        }
        if at == 0 {
            break;
        }
        at -= 1;
    }
    let at = if at == -1 { 0 } else if found { at as usize + 1 } else { at as usize };
    *c.root = text::trim_end(&root[..at]).to_vec();
    s("")
}

// --- snapshot-served host reads -------------------------------------------

fn snapshot_read(map: &HashMap<Units, Units>, key: &[u16]) -> Result<Units, String> {
    if listed(OBJECT_PROTO, key) {
        return Err("variable-prototype-key".into());
    }
    Ok(map.get(key).cloned().unwrap_or_else(|| units("null")))
}
// getChatVar(key): the stored value, else the first default naming the key, else 'null'. A missing key is the
// property 'undefined', which no default line names. Against a database the stored keys carry a '$' prefix, so no
// key meets a prototype name, and no selected character reads 'null'.
pub(super) fn chat_var(context: &Context, key: Option<&[u16]>) -> Result<Units, String> {
    let name = key.unwrap_or(UNDEFINED);
    match &context.host {
        Some(host) if !host.has_character() => return Ok(units("null")),
        Some(_) => {}
        None if listed(OBJECT_PROTO, name) => return Err("variable-prototype-key".into()),
        None => {}
    }
    if context.unreadable.contains(name) {
        return Err("unreadable-variable".into());
    }
    let value = context.variables.get(name).or_else(|| key.and_then(|k| context.defaults.get(k)));
    Ok(value.cloned().unwrap_or_else(|| units("null")))
}
pub(super) fn global_var(context: &Context, key: &[u16]) -> Result<Units, String> {
    snapshot_read(&context.globals, key)
}
fn getvar(c: &mut Call) -> Result<Out, String> {
    match c.env.chat_var(c.args.first().map(Vec::as_slice))? {
        ChatVar::Text(value) => ok(value),
        // The parser keeps the tag for a falsy result. A truthy one is read as `{text, var}`: reading the forced
        // return of a missing `var` throws a TypeError outside the matcher's catch (parser.svelte.ts:2023-2032), and a
        // `var` that holds a value declines.
        ChatVar::Other { holds_var: true, .. } => Err("matcher-result-var".into()),
        ChatVar::Other { truthy: false, .. } => Ok(Out::Preserve),
        ChatVar::Other { truthy: true, .. } => Err("invalid:matcher-result".into()),
    }
}
fn getglobalvar(c: &mut Call) -> Result<Out, String> {
    ok(global_var(c.env.context, c.arg(0).map_or(UNDEFINED, Vec::as_slice))?)
}
// Math.random from the injected source, or the verification LCG the oracle installs.
fn draw(c: &mut Call) -> Result<f64, String> {
    let context = &mut *c.env.context;
    context.draws += 1;
    if let Some(rng) = c.env.rng.as_deref_mut() {
        return rng.random().map_err(super::shared_error);
    }
    context.state = context.state.wrapping_mul(1664525).wrapping_add(1013904223);
    Ok(f64::from(context.state) / 4_294_967_296.0)
}
// '§X', which random writes in place of an escaped comma.
const SECTION_X: &[u16] = &[167, 88];
fn random(c: &mut Call) -> Result<Out, String> {
    let rand = draw(c)?;
    pick_from(c, rand)
}
// randomPickImpl: the number without arguments, else the element `rand` selects.
pub(super) fn pick_from(c: &mut Call, rand: f64) -> Result<Out, String> {
    if c.args.is_empty() {
        return ok(number_string(rand));
    }
    let pick = |len: usize| if c.env.options.tokenize_accurate { 0 } else { (rand * len as f64).floor() as usize };
    // Several arguments are the choices as given; only the drawn one is rewritten.
    if c.args.len() > 1 {
        let index = pick(c.args.len());
        return ok(c.args.get(index).map_or_else(Vec::new, |s| replace_literal(s, SECTION_X, &[44])));
    }
    let values: Vec<Json> = {
        let a = &c.args[0];
        if a.first() == Some(&91) && a.last() == Some(&93) {
            parse_array(a)?
        } else {
            let marked = replace_literal(a, &[92, 44], SECTION_X);
            let mut parts = Vec::new();
            let mut start = 0;
            for (i, &u) in marked.iter().enumerate() {
                if u == 58 || u == 44 {
                    parts.push(Json::Str(marked[start..i].to_vec()));
                    start = i + 1;
                }
            }
            parts.push(Json::Str(marked[start..].to_vec()));
            parts
        }
    };
    let index = pick(values.len());
    ok(match values.get(index) {
        Some(Json::Str(s)) => replace_literal(s, SECTION_X, &[44]),
        Some(v) => json_string(v),
        None => Vec::new(),
    })
}
fn dice_loop(c: &mut Call, count: f64, sides: f64) -> Result<Out, String> {
    if count > 1_000_000.0 {
        return Err("rng-loop-limit".into());
    }
    let mut total = 0.0;
    let mut i = 0.0;
    while i < count {
        c.env.budget.tick()?;
        total += (draw(c)? * sides).floor() + 1.0;
        i += 1.0;
    }
    ok(number_string(total))
}
fn randint(c: &mut Call) -> Result<Out, String> {
    let min = c.number(0)?;
    let max = c.number(1)?;
    if min.is_nan() || max.is_nan() {
        return s("NaN");
    }
    let r = draw(c)?;
    ok(number_string((r * (max - min + 1.0)).floor() + min))
}
fn dice(c: &mut Call) -> Result<Out, String> {
    let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
    let parts = split(a, &[100]);
    let budget = &mut *c.env.budget;
    let count = scalar::number(parts.first(), Some(budget))?;
    let sides = scalar::number(parts.get(1), Some(budget))?;
    if count.is_nan() || sides.is_nan() {
        return s("NaN");
    }
    dice_loop(c, count, sides)
}
fn roll(c: &mut Call) -> Result<Out, String> {
    match dice_notation(c)? {
        Ok((count, sides)) => dice_loop(c, count, sides),
        Err(out) => Ok(out),
    }
}
// The XdY notation of roll and rollp: '1' without arguments, 'NaN' for an unusable one, else the count and sides.
pub(super) fn dice_notation(c: &mut Call) -> Result<Result<(f64, f64), Out>, String> {
    let Some(a) = c.arg(0) else { return Ok(Err(Out::Text(units("1")))) };
    let parts = split(a, &[100]);
    let budget = &mut *c.env.budget;
    let (mut count, mut sides) = (1.0, 6.0);
    if parts.len() == 2 {
        count = if parts[0].is_empty() { 1.0 } else { scalar::number(Some(&parts[0]), Some(budget))? };
        sides = if parts[1].is_empty() { 6.0 } else { scalar::number(Some(&parts[1]), Some(budget))? };
    } else if parts.len() == 1 {
        sides = scalar::number(Some(&parts[0]), Some(budget))?;
    }
    if count.is_nan() || sides.is_nan() || count < 1.0 || sides < 1.0 {
        return Ok(Err(Out::Text(units("NaN"))));
    }
    Ok(Ok((count, sides)))
}

// --- registry --------------------------------------------------------------

macro_rules! entry {
    ($status:ident, $fold:expr, $id:expr, [$($k:expr),*], $call:expr) => {
        Entry { id: $id, keys: &[$($k),*], status: Status::$status, call: $call, foldable: $fold }
    };
}
macro_rules! native {
    ($id:expr, [$($k:expr),*], $f:expr) => { entry!(Native, true, $id, [$($k),*], Some($f)) };
}
macro_rules! stateful {
    ($id:expr, [$($k:expr),*], $f:expr) => { entry!(Native, false, $id, [$($k),*], Some($f)) };
}
macro_rules! snapshot {
    ($id:expr, [$($k:expr),*], $f:expr) => { entry!(Snapshot, false, $id, [$($k),*], Some($f)) };
}
macro_rules! host {
    ($id:expr, [$($k:expr),*], $f:expr) => { entry!(Host, false, $id, [$($k),*], Some($f)) };
}
macro_rules! refused {
    ($id:expr, [$($k:expr),*]) => { entry!(Refused, false, $id, [$($k),*], None) };
}
macro_rules! constant {
    ($id:expr, [$($k:expr),*], $v:expr) => { native!($id, [$($k),*], |_| s($v)) };
}

fn build() -> Vec<Entry> {
    vec![
        host!("char", ["char", "bot"], host::char_),
        snapshot!("user", ["user"], |c| ok(c.env.context.user.clone())),
        host!("trigger_id", ["triggerid"], host::trigger_id),
        host!("previouscharchat", ["previouscharchat", "lastcharmessage"], host::previouscharchat),
        host!("previoususerchat", ["previoususerchat", "lastusermessage"], host::previoususerchat),
        host!("personality", ["personality", "charpersona"], host::personality),
        host!("description", ["description", "chardesc"], host::description),
        host!("scenario", ["scenario"], host::scenario),
        host!("exampledialogue", ["exampledialogue", "examplemessage"], host::exampledialogue),
        host!("persona", ["persona", "userpersona"], host::persona),
        host!("mainprompt", ["mainprompt", "systemprompt"], host::mainprompt),
        host!("lorebook", ["lorebook", "worldinfo"], host::lorebook),
        host!("userhistory", ["userhistory", "usermessages"], host::userhistory),
        host!("charhistory", ["charhistory", "charmessages"], host::charhistory),
        host!("jb", ["jb", "jailbreak"], host::jb),
        host!("globalnote", ["globalnote", "systemnote", "ujb"], host::globalnote),
        host!("authornote", ["authornote"], host::authornote),
        snapshot!("chatindex", ["chatindex"], |c| s(&c.env.context.index.to_string())),
        host!("firstmsgindex", ["firstmsgindex", "firstmessageindex"], host::firstmsgindex),
        constant!("blank", ["blank", "none"], ""),
        host!("messagetime", ["messagetime"], host::messagetime),
        host!("messagedate", ["messagedate"], host::messagedate),
        host!("messageunixtimearray", ["messageunixtimearray"], host::messageunixtimearray),
        host!("unixtime", ["unixtime"], host::unixtime),
        refused!("time", []),
        host!("isotime", ["isotime"], host::isotime),
        host!("isodate", ["isodate"], host::isodate),
        host!("messageidleduration", ["messageidleduration"], host::messageidleduration),
        host!("idleduration", ["idleduration"], host::idleduration),
        constant!("br", ["br", "newline"], "\n"),
        host!("model", ["model"], host::model),
        host!("axmodel", ["axmodel"], host::axmodel),
        host!("role", ["role"], host::role),
        host!("isfirstmsg", ["isfirstmsg", "isfirstmessage"], host::isfirstmsg),
        host!("jbtoggled", ["jbtoggled"], host::jbtoggled),
        host!("maxcontext", ["maxcontext"], host::maxcontext),
        host!("lastmessage", ["lastmessage"], host::lastmessage),
        host!("lastmessageid", ["lastmessageid", "lastmessageindex"], host::lastmessageid),
        stateful!("tempvar", ["tempvar", "gettempvar"], tempvar),
        stateful!("settempvar", ["settempvar"], settempvar),
        stateful!("return", ["return"], return_),
        snapshot!("getvar", ["getvar"], getvar),
        native!("calc", ["calc"], calc),
        host!("addvar", ["addvar"], host::addvar),
        host!("setvar", ["setvar"], host::setvar),
        host!("setdefaultvar", ["setdefaultvar"], host::setdefaultvar),
        snapshot!("getglobalvar", ["getglobalvar"], getglobalvar),
        native!("button", ["button"], button),
        native!("risu", ["risu"], risu),
        native!("equal", ["equal"], |c| bit(c.arg(0) == c.arg(1))),
        native!("notequal", ["notequal"], |c| bit(c.arg(0) != c.arg(1))),
        native!("greater", ["greater"], |c| compare(c, |a, b| a > b)),
        native!("less", ["less"], |c| compare(c, |a, b| a < b)),
        native!("greaterequal", ["greaterequal"], |c| compare(c, |a, b| a >= b)),
        native!("lessequal", ["lessequal"], |c| compare(c, |a, b| a <= b)),
        native!("and", ["and"], |c| bit(one(c, 0) && one(c, 1))),
        native!("or", ["or"], |c| bit(one(c, 0) || one(c, 1))),
        native!("not", ["not"], |c| bit(!one(c, 0))),
        stateful!("file", ["file"], file),
        native!("startswith", ["startswith"], |c| string_test(c, |a, b| a.starts_with(b))),
        native!("endswith", ["endswith"], |c| string_test(c, |a, b| a.ends_with(b))),
        native!("contains", ["contains"], |c| string_test(c, |a, b| text::find(a, b).is_some())),
        native!("replace", ["replace"], replace),
        native!("split", ["split"], split_),
        native!("join", ["join"], join),
        native!("spread", ["spread"], spread),
        native!("trim", ["trim"], |c| unary(c, |a| trim(a).to_vec())),
        native!("length", ["length"], |c| unary(c, |a| units(&a.len().to_string()))),
        native!("arraylength", ["arraylength"], |c| {
            let Some(a) = c.arg(0) else { return Ok(Out::Preserve) };
            s(&parse_array(a)?.len().to_string())
        }),
        native!("lower", ["lower"], |c| locale_case(c, text::lower)),
        native!("upper", ["upper"], |c| locale_case(c, text::upper)),
        native!("capitalize", ["capitalize"], capitalize_call),
        native!("round", ["round"], |c| scalar_op(c, "round", false)),
        native!("floor", ["floor"], |c| scalar_op(c, "floor", false)),
        native!("ceil", ["ceil"], |c| scalar_op(c, "ceil", false)),
        native!("abs", ["abs"], |c| scalar_op(c, "abs", false)),
        native!("remaind", ["remaind"], |c| scalar_op(c, "remaind", false)),
        host!("previouschatlog", ["previouschatlog"], host::previouschatlog),
        native!("tonumber", ["tonumber"], |c| scalar_op(c, "tonumber", true)),
        refused!("pow", ["pow"]),
        native!("arrayelement", ["arrayelement"], arrayelement),
        native!("dictelement", ["dictelement", "objectelement"], dictelement),
        native!("objectassert", ["objectassert", "dictassert"], objectassert),
        native!("element", ["element", "ele"], element),
        native!("arrayshift", ["arrayshift"], |c| array_op(c, "arrayshift")),
        native!("arraypop", ["arraypop"], |c| array_op(c, "arraypop")),
        native!("arraypush", ["arraypush"], |c| array_op(c, "arraypush")),
        native!("arraysplice", ["arraysplice"], |c| array_op(c, "arraysplice")),
        native!("arrayassert", ["arrayassert"], arrayassert),
        native!("makearray", ["makearray", "array", "a"], makearray),
        native!("makedict", ["makedict", "dict", "d", "makeobject", "object", "o"], makedict),
        host!("emotionlist", ["emotionlist"], host::emotionlist),
        host!("assetlist", ["assetlist"], host::assetlist),
        host!("prefillsupported", ["prefillsupported", "prefill"], host::prefillsupported),
        host!("screenwidth", ["screenwidth"], host::screenwidth),
        host!("screenheight", ["screenheight"], host::screenheight),
        native!("cbr", ["cbr", "cnl", "cnewline"], cbr),
        constant!("decbo", ["decbo", "displayescapedcurlybracketopen"], "\u{e9b8}"),
        constant!("decbc", ["decbc", "displayescapedcurlybracketclose"], "\u{e9b9}"),
        constant!("bo", ["bo", "ddecbo", "doubledisplayescapedcurlybracketopen"], "\u{e9b8}\u{e9b8}"),
        constant!("bc", ["bc", "ddecbc", "doubledisplayescapedcurlybracketclose"], "\u{e9b9}\u{e9b9}"),
        constant!("displayescapedbracketopen", ["displayescapedbracketopen", "debo", "("], "\u{e9ba}"),
        constant!("displayescapedbracketclose", ["displayescapedbracketclose", "debc", ")"], "\u{e9bb}"),
        constant!("displayescapedanglebracketopen", ["displayescapedanglebracketopen", "deabo", "<"], "\u{e9bc}"),
        constant!("displayescapedanglebracketclose", ["displayescapedanglebracketclose", "deabc", ">"], "\u{e9bd}"),
        constant!("displayescapedcolon", ["displayescapedcolon", "dec"], "\u{e9be}"),
        constant!("displayescapedsemicolon", ["displayescapedsemicolon", ";"], "\u{e9bf}"),
        host!("chardisplayasset", ["chardisplayasset"], host::chardisplayasset),
        host!("history", ["history", "messages"], host::history),
        native!("range", ["range"], range),
        host!("date", ["date", "datetimeformat"], host::date),
        host!("time@2", ["time"], host::time),
        host!("moduleenabled", ["moduleenabled"], host::moduleenabled),
        host!("moduleassetlist", ["moduleassetlist"], host::moduleassetlist),
        native!("filter", ["filter"], filter),
        native!("all", ["all"], |c| every_some(c, true)),
        native!("any", ["any"], |c| every_some(c, false)),
        native!("min", ["min"], |c| aggregate(c, "min")),
        native!("max", ["max"], |c| aggregate(c, "max")),
        native!("sum", ["sum"], |c| aggregate(c, "sum")),
        native!("average", ["average"], |c| aggregate(c, "average")),
        native!("fixnum", ["fixnum", "fixnumber"], |c| scalar_op(c, "fixnum", false)),
        native!("unicodeencode", ["unicodeencode"], |c| scalar_op(c, "unicodeencode", true)),
        native!("unicodedecode", ["unicodedecode"], |c| scalar_op(c, "unicodedecode", false)),
        native!("u", ["u", "unicodedecodefromhex"], |c| scalar_op(c, "u", false)),
        native!("ue", ["ue", "unicodeencodefromhex"], |c| scalar_op(c, "ue", false)),
        native!("hash", ["hash"], hash),
        snapshot!("randint", ["randint"], randint),
        snapshot!("dice", ["dice"], dice),
        native!("fromhex", ["fromhex"], fromhex),
        native!("tohex", ["tohex"], tohex),
        host!("metadata", ["metadata"], host::metadata),
        native!("iserror", ["iserror"], iserror),
        native!("xor", ["xor", "xorencrypt", "xorencode", "xore"], xor),
        native!("xordecrypt", ["xordecrypt", "xordecode", "xord"], xordecrypt),
        native!("crypt", ["crypt", "crypto", "caesar", "encrypt", "decrypt"], crypt),
        snapshot!("random", ["random"], random),
        host!("pick", ["pick"], host::pick),
        snapshot!("roll", ["roll"], roll),
        host!("rollp", ["rollp", "rollpick"], host::rollp),
        constant!("hiddenkey", ["hiddenkey"], ""),
        native!("reverse", ["reverse"], reverse),
        stateful!("comment", ["comment"], comment),
        native!("tex", ["tex", "latex", "katex"], |c| ok(cat(&[&units("$$"), &c.shown(0), &units("$$")]))),
        native!("ruby", ["ruby", "furigana"], ruby),
        native!("codeblock", ["codeblock"], codeblock),
        stateful!("bkspc", ["bkspc"], bkspc),
        stateful!("erase", ["erase"], erase),
        host!("declare", ["declare"], host::declare),
        // Internal-only: lookup removes the underscores, so no key reaches it.
        entry!(Native, false, "__", [], Some(|_| Ok(Out::Preserve))),
    ]
}

// FxHash over the bytes; registry keys are short ASCII words.
#[derive(Default)]
struct KeyHasher(u64);
impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
}
// The longest registry key, which `find_entry` normalizes into a buffer of this size.
const KEY_UNITS: usize = 48;
struct Registry {
    entries: Vec<Entry>,
    keys: HashMap<&'static str, usize, BuildHasherDefault<KeyHasher>>,
}
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let entries = build();
        let mut keys = HashMap::default();
        for (i, e) in entries.iter().enumerate() {
            for k in e.keys {
                assert!(k.len() <= KEY_UNITS, "CBS key {k} is longer than {KEY_UNITS}");
                assert!(keys.insert(*k, i).is_none(), "duplicate CBS key {k}");
            }
        }
        Registry { entries, keys }
    })
}
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn entries() -> &'static [Entry] {
    &registry().entries
}
// name.toLocaleLowerCase().replace(/[\s_-]/g, ''): among non-ASCII code points only white space
// (removed) and U+212A KELVIN SIGN (to 'k') lower-case to ASCII, so any other one cannot match.
// A name longer than every key once normalized cannot match one.
fn normalize(name: &[u16], out: &mut [u8; KEY_UNITS]) -> Option<usize> {
    let mut n = 0;
    for &c in name {
        if ws(c) || c == 95 || c == 45 {
            continue;
        }
        let byte = match c {
            65..=90 => c as u8 + 32,
            0x212a => b'k',
            0..=127 => c as u8,
            _ => return None,
        };
        *out.get_mut(n)? = byte;
        n += 1;
    }
    Some(n)
}
pub(super) fn find_entry(name: &[u16]) -> Option<&'static Entry> {
    let r = registry();
    let mut key = [0u8; KEY_UNITS];
    let n = normalize(name, &mut key)?;
    r.keys.get(std::str::from_utf8(&key[..n]).ok()?).map(|&i| &r.entries[i])
}
fn arguments(dat: &[u16]) -> (&[u16], Vec<Units>) {
    match dat.iter().position(|&c| c == 58) {
        None => (dat, Vec::new()),
        Some(i) => {
            let sep: &[u16] = if dat.get(i + 1) == Some(&58) { &[58, 58] } else { &[58] };
            (&dat[..i], split(&dat[i + sep.len()..], sep))
        }
    }
}
// matcher(p1, matcherArg, tempVar) for a dat that does not start with '#' or ':'; `arg` holds matcherArg's
// callStack and functions.
pub(super) fn matcher(dat: &[u16], env: &mut Env, temp: &mut Temp, root: &mut Units, arg: &mut ArgState) -> Result<Out, String> {
    if dat.starts_with(&[63, 32]) {
        return Ok(match calc::calc_string(&dat[2..], env.budget)? {
            Some(n) => Out::Text(calc::js_string(n)),
            None => Out::Preserve,
        });
    }
    let (name, args) = arguments(dat);
    // Under the az and tr rules toLocaleLowerCase maps I to dotless i and U+0130 to i.
    if env.options.tailored_case && name.iter().any(|&c| c == 0x49 || c == 0x130) {
        return Err("case-locale".into());
    }
    let Some(entry) = find_entry(name) else { return Ok(Out::Preserve) };
    if env.context.declined.contains(&entry.id) {
        return Err(format!("declined:{}", entry.id));
    }
    let call = match (entry.status, entry.call) {
        (Status::Refused, _) | (_, None) => return Err("unsupported-operator".into()),
        (Status::Host, _) if env.context.host.is_none() => return Err("unsupported-operator".into()),
        (_, Some(call)) => call,
    };
    call(&mut Call { dat, args: &args, env, temp, root, matcher: arg })
}
// A literal macro whose value cannot depend on the invocation.
pub(super) fn foldable(dat: &[u16]) -> bool {
    if dat.starts_with(&[63, 32]) {
        return true;
    }
    find_entry(arguments(dat).0).is_some_and(|e| e.foldable)
}
