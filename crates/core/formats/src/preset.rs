//! Preset files: the preset `importPreset` adds for a parsed `.json` file or a decoded `.risup` body
//! (`src/ts/storage/database.svelte.ts:2443-2604`, stored by `addPreset`, `src/ts/storage/presetWorkingSetOperations.ts:160-167`),
//! and the object `downloadPreset` writes for a stored preset (`database.svelte.ts:2379-2390`). Reading and writing the
//! file container stays with the caller; [`import_risup`] takes a `.risup` file from its decoded msgpack container.
//!
//! Declines, where TS follows JS semantics the port does not carry or throws a `TypeError`: `preset` (a file that is a
//! string or an array, which TS spreads by index), `protoKey` (an own `__proto__` key anywhere), `presetVersion` (a
//! truthy version that is not a number or boolean), `parameters` (a NovelAI preset whose `parameters` is not an
//! object, or whose scaled values are not numbers, booleans or `null`), `prompts` (a SillyTavern prompt lookup that
//! reaches a `null` entry), `temperature`, `frequency_penalty` and `presence_penalty` (SillyTavern sampling values that
//! are not numbers or booleans) and `toPrimitive` (`String()` of an object with an own `toString`).

use crate::ids::Ids;
use crate::risusave::{import_message_names, MESSAGE_NAMES};
use crate::js::{
    coalesce, concat_texts, copy, copy_or_undefined, empty_array, empty_object, field, field_mut, has_proto_key, nullish, push,
    remove, set, string, text, truthy, unsupported,
};
use risunest_ir::budget::Budget;
use risunest_ir::json::parse_json;
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;

/// `{template: presetTemplate, nai: prebuiltPresets.NAI}` (`database.svelte.ts:2041-2073`,
/// `src/ts/process/templates/templates.ts:6`), written by `benchmarks/rust-core-oracles/formats-constants.mjs` from the
/// value the preset oracle records.
const CONSTANTS: &str = include_str!("preset_constants.json");

/// The fields `downloadPreset` blanks before writing.
const SECRETS: [&str; 6] = ["openAIKey", "forceReplaceUrl", "forceReplaceUrl2", "proxyKey", "textgenWebUIStreamURL", "textgenWebUIBlockingURL"];

/// The preset template and the NAI preset, as `{template, nai}`.
pub fn constants(budget: &Budget) -> Result<Value, StageError> { Ok(parse_json(CONSTANTS, budget)?) }

fn constant(name: &str, budget: &Budget) -> Result<Value, StageError> {
    let mut constants = constants(budget)?;
    Ok(constants.take(name.as_bytes(), budget)?.expect("the constants hold both presets"))
}

/// A fresh copy of `presetTemplate`.
pub(crate) fn template(budget: &Budget) -> Result<Value, StageError> { constant("template", budget) }

/// The preset `importPreset` adds for `parsed`, the parsed `.json` file or the decoded `.risup` body, with the id
/// `addPreset` draws from `ids`.
pub fn import_preset(parsed: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if has_proto_key(parsed, budget)? { return Err(unsupported("protoKey")); }
    // `{...presetTemplate, ...parsed}`
    let mut pre = constant("template", budget)?;
    match parsed {
        Value::Object(entries) => {
            for (key, value) in entries.as_slice() {
                budget.work(1)?;
                pre.define(key.copy(budget)?, copy(value, budget)?, budget)?;
            }
        }
        Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) => {}
        _ => return Err(unsupported("preset")),
    }
    if field(&pre, "promptTemplate", budget)?.is_some() {
        let normalized = normalize_prompt_template(field(&pre, "promptTemplate", budget)?, budget)?;
        set(&mut pre, "promptTemplate", normalized, budget)?;
    }
    let mut preset = if novelai(field(&pre, "presetVersion", budget)?)? {
        novelai_preset(&pre, budget)?
    } else if let Some(order) = sillytavern_order(&pre, budget)? {
        sillytavern_preset(&pre, order, budget)?
    } else {
        import_message_names(&mut pre, budget)?;
        if nullish(field(&pre, "name", budget)?) { set(&mut pre, "name", text("Imported", budget)?, budget)?; }
        pre
    };
    // `{...clonePreset(preset), id: v4()}`
    set(&mut preset, "id", Value::Text(ids.next_id(budget)?), budget)?;
    Ok(preset)
}

/// A value of a decoded `.risup` or `.risupreset` container, as the caller's msgpack decoder read it.
pub enum Decoded<'a> {
    Data(&'a Value),
    /// msgpack binary, which a [`Value`] cannot hold.
    Binary(&'a [u8]),
}

/// The preset `importPreset` adds for a `.risup` or `.risupreset` file (`database.svelte.ts:2453-2463`), after the
/// caller has removed the RPack layer (`.risup` only), inflated the file and decoded its msgpack. `container` holds that
/// value's entries when it is a map, a repeated key keeping its last value as msgpackr's object does, and `None` for any
/// other value. `open` decrypts the selected body with the `risupreset` key and decodes its msgpack (`decryptBuffer`,
/// `decodeMsgpack`); it runs once, after the gate. The body then maps as [`import_preset`] maps a parsed `.json` file.
/// `open` must decline (return `Unsupported`) a body holding a value msgpack can carry and JSON cannot: binary, a
/// `Date`, a `Map`, a bigint or `undefined`. TS keeps those values in the imported preset, and no [`Value`] holds
/// them.
///
/// Declines, where TS throws a `TypeError`: `risupContainer` (the value is not a map, or its `presetVersion` is not
/// `0` or `2` or its `type` is not `'preset'`, which leaves TS without a preset to read) and `risupBody` (`preset ??
/// pres` is not binary, which WebCrypto refuses).
pub fn import_risup(
    container: Option<&[(&str, Decoded<'_>)]>,
    open: &mut dyn FnMut(&[u8]) -> Result<Value, StageError>,
    ids: &mut dyn Ids,
    budget: &Budget,
) -> Result<Value, StageError> {
    let Some(entries) = container else { return Err(unsupported("risupContainer")) };
    let read = |key: &str| -> Result<Option<&Decoded<'_>>, StageError> {
        budget.work(entries.len())?;
        Ok(entries.iter().rev().find(|(name, _)| *name == key).map(|(_, value)| value))
    };
    let data = |key: &str| -> Result<Option<&Value>, StageError> {
        Ok(match read(key)? { Some(Decoded::Data(value)) => Some(*value), _ => None })
    };
    // `(decoded.presetVersion === 0 || decoded.presetVersion === 2) && decoded.type === 'preset'`
    let version = matches!(data("presetVersion")?, Some(Value::Number(version)) if *version == 0.0 || *version == 2.0);
    let kind = matches!(data("type")?, Some(Value::Text(kind)) if kind.equals_ascii(b"preset"));
    if !version || !kind { return Err(unsupported("risupContainer")); }
    // `decoded.preset ?? decoded.pres`
    let body = match read("preset")? {
        Some(Decoded::Data(value)) if nullish(Some(value)) => read("pres")?,
        None => read("pres")?,
        selected => selected,
    };
    let Some(Decoded::Binary(bytes)) = body else { return Err(unsupported("risupBody")) };
    let parsed = open(bytes)?;
    import_preset(&parsed, ids, budget)
}

/// The object `downloadPreset` writes for a stored preset: the secrets blanked, then the message name settings under
/// their upstream names (`exportMessageNameSettings`).
pub fn export_preset(preset: &Value, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(preset, Value::Object(_)) { return Err(unsupported("preset")); }
    if has_proto_key(preset, budget)? { return Err(unsupported("protoKey")); }
    let mut exported = copy(preset, budget)?;
    for secret in SECRETS { set(&mut exported, secret, text("", budget)?, budget)?; }
    for (upstream, local) in MESSAGE_NAMES {
        if let Some(value) = field(&exported, local, budget)? {
            let value = copy(value, budget)?;
            set(&mut exported, upstream, value, budget)?;
        }
        remove(&mut exported, local, budget)?;
    }
    Ok(exported)
}

/// The already-exported preset as `JSON.stringify(exported, null, 2)` writes it.
pub fn exported_preset_json(exported: &Value, budget: &Budget) -> Result<Option<Text>, StageError> {
    Ok(crate::js::stringify_indented(exported, 2, budget)?)
}
/// `pre.presetVersion && pre.presetVersion >= 3`.
fn novelai(version: Option<&Value>) -> Result<bool, StageError> {
    match version {
        Some(Value::Number(version)) => Ok(*version >= 3.0),
        Some(Value::Bool(_)) => Ok(false),
        version if !truthy(version) => Ok(false),
        _ => Err(unsupported("presetVersion")),
    }
}

/// `value * factor` for the values the port carries: `undefined` is `NaN`, `null` and booleans are numbers.
fn times(value: Option<&Value>, factor: f64, name: &'static str) -> Result<f64, StageError> {
    match value {
        None => Ok(f64::NAN),
        Some(Value::Null) => Ok(0.0 * factor),
        Some(Value::Bool(flag)) => Ok(f64::from(u8::from(*flag)) * factor),
        Some(Value::Number(number)) => Ok(number * factor),
        Some(_) => Err(unsupported(name)),
    }
}

/// The NAI preset filled from a NovelAI preset's `parameters`.
fn novelai_preset(pre: &Value, budget: &Budget) -> Result<Value, StageError> {
    let parameters = match field(pre, "parameters", budget)? {
        Some(parameters @ Value::Object(_)) => parameters,
        _ => return Err(unsupported("parameters")),
    };
    let read = |key: &str| field(parameters, key, budget);
    let mut preset = constant("nai", budget)?;
    set(&mut preset, "temperature", Value::Number(times(read("temperature")?, 100.0, "parameters")?), budget)?;
    set(&mut preset, "maxResponse", copy_or_undefined(read("max_length")?, budget)?, budget)?;
    let presence = times(read("repetition_penalty_presence")?, 100.0, "parameters")?;
    {
        let settings = field_mut(&mut preset, "NAISettings", budget)?.expect("the NAI preset has NAISettings");
        for (local, upstream) in [
            ("topK", "top_k"), ("topP", "top_p"), ("topA", "top_a"), ("typicalp", "typical_p"),
            ("tailFreeSampling", "tail_free_sampling"), ("repetitionPenalty", "repetition_penalty"),
            ("repetitionPenaltyRange", "repetition_penalty_range"), ("repetitionPenaltySlope", "repetition_penalty_slope"),
            ("frequencyPenalty", "repetition_penalty_frequency"), ("repostitionPenaltyPresence", "repetition_penalty_presence"),
            ("cfg_scale", "cfg_scale"), ("mirostat_lr", "mirostat_lr"), ("mirostat_tau", "mirostat_tau"),
        ] {
            set(settings, local, copy_or_undefined(read(upstream)?, budget)?, budget)?;
        }
    }
    set(&mut preset, "PresensePenalty", Value::Number(presence), budget)?;
    let name = coalesce(field(pre, "name", budget)?, || text("Imported", budget), budget)?;
    set(&mut preset, "name", name, budget)?;
    Ok(preset)
}

/// `pre.prompt_order[0].order` when it and `pre.prompts` are arrays, which marks a SillyTavern preset.
fn sillytavern_order<'a>(pre: &'a Value, budget: &Budget) -> Result<Option<&'a [Value]>, StageError> {
    let first = match field(pre, "prompt_order", budget)? {
        Some(Value::Array(orders)) => orders.as_slice().first(),
        Some(orders @ Value::Object(_)) => field(orders, "0", budget)?,
        _ => None,
    };
    let order = match first {
        Some(first @ Value::Object(_)) => field(first, "order", budget)?,
        _ => None,
    };
    match (order, field(pre, "prompts", budget)?) {
        (Some(Value::Array(order)), Some(Value::Array(_))) => Ok(Some(order.as_slice())),
        _ => Ok(None),
    }
}

/// `a === b` for a `b` that is not `undefined`. Objects from parsed data are never the same reference.
fn strictly_equal(a: Option<&Value>, b: &Value) -> bool {
    match (a, b) {
        (Some(Value::Text(a)), Value::Text(b)) => a.equals(b),
        (Some(Value::Number(a)), Value::Number(b)) => a == b,
        (Some(Value::Bool(a)), Value::Bool(b)) => a == b,
        (Some(Value::Null), Value::Null) => true,
        _ => false,
    }
}

/// `{type, type2, text: p.content ?? "", role: p.role ?? "system"}`.
fn prompt_item(kind: &str, kind2: &str, prompt: &Value, budget: &Budget) -> Result<Value, StageError> {
    let mut item = empty_object(budget)?;
    set(&mut item, "type", text(kind, budget)?, budget)?;
    set(&mut item, "type2", text(kind2, budget)?, budget)?;
    set(&mut item, "text", coalesce(field(prompt, "content", budget)?, || text("", budget), budget)?, budget)?;
    set(&mut item, "role", coalesce(field(prompt, "role", budget)?, || text("system", budget), budget)?, budget)?;
    Ok(item)
}

fn typed(kind: &str, budget: &Budget) -> Result<Value, StageError> {
    let mut item = empty_object(budget)?;
    set(&mut item, "type", text(kind, budget)?, budget)?;
    Ok(item)
}

/// The template filled from a SillyTavern preset's enabled prompts.
fn sillytavern_preset(pre: &Value, order: &[Value], budget: &Budget) -> Result<Value, StageError> {
    let Some(Value::Array(prompts)) = field(pre, "prompts", budget)? else { unreachable!("checked by sillytavern_order") };
    let mut preset = constant("template", budget)?;
    set(&mut preset, "promptTemplate", empty_array(budget)?, budget)?;
    let sampling = |key: &'static str, fallback: f64| -> Result<f64, StageError> {
        match field(pre, key, budget)? {
            Some(Value::Null) | None => Ok(fallback * 100.0),
            value => times(value, 100.0, key),
        }
    };
    set(&mut preset, "temperature", Value::Number(sampling("temperature", 0.8)?), budget)?;
    set(&mut preset, "frequencyPenalty", Value::Number(sampling("frequency_penalty", 0.7)?), budget)?;
    let presence = times(field(pre, "presence_penalty", budget)?, 0.7, "presence_penalty")? * 100.0;
    set(&mut preset, "PresensePenalty", Value::Number(presence), budget)?;
    set(&mut preset, "top_p", coalesce(field(pre, "top_p", budget)?, || Ok(Value::Number(1.0)), budget)?, budget)?;
    let mut template = empty_array(budget)?;
    for entry in order {
        budget.work(1)?;
        if !truthy(match entry { Value::Object(_) => field(entry, "enabled", budget)?, _ => None }) { continue; }
        let identifier = coalesce(field(entry, "identifier", budget)?, || text("", budget), budget)?;
        let mut found = None;
        for prompt in prompts.as_slice() {
            budget.work(1)?;
            match prompt {
                Value::Undefined | Value::Null => return Err(unsupported("prompts")),
                Value::Object(_) if strictly_equal(field(prompt, "identifier", budget)?, &identifier) => { found = Some(prompt); break; }
                _ => {}
            }
        }
        let Some(prompt) = found else { continue };
        let name = match &identifier { Value::Text(name) => String::from_utf16_lossy(&name.units().collect::<Vec<_>>()), _ => String::new() };
        let item = match name.as_str() {
            "main" => prompt_item("plain", "main", prompt, budget)?,
            "jailbreak" | "nsfw" => prompt_item("jailbreak", "normal", prompt, budget)?,
            "dialogueExamples" | "charPersonality" | "scenario" | "worldInfoAfter" => continue,
            "chatHistory" => {
                let mut item = typed("chat", budget)?;
                set(&mut item, "rangeEnd", text("end", budget)?, budget)?;
                set(&mut item, "rangeStart", Value::Number(0.0), budget)?;
                item
            }
            "worldInfoBefore" => typed("lorebook", budget)?,
            "charDescription" => typed("description", budget)?,
            "personaDescription" => typed("persona", budget)?,
            _ => prompt_item("plain", "normal", prompt, budget)?,
        };
        push(&mut template, item)?;
    }
    if truthy(field(pre, "assistant_prefill", budget)?) {
        push(&mut template, typed("postEverything", budget)?)?;
        let prefill = string(field(pre, "assistant_prefill", budget)?, budget)?;
        let head = Text::from_str("{{#if {{prefill_supported}}}}", budget)?;
        let tail = Text::from_str("{{/if}}", budget)?;
        let mut item = typed("plain", budget)?;
        set(&mut item, "type2", text("main", budget)?, budget)?;
        set(&mut item, "text", Value::Text(concat_texts(&[&head, &prefill, &tail], budget)?), budget)?;
        set(&mut item, "role", text("bot", budget)?, budget)?;
        push(&mut template, item)?;
    }
    set(&mut preset, "promptTemplate", normalize_prompt_template(Some(&template), budget)?, budget)?;
    set(&mut preset, "name", text("Imported ST Preset", budget)?, budget)?;
    Ok(preset)
}

/// `normalizePromptRole` (`database.svelte.ts:2606-2614`).
fn prompt_role(role: Option<&Value>) -> Option<&'static str> {
    let Some(Value::Text(role)) = role else { return None };
    ["user", "bot", "system"].into_iter().find(|name| role.equals_ascii(name.as_bytes()))
        .or_else(|| ["assistant", "char"].iter().any(|name| role.equals_ascii(name.as_bytes())).then_some("bot"))
}

/// `normalizeCacheRole` (`database.svelte.ts:2616-2624`).
fn cache_role(role: Option<&Value>) -> &'static str {
    let Some(Value::Text(role)) = role else { return "all" };
    if let Some(name) = ["user", "assistant", "system", "all"].into_iter().find(|name| role.equals_ascii(name.as_bytes())) { return name; }
    if ["bot", "char"].iter().any(|name| role.equals_ascii(name.as_bytes())) { "assistant" } else { "all" }
}

/// `normalizePromptTemplate` (`database.svelte.ts:2626-2657`): `null` for anything but an array, otherwise a copy with
/// each item's roles normalized.
pub(crate) fn normalize_prompt_template(template: Option<&Value>, budget: &Budget) -> Result<Value, StageError> {
    let Some(template @ Value::Array(_)) = template else { return Ok(Value::Null) };
    let mut normalized = copy(template, budget)?;
    let Value::Array(items) = &mut normalized else { unreachable!("an array copies as an array") };
    for item in items.as_mut_slice() {
        budget.work(1)?;
        if !matches!(item, Value::Object(_)) { continue; }
        let kind = match field(item, "type", budget)? {
            Some(Value::Text(kind)) => String::from_utf16_lossy(&kind.units().collect::<Vec<_>>()),
            _ => continue,
        };
        match kind.as_str() {
            "plain" | "jailbreak" | "cot" => {
                let role = prompt_role(field(item, "role", budget)?).unwrap_or("system");
                set(item, "role", text(role, budget)?, budget)?;
            }
            "persona" | "description" | "authornote" | "memory" => {
                let role2 = field(item, "role2", budget)?;
                if !nullish(role2) {
                    let role = prompt_role(role2).unwrap_or("system");
                    set(item, "role2", text(role, budget)?, budget)?;
                }
            }
            "cache" => {
                let role = cache_role(field(item, "role", budget)?);
                set(item, "role", text(role, budget)?, budget)?;
            }
            _ => {}
        }
    }
    Ok(normalized)
}
