//! `applyParameters`, `getAdditionalParameters` and `applyAdditionalParameters` (`src/ts/process/request/shared.ts`).

use super::js::{self, J};
use super::settings::{ParameterValues, SamplingSettings};
use super::BuildError;
use risunest_ir::{json, Budget, Error, Text, Value};

fn has(parameters: &[Text], name: &str) -> bool { parameters.iter().any(|parameter| parameter.equals_ascii(name.as_bytes())) }

fn effort_name(effort: f64, disabled: &'static str, supports_xhigh: bool, minimum: &'static str) -> &'static str {
    if effort == -1.0 { disabled }
    else if effort == 0.0 { minimum }
    else if effort == 1.0 { "medium" }
    else if effort == 2.0 { "high" }
    else if effort == 3.0 { if supports_xhigh { "xhigh" } else { "high" } }
    else { "medium" }
}

fn verbosity_name(verbosity: f64) -> &'static str {
    if verbosity == 0.0 { "low" } else if verbosity == 1.0 { "medium" } else if verbosity == 2.0 { "high" } else { "medium" }
}

/// `applyParameters(...)` as the list of `(name, value)` pairs it sets, in order. The owner passes the result as
/// `ProviderCall::sampling`; the builder sets each pair on the body.
///
/// `model_mode` is the `ModelModeExtended` of the request, `model_id` the `LLMModel.id`, and `parameters` the model's
/// `LLMParameter` list. A missing separate-parameter set is a TS exception and returns [`BuildError::Thrown`].
pub fn apply_parameters(
    parameters: &[Text], settings: &SamplingSettings, model_mode: &Text, model_id: &Text, budget: &Budget,
) -> Result<Vec<(Text, Value)>, BuildError> {
    apply_parameters_with(parameters, settings, model_mode, model_id, false, budget)
}

/// [`apply_parameters`] with `arg.ignoreTopKIfZero`: a `top_k` of zero in the chosen set is left out.
pub fn apply_parameters_with(
    parameters: &[Text], settings: &SamplingSettings, model_mode: &Text, model_id: &Text, ignore_top_k_if_zero: bool, budget: &Budget,
) -> Result<Vec<(Text, Value)>, BuildError> {
    let disabled = if has(parameters, "reasoning_effort_none") { "none" } else { "minimal" };
    let minimum = if has(parameters, "reasoning_effort_min_medium") { "medium" } else { "low" };
    let supports_xhigh = has(parameters, "reasoning_effort_xhigh");
    let is_model = model_mode.equals_ascii(b"model");
    let separate = settings.separate_enabled && (!is_model || settings.separate_by_model);
    let mut chosen: Option<&ParameterValues> = Some(&settings.base);
    if separate {
        let table = if settings.separate_by_model { &settings.overrides } else { &settings.by_mode };
        let key = if settings.separate_by_model { model_id } else { model_mode };
        let found = table.iter().find(|(name, _)| name.equals(key)).map(|(_, values)| values);
        if settings.separate_by_model && found.is_none() {
            let message = format!("No seperate parameters found for model {} in model mode {}. Please set parameters for this model",
                lossy(model_id), lossy(model_mode));
            return Err(BuildError::Thrown(js::text(&message, budget)?));
        }
        chosen = if model_mode.equals_ascii(b"submodel") {
            settings.by_mode.iter().find(|(name, _)| name.equals_ascii(b"otherAx")).map(|(_, values)| values)
        } else { found };
    }
    let mut output = Vec::new();
    for parameter in parameters {
        let Some(name) = parameter.as_unicode() else { continue };
        if matches!(name, "reasoning_effort_min_medium" | "reasoning_effort_none" | "reasoning_effort_xhigh") { continue; }
        let chosen = chosen.ok_or(BuildError::Thrown(js::text("Cannot read properties of undefined", budget)?))?;
        if ignore_top_k_if_zero && name == "top_k" && chosen.top_k == 0.0 { continue; }
        let value = match name {
            "temperature" => Number(scaled(chosen.temperature)),
            "top_k" => Number(chosen.top_k),
            "repetition_penalty" => Number(chosen.repetition_penalty),
            "min_p" => Number(chosen.min_p),
            "top_a" => Number(chosen.top_a),
            "top_p" => Number(chosen.top_p),
            "thinking_tokens" => Number(chosen.thinking_tokens),
            "frequency_penalty" => Number(scaled(chosen.frequency_penalty)),
            "presence_penalty" => Number(scaled(chosen.presence_penalty)),
            "reasoning_effort" => Word(effort_name(chosen.reasoning_effort, disabled, supports_xhigh, minimum)),
            "verbosity" => Word(verbosity_name(chosen.verbosity)),
            _ => Number(0.0),
        };
        if let Number(number) = value {
            if number == -1000.0 || (separate && number.is_nan()) { continue; }
            output.push((parameter.copy(budget)?, Value::Number(number)));
        } else if let Word(word) = value {
            output.push((parameter.copy(budget)?, Value::Text(js::text(word, budget)?)));
        }
    }
    Ok(output)
}

enum Parameter { Number(f64), Word(&'static str) }
use Parameter::{Number, Word};

fn scaled(value: f64) -> f64 { if value == -1000.0 { -1000.0 } else { value / 100.0 } }

fn lossy(value: &Text) -> String { String::from_utf16_lossy(&value.units().collect::<Vec<u16>>()) }

/// `getAdditionalParameters(aiModel)`. `custom_params` is `customModels.find(id === aiModel)?.params`.
pub fn additional_parameters(
    ai_model: &Text, additional_params: &[(Text, Text)], apply_to_all: bool, custom_params: Option<&Text>, budget: &Budget,
) -> Result<Vec<(Text, Text)>, Error> {
    let copy = |pairs: &[(Text, Text)]| -> Result<Vec<(Text, Text)>, Error> {
        pairs.iter().map(|(key, value)| Ok((key.copy(budget)?, value.copy(budget)?))).collect()
    };
    if ai_model.len_units() == 0 { return Ok(Vec::new()); }
    if ai_model.equals_ascii(b"reverse_proxy") { return copy(additional_params); }
    if !js::starts_with(ai_model, "xcustom:::") { return if apply_to_all { copy(additional_params) } else { Ok(Vec::new()) }; }
    let Some(params) = custom_params.filter(|value| value.len_units() != 0) else { return Ok(Vec::new()) };
    let all = js::units(params);
    let mut output = Vec::new();
    for line in all.split(|&unit| unit == 10) {
        let mut parts = line.split(|&unit| unit == 61);
        let first = parts.next().unwrap_or(&[]);
        let rest: Vec<&[u16]> = parts.collect();
        if rest.is_empty() { continue; }
        let mut value: Vec<u16> = Vec::new();
        for (index, part) in rest.iter().enumerate() {
            if index != 0 { value.push(61); }
            value.extend_from_slice(part);
        }
        output.push((js::from_units(first, budget)?, js::from_units(&value, budget)?));
    }
    Ok(output)
}

fn boundary(unit: Option<u16>) -> bool {
    match unit {
        None => true,
        Some(unit) => !(unit < 128 && ((unit as u8).is_ascii_alphanumeric() || unit == 95 || unit == 36)),
    }
}

fn normalize_relaxed_keywords(value: &[u16]) -> Vec<u16> {
    let keywords: [(&str, &str); 3] = [("True", "true"), ("False", "false"), ("None", "null")];
    let mut output = Vec::with_capacity(value.len());
    let mut quote: Option<u16> = None;
    let mut index = 0;
    while index < value.len() {
        let unit = value[index];
        if let Some(open) = quote {
            output.push(unit);
            if unit == 92 && index + 1 < value.len() { index += 1; output.push(value[index]); index += 1; continue; }
            if unit == open { quote = None; }
            index += 1;
            continue;
        }
        if unit == 34 || unit == 39 { quote = Some(unit); output.push(unit); index += 1; continue; }
        let mut replaced = false;
        for (keyword, replacement) in keywords {
            let word = js::utf16(keyword);
            if value[index..].starts_with(&word)
                && boundary(index.checked_sub(1).map(|at| value[at]))
                && boundary(value.get(index + word.len()).copied()) {
                output.extend(js::utf16(replacement));
                index += word.len();
                replaced = true;
                break;
            }
        }
        if !replaced { output.push(unit); index += 1; }
    }
    output
}

/// `parseAdditionalParamJsonValue`; `None` is `undefined`.
fn parse_additional_json(value: &Text, budget: &Budget) -> Result<Option<J>, BuildError> {
    let parse = |text: &Text| -> Result<Option<J>, BuildError> {
        let Some(source) = text.as_unicode() else { return Err(BuildError::Unsupported("additional-parameter-isolated-surrogate")) };
        match json::parse(source, budget) {
            Ok(parsed) => Ok(Some(js::from_value(&parsed, budget)?)),
            Err(Error::Syntax { .. }) => Ok(None),
            Err(other) => Err(other.into()),
        }
    };
    if let Some(parsed) = parse(value)? { return Ok(Some(parsed)); }
    let original = js::units(value);
    let normalized = normalize_relaxed_keywords(&original);
    if normalized == original { return Ok(None); }
    parse(&js::from_units(&normalized, budget)?)
}

const PROTOTYPE_NAMES: [&str; 12] = ["constructor", "toString", "toLocaleString", "valueOf", "hasOwnProperty", "isPrototypeOf",
    "propertyIsEnumerable", "__proto__", "__defineGetter__", "__defineSetter__", "__lookupGetter__", "__lookupSetter__"];

/// `setObjectValue(obj, key, value)`: a dotted key creates or walks nested objects.
pub(crate) fn set_object_value(object: &mut J, key: &Text, value: J, budget: &Budget) -> Result<(), BuildError> {
    let all = js::units(key);
    if let Some(dot) = all.iter().position(|&unit| unit == 46) {
        let first = js::from_units(&all[..dot], budget)?;
        let rest = js::from_units(&all[dot + 1..], budget)?;
        let reserved = PROTOTYPE_NAMES.iter().any(|name| first.equals_ascii(name.as_bytes()));
        let J::Obj(entries) = object else { return Err(BuildError::Unsupported("additional-parameter-receiver")) };
        let at = js::find(entries, &first);
        if reserved && at.is_none() { return Err(BuildError::Unsupported("additional-parameter-prototype-key")); }
        let present = at.map(|at| js::truthy(&entries[at].1)).unwrap_or(false);
        if !present { js::set(object, first.copy(budget)?, js::new_object(), budget)?; }
        let J::Obj(entries) = object else { unreachable!() };
        let at = js::find(entries, &first).expect("set above");
        if !matches!(entries[at].1, J::Obj(_)) { return Err(BuildError::Unsupported("additional-parameter-receiver")); }
        return set_object_value(&mut entries[at].1, &rest, value, budget);
    }
    if key.equals_ascii(b"__proto__") { return Err(BuildError::Unsupported("additional-parameter-prototype-key")); }
    js::set(object, key.copy(budget)?, value, budget)?;
    Ok(())
}

/// `applyAdditionalParameters(body, headers, additionalParams)`.
pub(crate) fn apply_additional_parameters(
    body: &mut J, headers: &mut J, params: &[(Text, Text)], budget: &Budget,
) -> Result<(), BuildError> {
    for (key, value) in params {
        if key.len_units() == 0 || value.len_units() == 0 { continue; }
        let is_header = js::starts_with(key, "header::");
        if value.equals_ascii(b"{{none}}") {
            if is_header { js::delete_text(headers, &replace_prefix(key, "header::", budget)?); } else { js::delete_text(body, key); }
            continue;
        }
        if is_header {
            js::set(headers, replace_prefix(key, "header::", budget)?, J::Str(value.copy(budget)?), budget)?;
            continue;
        }
        if js::starts_with(value, "json::") {
            if let Some(parsed) = parse_additional_json(&replace_prefix(value, "json::", budget)?, budget)? {
                set_object_value(body, key, parsed, budget)?;
            }
            continue;
        }
        let length = value.len_units();
        let quoted = |mark: u16| length >= 1 && value.units().next() == Some(mark) && value.units().last() == Some(mark);
        if quoted(34) || quoted(39) {
            let inner = value.slice(1, length.saturating_sub(1).max(1), budget)?;
            set_object_value(body, key, J::Str(if length == 1 { js::text("", budget)? } else { inner }), budget)?;
            continue;
        }
        if value.equals_ascii(b"true") || value.equals_ascii(b"false") {
            set_object_value(body, key, J::Bool(value.equals_ascii(b"true")), budget)?;
            continue;
        }
        if value.equals_ascii(b"null") { set_object_value(body, key, J::Null, budget)?; continue; }
        let number = risunest_ir::num::to_number(value, budget)?;
        set_object_value(body, key, if number.is_nan() { J::Str(value.copy(budget)?) } else { J::Num(number) }, budget)?;
    }
    Ok(())
}

/// `string.replace(prefix, '')` for a prefix known to start the string.
fn replace_prefix(value: &Text, prefix: &str, budget: &Budget) -> Result<Text, Error> {
    value.slice(prefix.encode_utf16().count(), value.len_units(), budget)
}
