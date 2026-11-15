//! Effective settings, including protected preset values and persona mirrors.
//!
//! The settings a send reads are the root record with every preset and persona mirror resolved as
//! `deriveEffectivePresetMirrors` and `deriveEffectivePersonaMirrors` resolve them when the database loads
//! (`src/ts/storage/effectiveIdentityState.ts`, `applyEffectivePreset` and `setPreset` in
//! `src/ts/storage/database.svelte.ts`). The load runs `normalizeDatabaseDefaults` on the root before a selected
//! preset or persona replaces the mirrors, so a mirror nothing replaces is the root field as that function leaves it.

use crate::persona::{self, Persona};
use crate::preset::{self, Preset, MIRRORS, PROTECTED};
use crate::root::{copy, is_text, nullish, Fields, Root};
use risunest_ir::stage::StageError;
use risunest_ir::{parse_json, Budget, OwnedVec, Text, Value};
use std::borrow::Cow;

/// The resolved mirrors, in `presetMirrorMap` order followed by `personaMirrorMap` order. A mirror TS leaves
/// `undefined` holds `Value::Undefined`.
#[derive(Debug)]
pub struct EffectiveSettings { values: Vec<(&'static str, Value)> }

impl EffectiveSettings {
    /// Resolves the mirrors of `root` with `preset`, the stored preset a load selects ([`preset::selected`]; `None` when
    /// it selects none), and the persona a load selects.
    pub fn derive(root: Root<'_>, preset: Option<Preset<'_>>, budget: &Budget) -> Result<Self, StageError> {
        let mut values = Vec::with_capacity(MIRRORS.len() + persona::MIRRORS.len());
        let root_fields = root.fields();
        let defaults = [preset::mirror_defaults(budget)?, literal(ROOT_DEFAULTS, budget)?];
        let loaded_root = |key: &str| -> Result<Value, StageError> { loaded(key, root_fields.get(key, budget)?, &defaults, budget) };
        match preset {
            Some(preset) => {
                for (key, field) in MIRRORS {
                    let protected = match preset::protection_flag(key) { Some(flag) => root_fields.truthy(flag, budget)?, None => false };
                    // `applyEffectivePreset` resets every unprotected mirror to its default before `setPreset`, and a
                    // protected mirror keeps the loaded root field.
                    let kept;
                    let current = if protected { kept = loaded_root(key)?; Some(&kept) } else { defaults[0].get(key.as_bytes(), budget)? };
                    let given = preset.fields().get(field, budget)?;
                    values.push((key, set_preset(key, given, current, protected, budget)?));
                }
            }
            None => for (key, _) in MIRRORS { values.push((key, loaded_root(key)?)); },
        }
        let stored = root_fields.object("protectedPresetValues", "root.protectedPresetValues", budget)?;
        for (key, flag) in PROTECTED {
            let Some(stored) = stored else { break };
            if !root_fields.truthy(flag, budget)? { continue; }
            if let Some(value) = stored.value().get(key.as_bytes(), budget)? {
                let value = canonical(value, budget)?;
                if let Some(entry) = values.iter_mut().find(|(name, _)| *name == key) { entry.1 = value; }
            }
        }
        // `personas ??= [...]` gives a root without a persona list one persona made from the loaded root mirrors and an
        // empty prompt, and the load selects it.
        let defaulted = nullish(root_fields.get("personas", budget)?) && persona::selects_first(root, budget)?;
        let selected = persona::selected(root, budget)?;
        for (key, field) in persona::MIRRORS {
            let value = match selected {
                Some(persona) => Persona::get(&persona, field, budget)?.map_or(Ok(Value::Undefined), |value| canonical(value, budget))?,
                None if defaulted && key == "personaPrompt" => Value::Text(Text::from_str("", budget)?),
                None => loaded_root(key)?,
            };
            values.push((key, value));
        }
        Ok(Self { values })
    }
    /// A mirror's resolved value; `None` for a field that is not a mirror.
    pub fn get(&self, key: &str) -> Option<&Value> { self.values.iter().find(|(name, _)| *name == key).map(|(_, value)| value) }
    pub fn entries(&self) -> &[(&'static str, Value)] { &self.values }
}

/// The root record as a send reads it: mirror fields resolve through [`EffectiveSettings`], every other field is the
/// stored root field.
#[derive(Debug)]
pub struct Settings<'a> { root: Root<'a>, effective: EffectiveSettings }

impl<'a> Settings<'a> {
    pub fn derive(root: Root<'a>, preset: Option<Preset<'_>>, budget: &Budget) -> Result<Self, StageError> {
        Ok(Self { root, effective: EffectiveSettings::derive(root, preset, budget)? })
    }
    pub fn root(&self) -> Root<'a> { self.root }
    pub fn effective(&self) -> &EffectiveSettings { &self.effective }
    /// The field TS reads as `DBState.db[key]`; `None` when absent or `undefined`.
    pub fn get(&self, key: &str, budget: &Budget) -> Result<Option<&Value>, StageError> {
        budget.work(1)?;
        match self.effective.get(key) {
            Some(Value::Undefined) => Ok(None),
            Some(value) => Ok(Some(value)),
            None => self.root.fields().get(key, budget),
        }
    }
    pub fn truthy(&self, key: &str, budget: &Budget) -> Result<bool, StageError> { Ok(self.get(key, budget)?.is_some_and(Value::truthy)) }
    pub fn text(&self, key: &str, budget: &Budget) -> Result<Option<&Text>, StageError> {
        match self.get(key, budget)? {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Text(text)) => Ok(Some(text)),
            Some(_) => Err(invalid(key)),
        }
    }
    pub fn number(&self, key: &str, budget: &Budget) -> Result<Option<f64>, StageError> {
        match self.get(key, budget)? {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(value)) => Ok(Some(*value)),
            Some(_) => Err(invalid(key)),
        }
    }
    /// An object field, such as `promptSettings`, read through [`Fields`].
    pub fn object(&self, key: &'static str, budget: &Budget) -> Result<Option<Fields<'_>>, StageError> {
        match self.get(key, budget)? {
            None | Some(Value::Null) => Ok(None),
            Some(value @ Value::Object(_)) => Ok(Some(Fields::new(value, key)?)),
            Some(_) => Err(invalid(key)),
        }
    }
}

fn invalid(key: &str) -> StageError { StageError::Invalid(Cow::Owned(format!("settings.{key}"))) }

fn optional_copy(value: Option<&Value>, budget: &Budget) -> Result<Value, StageError> {
    value.map_or(Ok(Value::Undefined), |value| copy(value, budget))
}

fn literal(json: &str, budget: &Budget) -> Result<Value, StageError> { Ok(parse_json(json, budget)?) }

// `normalizeDatabaseDefaults({})` for the root mirrors [`preset::mirror_defaults`] leaves out, because `setPreset` gives
// them fallbacks of its own, and for the persona mirrors.
const ROOT_DEFAULTS: &str = r#"{"NAIadventure":false,"NAIappendName":true,"customProxyRequestModel":"","reverseProxyOobaArgs":{"mode":"instruct"},"top_p":1,"promptSettings":{"assistantPrefill":"","postEndInnerFormat":"","sendChatAsSystem":false,"sendName":false,"utilOverride":false,"customChainOfThought":false,"maxThoughtTagDepth":-1},"repetition_penalty":1,"min_p":0,"top_a":0,"openrouterProvider":{"order":[],"only":[],"ignore":[]},"useInstructPrompt":false,"customPromptTemplateToggle":"","templateDefaultVariables":"","strictJsonSchema":true,"namedMessageRole":"user","seperateParametersEnabled":false,"seperateParameters":{"memory":{},"emotion":{},"translate":{},"otherAx":{},"overrides":{}},"customAPIFormat":0,"systemContentReplacement":"system: {{slot}}","systemRoleReplacement":"user","customFlags":[],"enableCustomFlags":false,"thinkingType":"budget","deepseekThinkingType":"off","adaptiveThinkingEffort":"high","deepseekReasoningEffort":"high","modelTools":[],"fallbackModels":{"memory":[],"emotion":[],"translate":[],"otherAx":[],"model":[]},"verbosity":1,"presetRegex":[],"reasoningEffort":0,"username":"User","userIcon":"","personaPrompt":"","userNote":""}"#;

// `JSON.parse(canonicalJson(value))`, which is how the startup load reads the root (`preparePersistentRootForWorkingSet`):
// object keys sorted by UTF-16 code units at every depth, without `undefined` members.
fn canonical(value: &Value, budget: &Budget) -> Result<Value, StageError> {
    budget.work(1)?;
    match value {
        Value::Array(items) => {
            let mut output = OwnedVec::new(budget)?;
            for item in items.as_slice() { output.push(canonical(item, budget)?)?; }
            Ok(Value::Array(output))
        }
        Value::Object(entries) => {
            let mut members = OwnedVec::new(budget)?;
            for (key, entry) in entries.as_slice() {
                if !matches!(entry, Value::Undefined) { members.push((key, entry))?; }
            }
            budget.work(members.as_slice().len())?;
            members.as_mut_slice().sort_by(|(left, _), (right, _)| left.units().cmp(right.units()));
            let mut output = Value::Object(OwnedVec::new(budget)?);
            for (key, entry) in members.as_slice() { output.define(key.copy(budget)?, canonical(entry, budget)?, budget)?; }
            Ok(output)
        }
        _ => copy(value, budget),
    }
}

// The root mirror `key` as the startup load leaves it: read through canonical JSON, then `normalizeDatabaseDefaults`
// gives a nullish field its default, replaces a few fields by type or rebuilds them, and leaves the rest.
fn loaded(key: &str, given: Option<&Value>, defaults: &[Value; 2], budget: &Budget) -> Result<Value, StageError> {
    let given = given.map(|value| canonical(value, budget)).transpose()?;
    let given = given.as_ref();
    let or_default = |given: Option<&Value>| -> Result<Value, StageError> {
        if nullish(given) {
            for defaults in defaults {
                if let Some(value) = defaults.get(key.as_bytes(), budget)? { return copy(value, budget); }
            }
        }
        optional_copy(given, budget)
    };
    Ok(match key {
        // These also replace a value of another type.
        "localNetworkMode" => match given { Some(value @ Value::Bool(_)) => copy(value, budget)?, _ => Value::Bool(false) },
        "localNetworkTimeoutSec" => match given { Some(value @ Value::Number(_)) => copy(value, budget)?, _ => Value::Number(600.0) },
        "top_p" => match given { Some(value @ Value::Number(_)) => copy(value, budget)?, _ => Value::Number(1.0) },
        "formatingOrder" => with_persona_item(or_default(given)?, budget)?,
        "promptTemplate" => match given { Some(Value::Array(_)) => normalize_prompt_template(given, budget)?, _ => optional_copy(given, budget)? },
        // A provider name from before provider objects becomes the provider order.
        "openrouterProvider" => match given {
            Some(Value::Text(name)) => {
                let mut order = OwnedVec::new(budget)?;
                if name.len_units() > 0 { order.push(Value::Text(name.copy(budget)?))?; }
                let mut provider = Value::Object(OwnedVec::new(budget)?);
                set_property(&mut provider, "order", Value::Array(order), budget)?;
                set_property(&mut provider, "only", Value::Array(OwnedVec::new(budget)?), budget)?;
                set_property(&mut provider, "ignore", Value::Array(OwnedVec::new(budget)?), budget)?;
                provider
            }
            _ => or_default(given)?,
        },
        "promptSettings" => {
            let mut value = or_default(given)?;
            default_property(&mut value, "maxThoughtTagDepth", Value::Number(-1.0), key, budget)?;
            value
        }
        "seperateParameters" => {
            let mut value = or_default(given)?;
            default_property(&mut value, "overrides", Value::Object(OwnedVec::new(budget)?), key, budget)?;
            value
        }
        "NAIsettings" => {
            let mut value = or_default(given)?;
            default_property(&mut value, "starter", Value::Text(Text::from_str("", budget)?), key, budget)?;
            default_property(&mut value, "cfg_scale", Value::Number(1.0), key, budget)?;
            default_property(&mut value, "mirostat_tau", Value::Number(0.0), key, budget)?;
            default_property(&mut value, "mirostat_lr", Value::Number(1.0), key, budget)?;
            value
        }
        // Rebuilt in this key order without empty model names; TS throws on a missing list.
        "fallbackModels" => {
            let value = or_default(given)?;
            let mut rebuilt = Value::Object(OwnedVec::new(budget)?);
            for name in ["model", "memory", "emotion", "translate", "otherAx"] {
                let Some(Value::Array(models)) = value.get(name.as_bytes(), budget)? else { return Err(invalid(key)) };
                let mut kept = OwnedVec::new(budget)?;
                for model in models.as_slice() {
                    if !is_text(Some(model), "") { kept.push(copy(model, budget)?)?; }
                }
                set_property(&mut rebuilt, name, Value::Array(kept), budget)?;
            }
            rebuilt
        }
        _ => or_default(given)?,
    })
}

// `given ?? fallback`.
fn or_literal(given: Option<&Value>, fallback: &str, budget: &Budget) -> Result<Value, StageError> {
    if nullish(given) { literal(fallback, budget) } else { optional_copy(given, budget) }
}

// `given ?? current`.
fn or_current(given: Option<&Value>, current: Option<&Value>, budget: &Budget) -> Result<Value, StageError> {
    optional_copy(if nullish(given) { current } else { given }, budget)
}

// `object[key] = value` on a plain object.
fn set_property(object: &mut Value, key: &str, value: Value, budget: &Budget) -> Result<(), StageError> {
    Ok(object.define(Text::from_str(key, budget)?, value, budget)?)
}

// `object[key] ??= value`: TS throws on a primitive, and on an array it sets a property JSON drops.
fn default_property(object: &mut Value, key: &str, value: Value, field: &str, budget: &Budget) -> Result<(), StageError> {
    match object {
        Value::Object(_) => {}
        Value::Array(_) => return Ok(()),
        _ => return Err(invalid(field)),
    }
    if nullish(object.get(key.as_bytes(), budget)?) { set_property(object, key, value, budget)?; }
    Ok(())
}

fn set_preset(key: &str, given: Option<&Value>, current: Option<&Value>, protected: bool, budget: &Budget) -> Result<Value, StageError> {
    Ok(match key {
        "promptTemplate" => normalize_prompt_template(given, budget)?,
        "NAIadventure" | "NAIappendName" | "localStopStrings" | "repetition_penalty" | "min_p" | "top_a" | "openrouterProvider"
        | "dynamicOutput" => optional_copy(given, budget)?,
        "NAIsettings" => {
            let mut value = or_current(given, current, budget)?;
            default_property(&mut value, "cfg_scale", Value::Number(1.0), key, budget)?;
            default_property(&mut value, "mirostat_tau", Value::Number(0.0), key, budget)?;
            default_property(&mut value, "mirostat_lr", Value::Number(1.0), key, budget)?;
            value
        }
        "promptSettings" => {
            let mut value = or_literal(given, r#"{"assistantPrefill":"","postEndInnerFormat":"","sendChatAsSystem":false,"sendName":false,"utilOverride":false}"#, budget)?;
            default_property(&mut value, "maxThoughtTagDepth", Value::Number(-1.0), key, budget)?;
            value
        }
        "customProxyRequestModel" | "customPromptTemplateToggle" | "templateDefaultVariables" | "moduleIntergration" | "jsonSchema"
        | "extractJson" | "messageNameTemplate" | "systemContentReplacement" => or_literal(given, r#""""#, budget)?,
        "reverseProxyOobaArgs" => or_literal(given, r#"{"mode":"instruct"}"#, budget)?,
        "top_p" | "verbosity" => or_literal(given, "1", budget)?,
        "useInstructPrompt" | "jsonSchemaEnabled" | "seperateParametersEnabled" | "enableCustomFlags" | "outputImageModal" => {
            or_literal(given, "false", budget)?
        }
        "strictJsonSchema" => or_literal(given, "true", budget)?,
        "namedMessageRole" | "systemRoleReplacement" => or_literal(given, r#""user""#, budget)?,
        "customAPIFormat" | "reasoningEffort" => or_literal(given, "0", budget)?,
        "customFlags" | "presetRegex" | "modelTools" => or_literal(given, "[]", budget)?,
        "thinkingTokens" => or_literal(given, "null", budget)?,
        "thinkingType" => or_literal(given, r#""budget""#, budget)?,
        "deepseekThinkingType" => or_literal(given, r#""off""#, budget)?,
        "adaptiveThinkingEffort" | "deepseekReasoningEffort" => or_literal(given, r#""high""#, budget)?,
        "seperateModelsForAxModels" | "fallbackWhenBlankResponse" if !protected => or_literal(given, "false", budget)?,
        "seperateModels" if !protected => or_literal(given, r#"{"memory":"","emotion":"","translate":"","otherAx":""}"#, budget)?,
        "fallbackModels" if !protected => {
            or_literal(given, r#"{"memory":[],"emotion":[],"translate":[],"otherAx":[],"model":[]}"#, budget)?
        }
        "seperateParameters" if !protected => match given {
            Some(value) if value.truthy() => copy(value, budget)?,
            _ => literal(r#"{"memory":{},"emotion":{},"translate":{},"otherAx":{},"overrides":{}}"#, budget)?,
        },
        // A protected field keeps the root value.
        "seperateModelsForAxModels" | "seperateModels" | "fallbackModels" | "fallbackWhenBlankResponse" | "seperateParameters" => {
            optional_copy(current, budget)?
        }
        _ => or_current(given, current, budget)?,
    })
}

fn prompt_role(role: Option<&Value>) -> Option<&'static str> {
    for (name, normalized) in [("user", "user"), ("bot", "bot"), ("system", "system"), ("assistant", "bot"), ("char", "bot")] {
        if is_text(role, name) { return Some(normalized); }
    }
    None
}

fn cache_role(role: Option<&Value>) -> &'static str {
    for (name, normalized) in [("user", "user"), ("assistant", "assistant"), ("system", "system"), ("all", "all"), ("bot", "assistant"), ("char", "assistant")] {
        if is_text(role, name) { return normalized; }
    }
    "all"
}

fn role_text(role: &str, budget: &Budget) -> Result<Value, StageError> { Ok(Value::Text(Text::from_str(role, budget)?)) }

// `normalizePromptTemplate`: a non-array template is `null`; card roles take their normalized names.
fn normalize_prompt_template(given: Option<&Value>, budget: &Budget) -> Result<Value, StageError> {
    let Some(Value::Array(cards)) = given else { return Ok(Value::Null) };
    let mut output = OwnedVec::new(budget)?;
    for card in cards.as_slice() {
        let mut card = copy(card, budget)?;
        if matches!(card, Value::Object(_)) {
            let kind = card.get(b"type", budget)?;
            if ["plain", "jailbreak", "cot"].iter().any(|name| is_text(kind, name)) {
                let role = prompt_role(card.get(b"role", budget)?).unwrap_or("system");
                set_property(&mut card, "role", role_text(role, budget)?, budget)?;
            } else if ["persona", "description", "authornote", "memory"].iter().any(|name| is_text(kind, name)) {
                let role = card.get(b"role2", budget)?;
                if !nullish(role) {
                    let role = prompt_role(role).unwrap_or("system");
                    set_property(&mut card, "role2", role_text(role, budget)?, budget)?;
                }
            } else if is_text(kind, "cache") {
                let role = cache_role(card.get(b"role", budget)?);
                set_property(&mut card, "role", role_text(role, budget)?, budget)?;
            }
        }
        output.push(card)?;
    }
    Ok(Value::Array(output))
}

// The load normalization of the root order: `formatingOrder.splice(formatingOrder.indexOf('main'), 0, 'personaPrompt')`
// when the order has no persona item, with JS `splice` reading a missing `main` (-1) as the last position.
fn with_persona_item(mut order: Value, budget: &Budget) -> Result<Value, StageError> {
    let Value::Array(items) = &mut order else { return Err(invalid("formatingOrder")) };
    budget.work(items.as_slice().len())?;
    if !items.as_slice().iter().any(|item| is_text(Some(item), "personaPrompt")) {
        let length = items.as_slice().len();
        let at = items.as_slice().iter().position(|item| is_text(Some(item), "main")).unwrap_or(length.saturating_sub(1));
        items.push(Value::Text(Text::from_str("personaPrompt", budget)?))?;
        items.as_mut_slice()[at..].rotate_right(1);
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::Limits;
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    use std::sync::{atomic::AtomicBool, Arc};

    const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/prompt/settings.cases.json");
    const EXPECTED: &str = include_str!("../../../../tests/fixtures/rust-core/prompt/settings.expected.json");

    fn budget() -> Budget {
        let limits = Limits { max_owned_bytes: 1 << 34, max_json_bytes: 1 << 26, max_text_units: 1 << 26, max_values: 1 << 26,
            max_depth: 128, max_work: usize::MAX / 4 };
        Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
    }

    // `JSON.stringify` for the replay comparison; `undefined` prints as a marker JSON cannot hold.
    fn stringify(value: &Value, budget: &Budget) -> String {
        risunest_ir::json::stringify(value, budget).unwrap().map_or("<undefined>".into(), |text| key_string(&text))
    }

    fn at<'a>(value: &'a Value, key: &str, budget: &Budget) -> &'a Value {
        value.get(key.as_bytes(), budget).unwrap().unwrap_or_else(|| panic!("missing {key}"))
    }
    fn key_string(key: &Text) -> String { String::from_utf16(&key.units().collect::<Vec<_>>()).unwrap() }

    // Later cases store only their differences from the first case's root.
    fn case_root(base: &Value, database: &Value, budget: &Budget) -> Value {
        if let Some(root) = database.get(b"root", budget).unwrap() { return copy(root, budget).unwrap(); }
        let changes = at(database, "rootChanges", budget);
        let mut root = copy(base, budget).unwrap();
        for (key, value) in at(changes, "set", budget).object().unwrap() {
            root.define(key.copy(budget).unwrap(), copy(value, budget).unwrap(), budget).unwrap();
        }
        let removed = at(changes, "removed", budget).array().unwrap();
        let Value::Object(entries) = &root else { panic!("root") };
        let mut next = OwnedVec::new(budget).unwrap();
        for (name, value) in entries.as_slice() {
            if removed.iter().any(|key| matches!(key, Value::Text(key) if key.equals(name))) { continue; }
            next.push((name.copy(budget).unwrap(), copy(value, budget).unwrap())).unwrap();
        }
        Value::Object(next)
    }

    // The recorded preset is the only stored preset.
    fn selected_preset<'a>(root: &Value, preset: &'a Value, budget: &Budget) -> Option<Preset<'a>> {
        preset::selected(Root::new(root).unwrap(), std::slice::from_ref(preset), budget).unwrap()
    }

    // Replays every recorded case. With `poison`, the root mirrors a found preset or persona replaces are overwritten
    // first, so a derivation that reads them instead fails.
    fn replay(poison: bool) -> (usize, Vec<String>) {
        let budget = budget();
        let expected = parse_json(EXPECTED, &budget).unwrap();
        let digest = Sha256::digest(CASES).iter().fold(String::new(), |mut text, byte| { write!(text, "{byte:02x}").unwrap(); text });
        assert_eq!(at(&expected, "fixtureSha256", &budget).to_text(&budget).unwrap().as_unicode().unwrap(), digest);
        let cases = at(&expected, "cases", &budget).array().unwrap();
        let base_root = at(at(at(&cases[0], "result", &budget), "database", &budget), "root", &budget);
        let mut mismatches = Vec::new();
        for case in cases {
            let id = key_string(match at(case, "id", &budget) { Value::Text(id) => id, _ => panic!("id") });
            let result = at(case, "result", &budget);
            let database = at(result, "database", &budget);
            let mut root = case_root(base_root, database, &budget);
            let preset_value = at(database, "preset", &budget);
            if poison {
                let found = selected_preset(&root, preset_value, &budget).is_some();
                let persona = persona::selected(Root::new(&root).unwrap(), &budget).unwrap().is_some();
                let mut fields: Vec<&str> = Vec::new();
                for (key, _) in MIRRORS {
                    let protected = preset::protection_flag(key)
                        .is_some_and(|flag| Root::new(&root).unwrap().fields().truthy(flag, &budget).unwrap());
                    if found && !protected { fields.push(key); }
                }
                if persona { fields.extend(persona::MIRRORS.iter().map(|(key, _)| *key)); }
                for key in fields {
                    let poisoned = Value::Text(Text::from_str("poisoned root mirror", &budget).unwrap());
                    root.define(Text::from_str(key, &budget).unwrap(), poisoned, &budget).unwrap();
                }
            }
            let preset = selected_preset(&root, preset_value, &budget);
            let actual = EffectiveSettings::derive(Root::new(&root).unwrap(), preset, &budget).unwrap();
            let effective = at(result, "effective", &budget).object().unwrap();
            let keys: Vec<String> = effective.iter().map(|(key, _)| key_string(key)).collect();
            let actual_keys: Vec<String> = actual.entries().iter().map(|(key, _)| key.to_string()).collect();
            if keys != actual_keys { mismatches.push(format!("{id}: mirror keys differ")); }
            for (key, entry) in effective {
                let key = key_string(key);
                let want = match entry.get(b"value", &budget).unwrap() { Some(value) => stringify(value, &budget), None => "<undefined>".into() };
                let got = actual.get(&key).map(|value| stringify(value, &budget)).unwrap_or_else(|| "<missing>".into());
                if want != got { mismatches.push(format!("{id}.{key}: want {want} got {got}")); }
            }
        }
        (cases.len(), mismatches)
    }

    #[test]
    fn effective_settings_match_the_ts_oracle() {
        let (cases, mismatches) = replay(false);
        assert_eq!(cases, 39);
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    }

    #[test]
    fn a_found_preset_and_persona_replace_the_stored_root_mirrors() {
        let (cases, mismatches) = replay(true);
        assert_eq!(cases, 39);
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    }
}
