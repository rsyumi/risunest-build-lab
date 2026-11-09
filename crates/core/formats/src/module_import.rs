//! The modules an imported file becomes: the JSON branches of `importModuleData` (`src/ts/process/modules.ts:437-492`)
//! over the parsed file, the RISUM module the native import job prepared (`activatePreparedNativeModuleContent`,
//! `src/ts/storage/nativeModuleContentActivation.ts:58-89`) or for a prepared card file (:97-121), and
//! `convertCharacterToModule` (`src/ts/interchangeability.ts:55-127`), which turns an imported charx character into a
//! module. Each result is the module TS stores, in its property order, before `canonicalClone`.
//!
//! Errors TS raises on purpose are `Invalid`: `stdioMcp` (a module whose `mcp.url` starts with `stdio:`, the
//! `StdioModuleImportError` TS shows), `moduleIdentity` (a `risuModule` without a truthy `name` or `id`, which TS
//! reports with `alertError(noData)`), `noData` (a JSON file no branch recognizes, `alertNormal(noData)`),
//! `assetMetadata` and `assetTuple` (RISUM asset metadata that does not match the prepared assets). Declines, where TS
//! asks the user or follows JS semantics: `lowLevelAccess` (TS confirms first), `data` and `module` (not an object),
//! `protoKey` (an own `__proto__` key anywhere), `entries` (an external lorebook that is not an object or array),
//! `externalLorebook` (an entry that is not an object, or whose `key`, `keys`, `keywords` or `secondary_keys` is truthy
//! and not an array), `globalLore` and `alternateGreetings` (truthy and not an array) and `toPrimitive` (an object with
//! an own `toString` turned into a string).

use crate::card_import::{map_prepared_card, PreparedCard};
use crate::ids::Ids;
use crate::js::{
    coalesce, concat_texts, copy, copy_or_undefined, empty_array, empty_object, field, has_proto_key, invalid, join,
    nullish, object, push, set, starts_with, string, text, truthy, unsupported,
};
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;

/// `assertModuleMCPImportAllowed` (`src/ts/process/mcp/moduleImport.ts:10`).
fn assert_mcp_allowed(module: &Value, budget: &Budget) -> Result<(), StageError> {
    let url = match field(module, "mcp", budget)? { Some(mcp) => field(mcp, "url", budget)?, None => None };
    match url {
        Some(Value::Text(url)) if starts_with(url, "stdio:") => Err(invalid("stdioMcp")),
        _ => Ok(()),
    }
}

/// `value || fallback`.
fn or_text(value: Option<&Value>, fallback: &str, budget: &Budget) -> Result<Value, StageError> {
    Ok(if truthy(value) { copy_or_undefined(value, budget)? } else { text(fallback, budget)? })
}

/// The module `importModuleData` pushes for a parsed JSON file, drawing its id from `ids`. See the module
/// documentation for its errors and declines.
pub fn map_module_json(parsed: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if object(Some(parsed)).is_none() { return Err(unsupported("data")); }
    if has_proto_key(parsed, budget)? { return Err(unsupported("protoKey")); }
    let read = |key: &str| field(parsed, key, budget);
    let is_type = |name: &[u8]| -> Result<bool, StageError> {
        Ok(matches!(read("type")?, Some(Value::Text(kind)) if kind.equals_ascii(name)))
    };
    if is_type(b"risuModule")? {
        assert_mcp_allowed(parsed, budget)?;
        if !truthy(read("name")?) || !truthy(read("id")?) { return Err(invalid("moduleIdentity")); }
        let mut module = copy(parsed, budget)?;
        set(&mut module, "id", Value::Text(ids.next_id(budget)?), budget)?;
        if truthy(read("lowLevelAccess")?) { return Err(unsupported("lowLevelAccess")); }
        return Ok(module);
    }
    let converted = |content: &str, value: Value, name: &str, description: &str, ids: &mut dyn Ids| -> Result<Value, StageError> {
        let mut module = empty_object(budget)?;
        set(&mut module, "name", or_text(read("name")?, name, budget)?, budget)?;
        set(&mut module, "description", or_text(read("description")?, description, budget)?, budget)?;
        set(&mut module, content, value, budget)?;
        set(&mut module, "id", Value::Text(ids.next_id(budget)?), budget)?;
        Ok(module)
    };
    if is_type(b"risu")? {
        if let Some(lore @ Value::Array(_)) = read("data")? {
            return converted("lorebook", copy(lore, budget)?, "Imported Lorebook", "Converted from risu lorebook", ids);
        }
    }
    let entries = read("entries")?;
    if truthy(entries) {
        let lorebook = convert_external_lorebook(entries.expect("truthy"), budget)?;
        return converted("lorebook", lorebook, "Imported Lorebook", "Converted from external lorebook", ids);
    }
    let data = read("data")?;
    if is_type(b"regex")? && truthy(data) {
        return converted("regex", copy_or_undefined(data, budget)?, "Imported Regex", "Converted from risu regex", ids);
    }
    Err(invalid("noData"))
}

/// `convertExternalLorebook` (`src/ts/process/lorebook.svelte.ts:835-853`): each entry of a `for...in` over `entries`.
fn convert_external_lorebook(entries: &Value, budget: &Budget) -> Result<Value, StageError> {
    let values: Vec<&Value> = match entries {
        Value::Array(values) => values.as_slice().iter().collect(),
        Value::Object(entries) => entries.as_slice().iter().map(|(_, value)| value).collect(),
        _ => return Err(unsupported("entries")),
    };
    let declined = || unsupported("externalLorebook");
    let mut lorebook = empty_array(budget)?;
    for entry in values {
        budget.work(1)?;
        if object(Some(entry)).is_none() { return Err(declined()); }
        let read = |key: &str| field(entry, key, budget);
        // `list ? list.join(', ') : …` throws unless a truthy list is an array.
        let joined = |key: &str| -> Result<Option<Value>, StageError> {
            match read(key)? {
                Some(Value::Array(values)) => Ok(Some(Value::Text(join(values.as_slice(), ", ", budget)?))),
                value if truthy(value) => Err(declined()),
                _ => Ok(None),
            }
        };
        let key = match joined("key")? {
            Some(key) => key,
            None => match joined("keys")? { Some(keys) => keys, None => joined("keywords")?.map_or_else(|| text("", budget), Ok)? },
        };
        let first_text = |keys: &[&str]| -> Result<Value, StageError> {
            for key in keys { if truthy(read(key)?) { return Ok(copy_or_undefined(read(key)?, budget)?); } }
            Ok(text("", budget)?)
        };
        let first_present = |keys: &[&str], fallback: Value| -> Result<Value, StageError> {
            for key in keys { if !nullish(read(key)?) { return Ok(copy_or_undefined(read(key)?, budget)?); } }
            Ok(fallback)
        };
        let budget_priority = match read("contextConfig")? { Some(config) => field(config, "budgetPriority", budget)?, None => None };
        let insertorder = match first_present(&["order", "priority"], Value::Undefined)? {
            Value::Undefined => coalesce(budget_priority, || Ok(Value::Number(0.0)), budget)?,
            found => found,
        };
        let mut lore = empty_object(budget)?;
        set(&mut lore, "key", key, budget)?;
        set(&mut lore, "insertorder", insertorder, budget)?;
        set(&mut lore, "comment", first_text(&["comment", "name", "displayName"])?, budget)?;
        set(&mut lore, "content", first_text(&["content", "entry", "text"])?, budget)?;
        set(&mut lore, "mode", text("normal", budget)?, budget)?;
        set(&mut lore, "alwaysActive", first_present(&["constant", "forceActivation"], Value::Bool(false))?, budget)?;
        set(&mut lore, "secondkey", joined("secondary_keys")?.map_or_else(|| text("", budget), Ok)?, budget)?;
        set(&mut lore, "selective", first_present(&["selective"], Value::Bool(false))?, budget)?;
        push(&mut lorebook, lore)?;
    }
    Ok(lorebook)
}

/// The module `activatePreparedNativeModuleContent` appends for prepared RISUM content: `metadata` is the file's
/// `module` object with asset keys blanked, `logical_ids` the stored key of each asset frame in order. Draws the module
/// id from `ids`. The native job already rejects asset metadata that is not an array of tuples of at least three
/// (`src-tauri/src/native_file_jobs/content.rs:552-568`), so `assetMetadata` and `assetTuple` only answer other callers.
pub fn map_prepared_risum(metadata: &Value, logical_ids: &[&Text], ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if object(Some(metadata)).is_none() { return Err(unsupported("module")); }
    if has_proto_key(metadata, budget)? { return Err(unsupported("protoKey")); }
    let mut module = copy(metadata, budget)?;
    assert_mcp_allowed(&module, budget)?;
    if truthy(field(&module, "lowLevelAccess", budget)?) { return Err(unsupported("lowLevelAccess")); }
    set(&mut module, "id", Value::Text(ids.next_id(budget)?), budget)?;
    let Some(assets) = module.get(b"assets", budget)? else { return Ok(module) };
    let Value::Array(tuples) = assets else { return Err(invalid("assetMetadata")) };
    if tuples.as_slice().len() != logical_ids.len() { return Err(invalid("assetMetadata")); }
    let mut retained = OwnedVec::new(budget)?;
    for (tuple, logical_id) in tuples.as_slice().iter().zip(logical_ids) {
        budget.work(1)?;
        let Value::Array(slots) = tuple else { return Err(invalid("assetTuple")) };
        if slots.as_slice().len() < 3 { return Err(invalid("assetTuple")); }
        let Value::Array(mut copied) = copy(tuple, budget)? else { unreachable!("an array copies as an array") };
        copied.as_mut_slice()[1] = Value::Text(logical_id.copy(budget)?);
        retained.push(Value::Array(copied))?;
    }
    set(&mut module, "assets", Value::Array(retained), budget)?;
    Ok(module)
}

/// The module the native import appends for a prepared character card file, and the character it was converted from,
/// which the caller still needs for the asset owner head.
pub struct CharxModule {
    pub character: Value,
    pub module: Value,
}

/// `activatePreparedNativeModuleContent` for prepared card content (`nativeModuleContentActivation.ts:97-121`): the card
/// mapped as a character, then [`module_from_mapped_card`]. `ids` serves the card mapping's draws, the conversion's and
/// the replacement, in that order.
pub fn map_prepared_charx_module(input: &PreparedCard<'_>, ids: &mut dyn Ids, budget: &Budget) -> Result<CharxModule, StageError> {
    let character = map_prepared_card(input, ids, budget)?;
    let module = module_from_mapped_card(&character, ids, budget)?;
    Ok(CharxModule { character, module })
}

/// The module the native card import appends for `character`, the card [`map_prepared_card`] mapped: converted with
/// `convertCharacterToModule`, then given a fresh id. `ids` serves the conversion's draw and the replacement.
pub fn module_from_mapped_card(character: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    let mut module = convert_character_to_module(character, ids, budget)?;
    set(&mut module, "id", Value::Text(ids.next_id(budget)?), budget)?;
    Ok(module)
}

/// One lore entry `convertCharacterToModule` appends: `{key: '', secondkey: '', insertorder: 0, comment, content,
/// mode: 'constant', alwaysActive, selective: false}`.
fn indicator_lore(comment: &str, content: Text, always_active: bool, budget: &Budget) -> Result<Value, StageError> {
    let mut lore = empty_object(budget)?;
    set(&mut lore, "key", text("", budget)?, budget)?;
    set(&mut lore, "secondkey", text("", budget)?, budget)?;
    set(&mut lore, "insertorder", Value::Number(0.0), budget)?;
    set(&mut lore, "comment", text(comment, budget)?, budget)?;
    set(&mut lore, "content", Value::Text(content), budget)?;
    set(&mut lore, "mode", text("constant", budget)?, budget)?;
    set(&mut lore, "alwaysActive", Value::Bool(always_active), budget)?;
    set(&mut lore, "selective", Value::Bool(false), budget)?;
    Ok(lore)
}

/// `convertCharacterToModule(character)`, drawing the module id from `ids`. The native charx module import then
/// replaces that id with another draw ([`map_prepared_charx_module`]); the TS-only `.charx` import keeps it.
pub fn convert_character_to_module(character: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if object(Some(character)).is_none() { return Err(unsupported("character")); }
    if has_proto_key(character, budget)? { return Err(unsupported("protoKey")); }
    let read = |key: &str| field(character, key, budget);
    let raw = |key: &str| -> Result<Value, StageError> { Ok(copy_or_undefined(read(key)?, budget)?) };
    // `mod.lorebook = safeStructuredClone(mod.lorebook || [])` takes pushes below, which throw on anything else.
    let mut lorebook = match read("globalLore")? {
        Some(lore @ Value::Array(_)) => copy(lore, budget)?,
        value if truthy(value) => return Err(unsupported("globalLore")),
        _ => empty_array(budget)?,
    };
    let greetings = match read("alternateGreetings")? {
        Some(Value::Array(values)) => values.as_slice(),
        value if truthy(value) => return Err(unsupported("alternateGreetings")),
        _ => &[],
    };
    let line = |value: &str| Text::from_str(value, budget);
    if truthy(read("desc")?) {
        let content = concat_texts(&[&line("@@indicator character_desc\n\n")?, &string(read("desc")?, budget)?], budget)?;
        push(&mut lorebook, indicator_lore("From Character Description", content, true, budget)?)?;
    }
    if truthy(read("firstMessage")?) || !greetings.is_empty() {
        let mut messages = concat_texts(&[&line("<FM>\n")?, &string(read("firstMessage")?, budget)?, &line("\n</FM>")?], budget)?;
        for greeting in greetings {
            budget.work(1)?;
            messages = concat_texts(&[&messages, &line("\n<FM_alt>\n")?, &string(Some(greeting), budget)?, &line("\n</FM_alt>")?], budget)?;
        }
        let content = concat_texts(&[&line("@@indicator character_first_message\n\n")?, &messages], budget)?;
        push(&mut lorebook, indicator_lore("From First Messages", content, false, budget)?)?;
    }
    if truthy(read("replaceGlobalNote")?) {
        let note = string(read("replaceGlobalNote")?, budget)?;
        let content = concat_texts(&[&line("@@indicator replace_global_note\n\n")?, &note], budget)?;
        push(&mut lorebook, indicator_lore("From Global Note Replacement", content, true, budget)?)?;
    }
    let mut module = empty_object(budget)?;
    set(&mut module, "name", raw("name")?, budget)?;
    set(&mut module, "description", raw("creatorNotes")?, budget)?;
    set(&mut module, "lorebook", lorebook, budget)?;
    set(&mut module, "regex", raw("customscript")?, budget)?;
    set(&mut module, "trigger", raw("triggerscript")?, budget)?;
    set(&mut module, "lowLevelAccess", raw("lowLevelAccess")?, budget)?;
    set(&mut module, "hideIcon", raw("hideChatIcon")?, budget)?;
    set(&mut module, "backgroundEmbedding", raw("backgroundHTML")?, budget)?;
    set(&mut module, "assets", raw("additionalAssets")?, budget)?;
    set(&mut module, "namespace", raw("moduleNamespace")?, budget)?;
    set(&mut module, "customModuleToggle", raw("customModuleToggle")?, budget)?;
    set(&mut module, "id", Value::Text(ids.next_id(budget)?), budget)?;
    set(&mut module, "icon", raw("image")?, budget)?;
    Ok(module)
}
