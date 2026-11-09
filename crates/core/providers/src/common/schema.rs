//! `simplifySchema` (`src/ts/util.ts`) and `extractJSON` with `jsonOutputTrimmer` (`src/ts/process/templates/jsonSchema.ts`).

use super::js::{self, J};
use super::BuildError;
use risunest_ir::{json, Budget, Error, Text, Value};

fn lower_ascii_or_unicode(value: &Text, budget: &Budget) -> Result<Text, BuildError> {
    // `toLowerCase` on a type name: ASCII names are the supported case; other text declines.
    let Some(source) = value.as_unicode() else { return Err(BuildError::Unsupported("schema-type-case-mapping")) };
    if !source.is_ascii() { return Err(BuildError::Unsupported("schema-type-case-mapping")); }
    Ok(js::text(&source.to_ascii_lowercase(), budget)?)
}

fn copy_member(source: &J, name: &str, result: &mut J, budget: &Budget) -> Result<(), Error> {
    if let Some(value) = js::get(source, name) {
        if !matches!(value, J::Undefined | J::Null) { js::set_str(result, name, js::clone_j(value, budget)?, budget)?; }
    }
    Ok(())
}

fn length_positive(value: &J) -> Result<bool, BuildError> {
    match value {
        J::Arr(items) => Ok(!items.is_empty()),
        J::Str(text) => Ok(text.len_units() > 0),
        J::Undefined | J::Null => Ok(false),
        J::Obj(_) | J::Num(_) | J::Bool(_) => Ok(false),
    }
}

/// `simplifySchema(schema)`. A schema that is not an object comes back unchanged, as TS returns it.
pub(crate) fn simplify_schema(schema: &J, budget: &Budget) -> Result<J, BuildError> {
    budget.work(1)?;
    if !matches!(schema, J::Obj(_) | J::Arr(_)) { return Ok(js::clone_j(schema, budget)?); }
    let mut nullable = js::get(schema, "nullable").map(js::truthy).unwrap_or(false);
    let mut schema_type: Option<J> = match js::get(schema, "type") {
        Some(J::Arr(types)) => {
            if types.iter().any(|item| matches!(item, J::Str(text) if text.equals_ascii(b"null"))) { nullable = true; }
            types.iter().find(|item| !matches!(item, J::Str(text) if text.equals_ascii(b"null"))).map(|item| js::clone_j(item, budget)).transpose()?
        }
        Some(other) => Some(js::clone_j(other, budget)?),
        None => None,
    };
    if matches!(schema_type, Some(J::Undefined)) { schema_type = None; }
    let is = |expected: &str| matches!(&schema_type, Some(J::Str(text)) if text.equals_ascii(expected.as_bytes()));
    let mut result = js::new_object();
    if let Some(kind) = schema_type.as_ref().filter(|kind| js::truthy(kind)) {
        let J::Str(text) = kind else { return Err(BuildError::Unsupported("schema-type-not-text")) };
        js::set_str(&mut result, "type", J::Str(lower_ascii_or_unicode(text, budget)?), budget)?;
    }
    if is("object") {
        let mut properties = js::new_object();
        match js::get(schema, "properties") {
            Some(J::Obj(entries)) => for (key, value) in entries {
                js::set(&mut properties, key.copy(budget)?, simplify_schema(value, budget)?, budget)?;
            },
            Some(J::Undefined | J::Null) | None => {}
            Some(_) => return Err(BuildError::Unsupported("schema-properties-not-object")),
        }
        js::set_str(&mut result, "properties", properties, budget)?;
        if let Some(required) = js::get(schema, "required") {
            if js::truthy(required) && length_positive(required)? { js::set_str(&mut result, "required", js::clone_j(required, budget)?, budget)?; }
        }
    }
    if is("array") {
        let items = match js::get(schema, "items") { Some(items) => simplify_schema(items, budget)?, None => J::Undefined };
        js::set_str(&mut result, "items", items, budget)?;
    }
    if is("string") {
        if let Some(values) = js::get(schema, "enum") {
            if js::truthy(values) && length_positive(values)? { js::set_str(&mut result, "enum", js::clone_j(values, budget)?, budget)?; }
        }
        if let Some(format) = js::get(schema, "format") {
            if js::truthy(format) { js::set_str(&mut result, "format", js::clone_j(format, budget)?, budget)?; }
        }
    }
    if nullable { js::set_str(&mut result, "nullable", J::Bool(true), budget)?; }
    for name in ["maxLength", "minLength", "minProperties", "maxProperties"] { copy_member(schema, name, &mut result, budget)?; }
    if let Some(description) = js::get(schema, "description") {
        if js::truthy(description) { js::set_str(&mut result, "description", js::clone_j(description, budget)?, budget)?; }
    }
    if let Some(any_of) = js::get(schema, "anyOf") {
        if js::truthy(any_of) {
            match any_of {
                J::Arr(items) if !items.is_empty() => {
                    let mapped = items.iter().map(|item| simplify_schema(item, budget)).collect::<Result<Vec<_>, _>>()?;
                    js::set_str(&mut result, "anyOf", J::Arr(mapped), budget)?;
                }
                J::Arr(_) => {}
                _ => return Err(BuildError::Unsupported("schema-any-of-not-array")),
            }
        }
    }
    Ok(result)
}

/// `jsonOutputTrimmer(data)`.
pub(crate) fn json_output_trimmer(data: &Text, budget: &Budget) -> Result<Text, Error> {
    let all = js::units(data);
    let open = js::utf16("<Thoughts>");
    let close = js::utf16("</Thoughts>");
    let mut kept: Vec<u16> = Vec::with_capacity(all.len());
    let mut at = 0;
    while at < all.len() {
        if all[at..].starts_with(&open) {
            if let Some(end) = js::find_units(&all, &close, at + open.len() + 1) {
                at = end + close.len();
                continue;
            }
        }
        kept.push(all[at]);
        at += 1;
    }
    let mut value = js::from_units(&kept, budget)?.trim(budget)?;
    if js::starts_with(&value, "```json") && js::ends_with(&value, "```") {
        let length = value.len_units();
        let end = length.saturating_sub(3);
        value = if 7 >= end { js::text("", budget)? } else { value.slice(7, end, budget)? }.trim(budget)?;
    }
    value.trim(budget)
}

/// `extractJSON(data, format)` with `format` already run through `risuChatParser` by the owner. A dotted format that
/// reaches an object returns the object in TS (a non-text result), which the native path declines.
pub(crate) fn extract_json(data: &Text, format: &Text, budget: &Budget) -> Result<Text, BuildError> {
    let trimmed = data.trim(budget)?;
    if !js::starts_with(&trimmed, "{") { return Ok(trimmed); }
    let source = json_output_trimmer(&trimmed, budget)?;
    let Some(source) = source.as_unicode() else { return Err(BuildError::Unsupported("extract-json-isolated-surrogate")) };
    let parsed = match json::parse(source, budget) {
        Ok(parsed) => parsed,
        Err(Error::Syntax { .. }) => return Ok(trimmed),
        Err(other) => return Err(other.into()),
    };
    let format_units = js::units(format);
    let mut segments = format_units.split(|&unit| unit == 46);
    let first = js::from_units(segments.next().unwrap_or(&[]), budget)?;
    let multiple = format_units.contains(&46);
    const INHERITED: [&str; 12] = ["constructor", "toString", "toLocaleString", "valueOf", "hasOwnProperty", "isPrototypeOf",
        "propertyIsEnumerable", "__proto__", "__defineGetter__", "__defineSetter__", "__lookupGetter__", "__lookupSetter__"];
    let Value::Object(entries) = &parsed else { return Err(BuildError::Unsupported("extract-json-not-object")) };
    let current = entries.as_slice().iter().find(|(name, _)| name.equals(&first)).map(|(_, value)| value);
    let Some(current) = current else {
        if INHERITED.iter().any(|name| first.equals_ascii(name.as_bytes())) { return Err(BuildError::Unsupported("extract-json-inherited-member")); }
        return Ok(js::text("", budget)?);
    };
    if matches!(current, Value::Undefined) { return Ok(js::text("", budget)?); }
    if multiple && matches!(current, Value::Object(_) | Value::Array(_) | Value::Null) {
        return Err(BuildError::Unsupported("extract-json-nested-object"));
    }
    if matches!(current, Value::Null) { return Ok(js::text("", budget)?); }
    match current.to_text(budget) {
        Ok(text) => Ok(text),
        Err(Error::Type { .. }) => Ok(js::text("", budget)?),
        Err(other) => Err(other.into()),
    }
}
