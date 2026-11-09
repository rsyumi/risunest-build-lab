//! `normalizeDatabaseDefaults` (`src/ts/storage/database.svelte.ts:41-755`), the first step of
//! `prepareDatabaseForPersistence`: every missing setting gets its default, a few older shapes are migrated, and each
//! chat's streaming state is cleared. A setting TS fills with `??=` or `checkNullish` takes the value an empty
//! database normalizes to, read from `defaults_constants.json`; the steps that read other settings, merge lists or
//! rebuild a value are written out here in TS order, so new keys land where TS appends them.
//!
//! Declines, where TS throws a `TypeError` or changes a value JSON cannot show: `database` (not an object),
//! `protoKey` (an own `__proto__` key anywhere), `NAIImgConfig`, `NAIsettings`, `google`, `promptSettings`,
//! `customTextTheme` and `seperateParameters` (not a plain object where TS fills a nested default), `loreBook` (not an
//! array where TS compares its length with `loreBookPage`), `loreBookPage` (not a number there), `formatingOrder` (not
//! an array), `botPresets` (truthy and not an array of objects), `hypaV3Presets` (not an array, or a `null` entry),
//! `hotkeys` (truthy and not an array, or a `null` entry), `fallbackModels` (not an object holding the five lists),
//! `plugins` (not an array where TS reads its length), `characters` (not an array, or a `null` entry), `chats` (a list
//! that is neither an array nor an empty string, or an entry that is not an object) and `toPrimitive` (a chat page
//! count that is an object with an own `toString`).

use crate::ids::Ids;
use crate::js::{
    array, coalesce, copy, copy_or_undefined, empty_array, empty_object, field, field_mut, has_proto_key, insert, nullish,
    round, set, string, text, truthy, type_of, unsupported,
};
use crate::preset::{normalize_prompt_template, template};
use risunest_ir::budget::{Budget, Error, OwnedVec};
use risunest_ir::json::parse_json;
use risunest_ir::num::to_number;
use risunest_ir::stage::StageError;
use risunest_ir::value::Value;
use std::collections::HashSet;

/// `{database, naiSettings, hotkeys, hypaV3Settings, inlay, chatLoadPages, character, sdData, prompts}`: the database
/// `normalizeDatabaseDefaults({})` returns and the constants the preparation steps copy, written by
/// `benchmarks/rust-core-oracles/formats-constants.mjs` from the value the preparation oracle records.
const CONSTANTS: &str = include_str!("defaults_constants.json");

/// Where the app runs, which `isTauri` (`src/ts/platform.ts:20`) reads from the WebView.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// The browser build.
    Web,
    /// A Tauri app, desktop or mobile.
    Tauri,
}

/// The recorded defaults and constants.
pub fn constants(budget: &Budget) -> Result<Value, StageError> { Ok(parse_json(CONSTANTS, budget)?) }

/// The constant at `path` in the recorded constants.
pub(crate) fn constant<'a>(constants: &'a Value, path: &[&str], budget: &Budget) -> Result<&'a Value, Error> {
    let mut value = constants;
    for key in path {
        value = value.get(key.as_bytes(), budget)?.unwrap_or_else(|| unreachable!("the constants hold {path:?}"));
    }
    Ok(value)
}

/// `normalizeDatabaseDefaults(database)` on `platform`. `ids` serves the two draws TS makes, for a missing preset list
/// and then a missing persona list.
pub fn normalize_database_defaults(database: &Value, platform: Platform, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(database, Value::Object(_)) { return Err(unsupported("database")); }
    if has_proto_key(database, budget)? { return Err(unsupported("protoKey")); }
    let mut database = copy(database, budget)?;
    normalize(&mut database, platform, ids, &constants(budget)?, budget)?;
    Ok(database)
}

/// `data[key] ??= value()`; `checkNullish` tests the same.
fn fill_with(data: &mut Value, key: &str, value: impl FnOnce() -> Result<Value, StageError>, budget: &Budget) -> Result<(), StageError> {
    if nullish(field(data, key, budget)?) {
        let value = value()?;
        set(data, key, value, budget)?;
    }
    Ok(())
}

/// `data[key] ??= <default>` for each key, with the value an empty database normalizes to.
fn fill(data: &mut Value, defaults: &Value, keys: &[&str], budget: &Budget) -> Result<(), StageError> {
    for key in keys {
        fill_with(data, key, || Ok(copy(constant(defaults, &[key], budget)?, budget)?), budget)?;
    }
    Ok(())
}

/// `data[key]` as a plain object TS writes into, or the decline `name`.
fn object_mut<'a>(data: &'a mut Value, key: &str, name: &'static str, budget: &Budget) -> Result<&'a mut Value, StageError> {
    match field_mut(data, key, budget)? {
        Some(value @ Value::Object(_)) => Ok(value),
        _ => Err(unsupported(name)),
    }
}

fn is_text(value: Option<&Value>, expected: &str) -> bool { matches!(value, Some(Value::Text(text)) if text.equals_ascii(expected.as_bytes())) }

/// `typeof value !== 'number' || Number.isNaN(value)`.
fn not_number(value: Option<&Value>) -> bool { !matches!(value, Some(Value::Number(number)) if !number.is_nan()) }

/// `if (typeof record.openrouterProvider === 'string')`: the old provider name becomes `{order, only, ignore}`.
fn migrate_provider(record: &mut Value, budget: &Budget) -> Result<(), StageError> {
    let order = match field(record, "openrouterProvider", budget)? {
        Some(Value::Text(old)) if old.len_units() > 0 => array(vec![Value::Text(old.copy(budget)?)], budget)?,
        Some(Value::Text(_)) => empty_array(budget)?,
        _ => return Ok(()),
    };
    let mut provider = empty_object(budget)?;
    set(&mut provider, "order", order, budget)?;
    set(&mut provider, "only", empty_array(budget)?, budget)?;
    set(&mut provider, "ignore", empty_array(budget)?, budget)?;
    set(record, "openrouterProvider", provider, budget)?;
    Ok(())
}

/// `record.promptTemplate = normalizePromptTemplate(record.promptTemplate)` when it is an array.
fn normalize_template(record: &mut Value, budget: &Budget) -> Result<(), StageError> {
    if let Some(template @ Value::Array(_)) = field(record, "promptTemplate", budget)? {
        let normalized = normalize_prompt_template(Some(template), budget)?;
        set(record, "promptTemplate", normalized, budget)?;
    }
    Ok(())
}

/// `data.aiModel === 'ollama-cloud' || data.subModel === 'ollama-cloud'`.
fn ollama_cloud(data: &Value, budget: &Budget) -> Result<bool, StageError> {
    Ok(is_text(field(data, "aiModel", budget)?, "ollama-cloud") || is_text(field(data, "subModel", budget)?, "ollama-cloud"))
}

fn numbered(prefix: &str, index: usize, budget: &Budget) -> Result<Value, StageError> { Ok(text(&format!("{prefix}{}", index + 1), budget)?) }

/// `createHypaV3Preset(name, existing)` (`src/ts/process/memory/hypav3.ts:1837-1881`): the default settings, each
/// overridden by an own entry of a plain-object `existing` whose key names a setting and whose `typeof` matches.
fn hypa_v3_preset(name: Value, existing: Option<&Value>, template: &Value, budget: &Budget) -> Result<Value, StageError> {
    let mut settings = copy(template, budget)?;
    if let Some(Value::Object(entries)) = existing {
        let Value::Object(slots) = &mut settings else { unreachable!("the settings are an object") };
        for (key, value) in entries.as_slice() {
            budget.work(slots.as_slice().len())?;
            // `key in settings` also finds `Object.prototype` members, whose `typeof` is 'function' and never matches.
            let Some((_, slot)) = slots.as_mut_slice().iter_mut().find(|(name, _)| name.equals(key)) else { continue };
            if type_of(Some(value)) == type_of(Some(&*slot)) { *slot = copy(value, budget)?; }
        }
    }
    let mut preset = empty_object(budget)?;
    set(&mut preset, "name", name, budget)?;
    set(&mut preset, "settings", settings, budget)?;
    Ok(preset)
}

/// The `hypaV3Presets` steps (`database.svelte.ts:600-613`): a missing list gets a "Default" preset from
/// `supaMemoryPrompt` and `hypaV3Settings`, then every preset is rebuilt by `createHypaV3Preset`.
fn hypa_v3_presets(data: &mut Value, template: &Value, budget: &Budget) -> Result<(), StageError> {
    if nullish(field(data, "hypaV3Presets", budget)?) {
        // `{summarizationPrompt: data.supaMemoryPrompt ? data.supaMemoryPrompt : "", ...data.hypaV3Settings}`
        let summary = match field(data, "supaMemoryPrompt", budget)? {
            Some(prompt) if prompt.truthy() => copy(prompt, budget)?,
            _ => text("", budget)?,
        };
        let mut existing = empty_object(budget)?;
        set(&mut existing, "summarizationPrompt", summary, budget)?;
        // A string or an array spreads index keys, which name no setting.
        if let Some(Value::Object(entries)) = field(data, "hypaV3Settings", budget)? {
            for (key, value) in entries.as_slice() {
                budget.work(1)?;
                existing.define(key.copy(budget)?, copy(value, budget)?, budget)?;
            }
        }
        let preset = hypa_v3_preset(text("Default", budget)?, Some(&existing), template, budget)?;
        set(data, "hypaV3Presets", array(vec![preset], budget)?, budget)?;
    }
    let Some(Value::Array(presets)) = field_mut(data, "hypaV3Presets", budget)? else { return Err(unsupported("hypaV3Presets")) };
    if presets.as_slice().is_empty() { return Ok(()); }
    let mut rebuilt = OwnedVec::new(budget)?;
    for (index, preset) in presets.as_slice().iter().enumerate() {
        budget.work(1)?;
        // `preset.name` and `preset.settings` read `undefined` on any other value but `null`.
        let (name, settings) = match preset {
            Value::Undefined | Value::Null => return Err(unsupported("hypaV3Presets")),
            Value::Object(_) => (field(preset, "name", budget)?, field(preset, "settings", budget)?),
            _ => (None, None),
        };
        let name = if truthy(name) { copy_or_undefined(name, budget)? } else { numbered("Preset ", index, budget)? };
        rebuilt.push(hypa_v3_preset(name, settings.filter(|settings| settings.truthy()), template, budget)?)?;
    }
    *presets = rebuilt;
    Ok(())
}

/// `createTranslatorPreset(name, existing)` (`src/ts/translator/presets.ts:91-102`) over the fields `existing` holds.
fn translator_preset(name: Value, prompt: Option<&Value>, max_response: Option<&Value>, budget: &Budget) -> Result<Value, StageError> {
    let mut preset = empty_object(budget)?;
    set(&mut preset, "name", name, budget)?;
    let prompt = match prompt { Some(prompt @ Value::Text(_)) => copy(prompt, budget)?, _ => text("", budget)? };
    set(&mut preset, "prompt", prompt, budget)?;
    let max_response = match max_response { Some(Value::Number(max)) if max.is_finite() => *max, _ => 1000.0 };
    set(&mut preset, "maxResponse", Value::Number(max_response), budget)?;
    Ok(preset)
}

/// `normalizeTranslatorPresetState(data)` (`src/ts/translator/presets.ts:104-145`).
fn translator_presets(data: &mut Value, budget: &Budget) -> Result<(), StageError> {
    // `getDefaultTranslatorPreset`: the legacy prompt and response length.
    let fallback = translator_preset(text("Default", budget)?, field(data, "translatorPrompt", budget)?, field(data, "translatorMaxResponse", budget)?, budget)?;
    let fallback = [fallback];
    let source = match field(data, "translatorPresets", budget)? {
        Some(Value::Array(presets)) if !presets.as_slice().is_empty() => presets.as_slice(),
        _ => &fallback[..],
    };
    let mut presets = OwnedVec::new(budget)?;
    for (index, preset) in source.iter().enumerate() {
        budget.work(1)?;
        // `isRecord` also accepts arrays, which hold none of the three fields.
        let read = |key: &str| -> Result<Option<&Value>, Error> { if matches!(preset, Value::Object(_)) { field(preset, key, budget) } else { Ok(None) } };
        let name = match read("name")? {
            Some(name @ Value::Text(text)) if text.trim(budget)?.len_units() > 0 => copy(name, budget)?,
            _ => numbered("Preset ", index, budget)?,
        };
        presets.push(translator_preset(name, read("prompt")?, read("maxResponse")?, budget)?)?;
    }
    let count = presets.as_slice().len();
    set(data, "translatorPresets", Value::Array(presets), budget)?;
    let requested = match field(data, "translatorPresetId", budget)? {
        Some(Value::Number(id)) if id.is_finite() && id.trunc() == *id => *id,
        _ => 0.0,
    };
    let id = requested.max(0.0).min(count.saturating_sub(1) as f64);
    set(data, "translatorPresetId", Value::Number(id), budget)?;
    // `syncCurrentTranslatorPresetToLegacyFields`: every normalized preset is valid.
    let Some(Value::Array(presets)) = field(data, "translatorPresets", budget)? else { unreachable!("set above") };
    let current = &presets.as_slice()[id as usize];
    let prompt = copy_or_undefined(field(current, "prompt", budget)?, budget)?;
    let max_response = copy_or_undefined(field(current, "maxResponse", budget)?, budget)?;
    set(data, "translatorPrompt", prompt, budget)?;
    set(data, "translatorMaxResponse", max_response, budget)?;
    Ok(())
}

/// The hotkey steps (`database.svelte.ts:640-654`): a falsy list becomes the defaults, a list gets each default whose
/// action it lacks, and `scrollToActiveChar` goes when that feature is off.
fn hotkeys(data: &mut Value, defaults: &Value, budget: &Budget) -> Result<(), StageError> {
    if !truthy(field(data, "hotkeys", budget)?) {
        set(data, "hotkeys", copy(defaults, budget)?, budget)?;
    } else {
        let Some(Value::Array(list)) = field_mut(data, "hotkeys", budget)? else { return Err(unsupported("hotkeys")) };
        let mut actions: HashSet<Vec<u16>> = HashSet::new();
        for hotkey in list.as_slice() {
            budget.work(1)?;
            match hotkey {
                Value::Undefined | Value::Null => return Err(unsupported("hotkeys")),
                Value::Object(_) => {
                    // The defaults' actions are strings, so only a string action can match one.
                    if let Some(Value::Text(action)) = field(hotkey, "action", budget)? { actions.insert(action.units().collect()); }
                }
                _ => {}
            }
        }
        let Value::Array(defaults) = defaults else { unreachable!("the default hotkeys are an array") };
        for hotkey in defaults.as_slice() {
            budget.work(1)?;
            let Some(Value::Text(action)) = field(hotkey, "action", budget)? else { unreachable!("each default hotkey has an action") };
            if !actions.contains(&action.units().collect::<Vec<_>>()) { list.push(copy(hotkey, budget)?)?; }
        }
    }
    if matches!(field(data, "enableScrollToActiveChar", budget)?, Some(Value::Bool(false))) {
        let Some(Value::Array(list)) = field_mut(data, "hotkeys", budget)? else { unreachable!("an array now") };
        let mut kept = OwnedVec::new(budget)?;
        for hotkey in list.as_mut_slice() {
            budget.work(1)?;
            let scroll = matches!(hotkey, Value::Object(_)) && is_text(field(hotkey, "action", budget)?, "scrollToActiveChar");
            if !scroll { kept.push(std::mem::replace(hotkey, Value::Undefined))?; }
        }
        *list = kept;
    }
    Ok(())
}

/// `data.fallbackModels` rebuilt as `{model, memory, emotion, translate, otherAx}` without empty strings; a missing
/// value starts as five empty lists.
fn fallback_models(data: &mut Value, budget: &Budget) -> Result<(), StageError> {
    let mut rebuilt = empty_object(budget)?;
    let current = field(data, "fallbackModels", budget)?;
    for key in ["model", "memory", "emotion", "translate", "otherAx"] {
        let mut kept = OwnedVec::new(budget)?;
        match current {
            None | Some(Value::Null | Value::Undefined) => {}
            Some(models @ Value::Object(_)) => {
                let Some(Value::Array(models)) = field(models, key, budget)? else { return Err(unsupported("fallbackModels")) };
                for model in models.as_slice() {
                    budget.work(1)?;
                    if !matches!(model, Value::Text(name) if name.len_units() == 0) { kept.push(copy(model, budget)?)?; }
                }
            }
            Some(_) => return Err(unsupported("fallbackModels")),
        }
        set(&mut rebuilt, key, Value::Array(kept), budget)?;
    }
    set(data, "fallbackModels", rebuilt, budget)?;
    Ok(())
}

/// `normalizeChatLoadPages(value, fallback)` (`src/ts/chatLoadPages.ts:4-15`).
fn chat_load_pages(value: Option<&Value>, fallback: f64, budget: &Budget) -> Result<f64, StageError> {
    let fallback = if fallback.is_finite() && fallback >= 1.0 { fallback.floor() } else { 1.0 };
    // `Number(value)`: an array or object converts through its string form.
    let number = match value {
        None | Some(Value::Undefined) => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Value::Number(number)) => *number,
        Some(Value::Text(text)) => to_number(text, budget)?,
        other => to_number(&string(other, budget)?, budget)?,
    };
    Ok(if !number.is_finite() || number < 1.0 { fallback } else { number.floor() })
}

/// The inlay settings (`database.svelte.ts:700-714`) through `normalizeInlayEncodeOptions`
/// (`src/ts/storage/blobStore.ts:24-44`), which only accepts finite numbers and booleans as they are.
fn inlay_options(data: &mut Value, inlay: &Value, budget: &Budget) -> Result<(), StageError> {
    let format = match field(data, "risunestInlayFormat", budget)? {
        Some(format) if is_text(Some(format), "png") || is_text(Some(format), "original") => copy(format, budget)?,
        _ => text("webp", budget)?,
    };
    set(data, "risunestInlayFormat", format, budget)?;
    let defaults = constant(inlay, &["defaults"], budget)?;
    let number = |value: &Value| match value { Value::Number(number) => *number, _ => unreachable!("the inlay limits are numbers") };
    let finite = |key: &str, default: &str| -> Result<f64, StageError> {
        Ok(match field(data, key, budget)? {
            Some(Value::Number(value)) if value.is_finite() => *value,
            _ => number(constant(defaults, &[default], budget)?),
        })
    };
    let quality = round(finite("risunestInlayWebpQuality", "quality")?).max(1.0).min(100.0);
    let max_dimension = round(finite("risunestInlayMaxDimension", "maxDimension")?).max(0.0).min(number(constant(inlay, &["maxDimension"], budget)?));
    let skip = match field(data, "risunestInlaySkipReencode", budget)? {
        Some(Value::Bool(skip)) => *skip,
        _ => matches!(constant(defaults, &["skipReencode"], budget)?, Value::Bool(true)),
    };
    let fps = round(finite("risunestInlayAnimationMaxFps", "animationMaxFps")?).max(0.0).min(number(constant(inlay, &["maxFps"], budget)?));
    set(data, "risunestInlayWebpQuality", Value::Number(quality), budget)?;
    set(data, "risunestInlayMaxDimension", Value::Number(max_dimension), budget)?;
    set(data, "risunestInlaySkipReencode", Value::Bool(skip), budget)?;
    set(data, "risunestInlayAnimationMaxFps", Value::Number(fps), budget)?;
    Ok(())
}

/// `chat.isStreaming = false` and `chat.activeStreamingDisplayOptimizationMode = undefined` on every chat.
fn clear_streaming(data: &mut Value, budget: &Budget) -> Result<(), StageError> {
    let Some(Value::Array(characters)) = field_mut(data, "characters", budget)? else { return Err(unsupported("characters")) };
    for character in characters.as_mut_slice() {
        budget.work(1)?;
        match character {
            Value::Undefined | Value::Null => return Err(unsupported("characters")),
            // `char.chats` reads `undefined` on any other value.
            Value::Object(_) => {}
            _ => continue,
        }
        match field_mut(character, "chats", budget)? {
            None | Some(Value::Undefined | Value::Null) => {}
            Some(Value::Text(chats)) if chats.len_units() == 0 => {}
            Some(Value::Array(chats)) => {
                for chat in chats.as_mut_slice() {
                    budget.work(1)?;
                    if !matches!(chat, Value::Object(_)) { return Err(unsupported("chats")); }
                    set(chat, "isStreaming", Value::Bool(false), budget)?;
                    set(chat, "activeStreamingDisplayOptimizationMode", Value::Undefined, budget)?;
                }
            }
            Some(_) => return Err(unsupported("chats")),
        }
    }
    Ok(())
}

/// The steps of `normalizeDatabaseDefaults` on `data`, in TS order.
pub(crate) fn normalize(data: &mut Value, platform: Platform, ids: &mut dyn Ids, constants: &Value, budget: &Budget) -> Result<(), StageError> {
    let d = constant(constants, &["database"], budget)?;
    fill(data, d, &[
        "characters", "apiType", "openAIKey", "mainPrompt", "jailbreak", "globalNote", "temperature", "maxContext", "maxResponse",
        "frequencyPenalty", "PresensePenalty", "aiModel", "jailbreakToggle", "formatingOrder", "loreBookDepth", "loreBookToken",
        "username", "userIcon", "userNote", "additionalPrompt", "descriptionPrefix", "forceReplaceUrl", "language", "translator",
        "translatorMaxResponse", "currentPluginProvider", "plugins", "zoomsize", "customBackground", "textgenWebUIStreamURL",
        "textgenWebUIBlockingURL", "autoTranslate", "fullScreen", "playMessage", "iconsize", "theme", "subModel", "waifuWidth",
        "waifuWidth2", "emotionPrompt", "proxyKey",
    ], budget)?;
    // `{...safeStructuredClone(presetTemplate), id: v4()}` named "Default".
    fill_with(data, "botPresets", || {
        let mut preset = template(budget)?;
        set(&mut preset, "id", Value::Text(ids.next_id(budget)?), budget)?;
        set(&mut preset, "name", text("Default", budget)?, budget)?;
        Ok(array(vec![preset], budget)?)
    }, budget)?;
    fill(data, d, &["botPresetsId"], budget)?;
    normalize_template(data, budget)?;
    fill(data, d, &[
        "sdProvider", "webUiUrl", "sdSteps", "sdCFG", "NAIImgUrl", "NAIApiKey", "NAIImgModel", "NAII2I", "NAIREF", "textTheme",
        "emotionPrompt2", "requestRetrys", "useSayNothing", "bias", "showUnrecommended", "elevenLabKey", "voicevoxUrl",
        "supaMemoryPrompt", "showMemoryLimit", "showFirstMessagePages", "supaMemoryKey", "hypaMemoryKey", "voyageApiKey",
        "supaModelType", "askRemoval", "sdConfig", "NAIImgConfig",
    ], budget)?;
    {
        // The NAI v4 fields of an older image config; the values are those of the default config.
        let current = constant(d, &["NAIImgConfig"], budget)?;
        let config = object_mut(data, "NAIImgConfig", "NAIImgConfig", budget)?;
        if nullish(field(config, "v4_prompt", budget)?) {
            for key in ["autoSmea", "use_coords", "legacy_uc"] { set(config, key, Value::Bool(false), budget)?; }
            for key in ["v4_prompt", "v4_negative_prompt"] { set(config, key, copy(constant(current, &[key], budget)?, budget)?, budget)?; }
        }
    }
    fill(data, d, &["customTextTheme", "hordeConfig", "novelai"], budget)?;
    if nullish(field(data, "loreBook", budget)?) {
        set(data, "loreBookPage", Value::Number(0.0), budget)?;
        set(data, "loreBook", copy(constant(d, &["loreBook"], budget)?, budget)?, budget)?;
    }
    // `checkNullish(data.loreBookPage) || data.loreBook.length < data.loreBookPage`
    let reset = nullish(field(data, "loreBookPage", budget)?) || {
        let Some(Value::Array(book)) = field(data, "loreBook", budget)? else { return Err(unsupported("loreBook")) };
        let Some(Value::Number(page)) = field(data, "loreBookPage", budget)? else { return Err(unsupported("loreBookPage")) };
        (book.as_slice().len() as f64) < *page
    };
    if reset { set(data, "loreBookPage", Value::Number(0.0), budget)?; }
    fill(data, d, &[
        "globalscript", "sendWithEnter", "autoSuggestPrompt", "autoSuggestPrefix", "OAIPrediction", "autoSuggestClean",
        "imageCompression", "enableBlockPartialEdit", "enableDragPartialEdit",
    ], budget)?;
    {
        let Some(Value::Array(order)) = field_mut(data, "formatingOrder", budget)? else { return Err(unsupported("formatingOrder")) };
        if !order.as_slice().iter().any(|entry| is_text(Some(entry), "personaPrompt")) {
            // `splice(indexOf('main'), 0, ...)`: without 'main' the index is -1, before the last entry.
            let length = order.as_slice().len();
            let index = order.as_slice().iter().position(|entry| is_text(Some(entry), "main")).unwrap_or(length.saturating_sub(1));
            insert(order, index, text("personaPrompt", budget)?, budget)?;
        }
    }
    fill(data, d, &["selectedPersona", "personaPrompt"], budget)?;
    if nullish(field(data, "personas", budget)?) {
        let mut persona = empty_object(budget)?;
        set(&mut persona, "id", Value::Text(ids.next_id(budget)?), budget)?;
        set(&mut persona, "name", copy_or_undefined(field(data, "username", budget)?, budget)?, budget)?;
        set(&mut persona, "personaPrompt", text("", budget)?, budget)?;
        set(&mut persona, "icon", copy_or_undefined(field(data, "userIcon", budget)?, budget)?, budget)?;
        set(&mut persona, "note", copy_or_undefined(field(data, "userNote", budget)?, budget)?, budget)?;
        set(&mut persona, "largePortrait", Value::Bool(false), budget)?;
        set(data, "personas", array(vec![persona], budget)?, budget)?;
    }
    fill(data, d, &[
        "classicMaxWidth", "ooba", "ainconfig", "openrouterKey", "openrouterRequestModel", "nanogptKey", "nanogptRequestModel",
        "nanogptRequestModelName", "nanogptProvider", "nanogptSubscriptionState", "nanogptUseSubscriptionEndpoint",
    ], budget)?;
    fill_with(data, "NAIsettings", || Ok(copy(constant(constants, &["naiSettings"], budget)?, budget)?), budget)?;
    fill(data, d, &["assetWidth", "animationSpeed", "colorScheme", "colorSchemeName"], budget)?;
    if nullish(field(data, "customColorScheme", budget)?) {
        let scheme = if is_text(field(data, "colorSchemeName", budget)?, "custom") {
            copy_or_undefined(field(data, "colorScheme", budget)?, budget)?
        } else {
            copy(constant(d, &["colorScheme"], budget)?, budget)?
        };
        set(data, "customColorScheme", scheme, budget)?;
    }
    fill_with(object_mut(data, "NAIsettings", "NAIsettings", budget)?, "starter", || Ok(text("", budget)?), budget)?;
    fill(data, d, &[
        "hypaModel", "mancerHeader", "emotionProcesser", "translatorType", "htmlTranslation", "deeplOptions", "deeplXOptions",
        "NAIadventure", "NAIappendName",
    ], budget)?;
    {
        let settings = object_mut(data, "NAIsettings", "NAIsettings", budget)?;
        for (key, value) in [("cfg_scale", 1.0), ("mirostat_tau", 0.0), ("mirostat_lr", 1.0)] {
            fill_with(settings, key, || Ok(Value::Number(value)), budget)?;
        }
    }
    fill(data, d, &["autofillRequestUrl", "customProxyRequestModel", "generationSeed", "newOAIHandle", "localNetworkMode"], budget)?;
    if !matches!(field(data, "localNetworkMode", budget)?, Some(Value::Bool(_))) { set(data, "localNetworkMode", Value::Bool(false), budget)?; }
    fill(data, d, &["localNetworkTimeoutSec"], budget)?;
    if not_number(field(data, "localNetworkTimeoutSec", budget)?) { set(data, "localNetworkTimeoutSec", Value::Number(600.0), budget)?; }
    fill(data, d, &["gptVisionQuality", "huggingfaceKey", "fishSpeechKey", "presetRegex", "reverseProxyOobaArgs", "top_p"], budget)?;
    if !matches!(field(data, "top_p", budget)?, Some(Value::Number(_))) { set(data, "top_p", Value::Number(1.0), budget)?; }
    fill_with(data, "google", || Ok(empty_object(budget)?), budget)?;
    {
        let google = object_mut(data, "google", "google", budget)?;
        for key in ["accessToken", "projectId"] { fill_with(google, key, || Ok(text("", budget)?), budget)?; }
    }
    fill(data, d, &["genTime", "promptSettings", "keiServerURL", "top_k"], budget)?;
    fill_with(object_mut(data, "promptSettings", "promptSettings", budget)?, "maxThoughtTagDepth", || Ok(Value::Number(-1.0)), budget)?;
    fill(data, d, &[
        "openrouterFallback", "openrouterMiddleOut", "removePunctuationHypa", "memoryLimitThickness", "modules", "enabledModules",
        "additionalParams", "heightMode", "antiClaudeOverload", "maxSupaChunkSize", "ollamaURL", "ollamaModel",
    ], budget)?;
    let cloud = ollama_cloud(data, budget)?;
    fill_with(data, "ollamaModelSource", || Ok(text(if cloud { "cloud" } else { "local" }, budget)?), budget)?;
    fill(data, d, &[
        "ollamaInputMode", "ollamaRequestFormat", "ollamaApiKey", "ollamaModelName", "ollamaCloudModel", "ollamaCloudModelName",
        "ollamaThinkingMode",
    ], budget)?;
    if cloud && !truthy(field(data, "ollamaCloudModel", budget)?) {
        let model = copy_or_undefined(field(data, "ollamaModel", budget)?, budget)?;
        let name = copy_or_undefined(field(data, "ollamaModelName", budget)?, budget)?;
        set(data, "ollamaCloudModel", model, budget)?;
        set(data, "ollamaCloudModelName", name, budget)?;
    }
    fill(data, d, &["autoContinueChat", "autoContinueMinTokens", "repetition_penalty", "min_p", "top_a", "customTokenizer", "instructChatTemplate"], budget)?;
    migrate_provider(data, budget)?;
    if let Some(presets) = field_mut(data, "botPresets", budget)? {
        if presets.truthy() {
            // `for...of` over anything but an array throws once it reaches a preset.
            let Value::Array(presets) = presets else { return Err(unsupported("botPresets")) };
            for preset in presets.as_mut_slice() {
                budget.work(1)?;
                if !matches!(preset, Value::Object(_)) { return Err(unsupported("botPresets")); }
                normalize_template(preset, budget)?;
                fill_with(preset, "localNetworkMode", || Ok(Value::Bool(false)), budget)?;
                fill_with(preset, "localNetworkTimeoutSec", || Ok(Value::Number(600.0)), budget)?;
                if !matches!(field(preset, "localNetworkMode", budget)?, Some(Value::Bool(_))) { set(preset, "localNetworkMode", Value::Bool(false), budget)?; }
                if not_number(field(preset, "localNetworkTimeoutSec", budget)?) { set(preset, "localNetworkTimeoutSec", Value::Number(600.0), budget)?; }
                migrate_provider(preset, budget)?;
            }
        }
    }
    fill(data, d, &[
        "openrouterProvider", "useInstructPrompt", "hanuraiEnable", "hanuraiSplit", "hanuraiTokens", "textAreaSize", "sideBarSize",
        "textAreaTextSize", "combineTranslation", "customPromptTemplateToggle", "globalChatVariables", "templateDefaultVariables",
        "hypaAllocatedTokens", "hypaChunkSize", "dallEQuality",
    ], budget)?;
    {
        let theme = object_mut(data, "customTextTheme", "customTextTheme", budget)?;
        fill_with(theme, "FontColorQuote1", || Ok(text("#8BE9FD", budget)?), budget)?;
        fill_with(theme, "FontColorQuote2", || Ok(text("#FFB86C", budget)?), budget)?;
    }
    fill(data, d, &[
        "font", "customFont", "lineHeight", "stabilityModel", "stabllityStyle", "legacyTranslation", "comfyUiUrl", "comfyConfig",
        "hideApiKey", "unformatQuotes", "ttsAutoSpeech", "translatorInputLanguage", "falModel", "falLoraScale", "customCSS",
        "strictJsonSchema", "statics", "customQuotes", "customQuotesData", "namedMessageRole", "customGUI", "customAPIFormat",
        "systemContentReplacement", "systemRoleReplacement", "vertexAccessToken", "vertexAccessTokenExpires", "vertexClientEmail",
        "vertexPrivateKey", "vertexRegion", "seperateParametersEnabled", "seperateParameters",
    ], budget)?;
    fill_with(object_mut(data, "seperateParameters", "seperateParameters", budget)?, "overrides", || Ok(empty_object(budget)?), budget)?;
    fill(data, d, &[
        "customFlags", "enableCustomFlags", "assetMaxDifference", "showSavingIcon", "banCharacterset", "showPromptComparison",
        "OaiCompAPIKeys", "reasoningEffort", "verbosity",
    ], budget)?;
    hypa_v3_presets(data, constant(constants, &["hypaV3Settings"], budget)?, budget)?;
    fill(data, d, &["hypaV3PresetId"], budget)?;
    translator_presets(data, budget)?;
    fill(data, d, &[
        "showDeprecatedTriggerV2", "returnCSSError", "realmDirectOpen", "blockedRealmCreators", "checkCorruption",
        "toggleConfirmRecommendedPreset", "useExperimentalGoogleTranslator", "thinkingType", "deepseekThinkingType",
        "adaptiveThinkingEffort", "deepseekReasoningEffort",
    ], budget)?;
    if truthy(field(data, "antiClaudeOverload", budget)?) {
        set(data, "antiClaudeOverload", Value::Bool(false), budget)?;
        set(data, "antiServerOverloads", Value::Bool(true), budget)?;
    }
    {
        // `data.hypaCustomSettings?.[key] ?? ""`: only an object has the fields.
        let mut settings = empty_object(budget)?;
        let current = match field(data, "hypaCustomSettings", budget)? { Some(current @ Value::Object(_)) => Some(current), _ => None };
        for key in ["url", "key", "model"] {
            let value = match current { Some(current) => field(current, key, budget)?, None => None };
            set(&mut settings, key, coalesce(value, || text("", budget), budget)?, budget)?;
        }
        set(data, "hypaCustomSettings", settings, budget)?;
    }
    fill(data, d, &["doNotChangeSeperateModels", "modelTools", "enableScrollToActiveChar"], budget)?;
    hotkeys(data, constant(constants, &["hotkeys"], budget)?, budget)?;
    fallback_models(data, budget)?;
    fill(data, d, &[
        "customModels", "authRefreshes", "openAIFlexProcessing", "rememberToolUsage", "simplifiedToolUse", "streamGeminiThoughts",
        "settingsCloseButtonSize", "hideAllImages", "ImagenModel", "ImagenImageSize", "ImagenAspectRatio", "ImagenPersonGeneration",
        "openaiCompatImage", "wavespeedImage", "autoScrollToNewMessage", "alwaysScrollToNewMessage", "newMessageButtonStyle",
    ], budget)?;
    for (key, fallback) in [("chatLoadInitialPages", "initial"), ("chatLoadAdditionalPages", "additional")] {
        let Value::Number(fallback) = constant(constants, &["chatLoadPages", fallback], budget)? else { unreachable!("page counts are numbers") };
        let pages = chat_load_pages(field(data, key, budget)?, *fallback, budget)?;
        set(data, key, Value::Number(pages), budget)?;
    }
    inlay_options(data, constant(constants, &["inlay"], budget)?, budget)?;
    fill(data, d, &[
        "risunestInlayAnimationStillFrame", "streamingDisplayOptimizationMode", "streamingThoughtMode",
        "streamingDeferDisplayProcessing", "risunestChatEditPopup", "risunestScrollNavigation", "risunestSyncStatusUi",
        "echoMessage", "echoDelay",
    ], budget)?;
    if platform == Platform::Web { set(data, "promptInfoInsideChat", Value::Bool(false), budget)?; }
    fill(data, d, &["createFolderOnBranch", "hamburgerButtonBottom", "dynamicModelRegistry", "saveSignatures"], budget)?;
    if nullish(field(data, "enableRisuaiProTools", budget)?) {
        let Some(Value::Array(plugins)) = field(data, "plugins", budget)? else { return Err(unsupported("plugins")) };
        let enabled = !plugins.as_slice().is_empty();
        set(data, "enableRisuaiProTools", Value::Bool(enabled), budget)?;
    }
    fill(data, d, &[
        "keepSessionAlive", "loadouts", "loadoutApplyOptions", "longPressToPopupEditor", "customSidebarItems",
        "moveInsteadOfCopyOnCMPConvert", "skipSavingAssetsOnWebSync",
    ], budget)?;
    clear_streaming(data, budget)
}
