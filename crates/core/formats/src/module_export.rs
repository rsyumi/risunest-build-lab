//! The files a stored module exports as. `exportModule` (`src/ts/process/modules.ts:90-113`) turns the module into a
//! character with `convertModuleToCharacter` (`src/ts/interchangeability.ts:6-53`) and writes it as a CharX card, whose
//! card and `module.risum` module come from [`crate::card_export::project_charx`]; `exportModuleLegacy`
//! (`modules.ts:115-147`) writes the module itself as RISUM, with the metadata [`risum_metadata`] builds. Writing the
//! containers and embedding the asset payloads is the caller's part.
//!
//! `mcpModule` is the error TS raises for a module with `mcp`. Declines, where TS throws a TypeError or follows JS
//! semantics: `module` (not an object), `protoKey` (an own `__proto__` key anywhere), `lorebook` (a lore entry that is
//! not an object with a string `content`, or a truthy `lorebook` that is not an array), `assets` (a truthy `assets`
//! that is not an array of arrays) and `defaultToggleValues` (a spread of something other than an object or `null`).

use crate::card_import::default_sd_data;
use crate::ids::Ids;
use crate::js::{
    after, array, copy, copy_or_undefined, empty_array, empty_object, field, field_mut, has_proto_key, invalid, nullish,
    object, set, starts_with, stringify_indented, text, truthy, unsupported,
};
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;

/// `createBlankChar()` (`src/ts/characters.ts:782-836`). `root` is the stored root `defaultChatToggleBinding`
/// (`src/ts/toggleBindings.ts:101`) reads; the chat id and then `chaId` come from `ids`.
pub fn create_blank_character(root: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    let mut chat = empty_object(budget)?;
    let defaults = field(root, "defaultToggleValues", budget)?;
    if !truthy(field(root, "disableToggleBinding", budget)?) && defaults.is_some() {
        let saved = match defaults {
            Some(Value::Null) => empty_object(budget)?,
            Some(values @ Value::Object(_)) => copy(values, budget)?,
            _ => return Err(unsupported("defaultToggleValues")),
        };
        set(&mut chat, "savedToggleValues", saved, budget)?;
    }
    set(&mut chat, "message", empty_array(budget)?, budget)?;
    set(&mut chat, "note", text("", budget)?, budget)?;
    set(&mut chat, "name", text("Chat 1", budget)?, budget)?;
    set(&mut chat, "localLore", empty_array(budget)?, budget)?;
    set(&mut chat, "id", Value::Text(ids.next_id(budget)?), budget)?;

    let trigger = |comment: &str, effect: Value| -> Result<Value, StageError> {
        let mut trigger = empty_object(budget)?;
        set(&mut trigger, "comment", text(comment, budget)?, budget)?;
        set(&mut trigger, "type", text("manual", budget)?, budget)?;
        set(&mut trigger, "conditions", empty_array(budget)?, budget)?;
        set(&mut trigger, "effect", effect, budget)?;
        Ok(trigger)
    };
    let mut header = empty_object(budget)?;
    set(&mut header, "type", text("v2Header", budget)?, budget)?;
    set(&mut header, "code", text("", budget)?, budget)?;
    set(&mut header, "indent", Value::Number(0.0), budget)?;
    let triggers = array(vec![trigger("", array(vec![header], budget)?)?, trigger("New Event", empty_array(budget)?)?], budget)?;

    let mut character = empty_object(budget)?;
    let mut put = |key: &str, value: Value| set(&mut character, key, value, budget);
    let empty = || text("", budget);
    put("name", empty()?)?;
    put("firstMessage", empty()?)?;
    put("desc", empty()?)?;
    put("notes", empty()?)?;
    put("chats", array(vec![chat], budget)?)?;
    put("chatFolders", empty_array(budget)?)?;
    put("chatPage", Value::Number(0.0))?;
    put("emotionImages", empty_array(budget)?)?;
    put("bias", empty_array(budget)?)?;
    put("viewScreen", text("none", budget)?)?;
    put("globalLore", empty_array(budget)?)?;
    put("chaId", Value::Text(ids.next_id(budget)?))?;
    put("type", text("character", budget)?)?;
    put("sdData", default_sd_data(budget)?)?;
    put("utilityBot", Value::Bool(false))?;
    put("customscript", empty_array(budget)?)?;
    put("exampleMessage", empty()?)?;
    put("creatorNotes", empty()?)?;
    put("systemPrompt", empty()?)?;
    put("postHistoryInstructions", empty()?)?;
    put("alternateGreetings", empty_array(budget)?)?;
    put("tags", empty_array(budget)?)?;
    put("creator", empty()?)?;
    put("characterVersion", empty()?)?;
    put("personality", empty()?)?;
    put("scenario", empty()?)?;
    put("firstMsgIndex", Value::Number(-1.0))?;
    put("replaceGlobalNote", empty()?)?;
    put("triggerscript", triggers)?;
    put("additionalText", empty()?)?;
    Ok(character)
}

/// `value || fallback` for a copied property.
fn or_else(value: Option<&Value>, fallback: impl FnOnce() -> Result<Value, risunest_ir::budget::Error>, budget: &Budget) -> Result<Value, StageError> {
    Ok(if truthy(value) { copy_or_undefined(value, budget)? } else { fallback()? })
}

/// `value.trim()` after removing its first `prefix` units.
fn trimmed_after(value: &Text, prefix: &str, budget: &Budget) -> Result<Text, StageError> {
    Ok(after(value, prefix.encode_utf16().count(), budget)?.trim(budget)?)
}

/// The units between the first `open` at or after `from` and the first `close` after it, with the index after `close`:
/// one step of `/<open>([\s\S]*?)<close>/`.
fn tagged(units: &[u16], from: usize, open: &str, close: &str) -> Option<(usize, usize, usize)> {
    let open: Vec<u16> = open.encode_utf16().collect();
    let close: Vec<u16> = close.encode_utf16().collect();
    let find = |needle: &[u16], start: usize| (start..=units.len().saturating_sub(needle.len()))
        .find(|at| units.len() >= needle.len() && units[*at..*at + needle.len()] == *needle);
    let start = find(&open, from)? + open.len();
    let end = find(&close, start)?;
    Some((start, end, end + close.len()))
}

/// `convertModuleToCharacter(module)`: a blank character from [`create_blank_character`] carrying the module's lore,
/// scripts and assets, with the `@@indicator` lore entries `convertCharacterToModule` writes turned back into the
/// description, greetings and global note replacement.
pub fn convert_module_to_character(module: &Value, root: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if object(Some(module)).is_none() { return Err(unsupported("module")); }
    if has_proto_key(module, budget)? { return Err(unsupported("protoKey")); }
    let mut character = create_blank_character(root, ids, budget)?;
    let read = |key: &str| field(module, key, budget);
    if truthy(read("mcp")?) { return Err(invalid("mcpModule")); }
    let lore = match read("lorebook")? {
        Some(lore @ Value::Array(_)) => lore.array().unwrap_or_default(),
        value if truthy(value) => return Err(unsupported("lorebook")),
        _ => &[],
    };
    let mut kept = OwnedVec::new(budget)?;
    let (mut desc, mut first_message, mut greetings, mut note) = (None, None, None, None);
    let units_of = |value: &Text| value.units().collect::<Vec<u16>>();
    for entry in lore {
        budget.work(1)?;
        let Some(Value::Text(content)) = object(Some(entry)).map(|entry| field(entry, "content", budget)).transpose()?.flatten() else {
            return Err(unsupported("lorebook"));
        };
        // `/^@@indicator\s+(?:replace_global_note|phi)/` removes exactly the prefix `startsWith` found.
        let indicator = ["@@indicator replace_global_note", "@@indicator phi"].into_iter().find(|prefix| starts_with(content, prefix));
        if let Some(prefix) = indicator {
            note = Some(trimmed_after(content, prefix, budget)?);
        } else if starts_with(content, "@@indicator character_desc") {
            desc = Some(trimmed_after(content, "@@indicator character_desc", budget)?);
        } else if starts_with(content, "@@indicator character_first_message") {
            let messages = trimmed_after(content, "@@indicator character_first_message", budget)?;
            let units = units_of(&messages);
            budget.work(units.len())?;
            if let Some((start, end, _)) = tagged(&units, 0, "<FM>", "</FM>") {
                first_message = Some(Text::from_units(&units[start..end], budget)?.trim(budget)?);
            }
            let mut alternates = Vec::new();
            let mut from = 0;
            while let Some((start, end, next)) = tagged(&units, from, "<FM_alt>", "</FM_alt>") {
                budget.work(1)?;
                alternates.push(Value::Text(Text::from_units(&units[start..end], budget)?.trim(budget)?));
                from = next;
            }
            greetings = Some(array(alternates, budget)?);
        } else {
            kept.push(copy(entry, budget)?)?;
        }
    }
    let raw = |key: &str| -> Result<Value, StageError> { Ok(copy_or_undefined(read(key)?, budget)?) };
    let mut put = |key: &str, value: Value| set(&mut character, key, value, budget);
    put("name", raw("name")?)?;
    put("creatorNotes", raw("description")?)?;
    put("globalLore", Value::Array(kept))?;
    put("customscript", or_else(read("regex")?, || empty_array(budget), budget)?)?;
    put("triggerscript", or_else(read("trigger")?, || empty_array(budget), budget)?)?;
    put("lowLevelAccess", or_else(read("lowLevelAccess")?, || Ok(Value::Bool(false)), budget)?)?;
    put("hideChatIcon", or_else(read("hideIcon")?, || Ok(Value::Bool(false)), budget)?)?;
    put("backgroundHTML", or_else(read("backgroundEmbedding")?, || text("", budget), budget)?)?;
    put("additionalAssets", or_else(read("assets")?, || empty_array(budget), budget)?)?;
    put("moduleNamespace", raw("namespace")?)?;
    put("customModuleToggle", or_else(read("customModuleToggle")?, || text("", budget), budget)?)?;
    put("image", or_else(read("icon")?, || text("", budget), budget)?)?;
    if let Some(note) = note { put("replaceGlobalNote", Value::Text(note))?; }
    if let Some(desc) = desc { put("desc", Value::Text(desc))?; }
    if let Some(first_message) = first_message { put("firstMessage", Value::Text(first_message))?; }
    if let Some(greetings) = greetings { put("alternateGreetings", greetings)?; }
    Ok(character)
}

/// Whether `exportModule` saves `/none.webp` as the portrait of `character`, a [`convert_module_to_character`] result.
pub fn needs_none_image(character: &Value, budget: &Budget) -> Result<bool, StageError> { Ok(!truthy(field(character, "image", budget)?)) }

/// `exportModule`'s fallback portrait: `image` becomes the saved `/none.webp` asset and `extentions.moduleNoneImage`
/// is set, `extentions` created when absent.
pub fn set_none_image(character: &mut Value, image: &Text, budget: &Budget) -> Result<(), StageError> {
    set(character, "image", Value::Text(image.copy(budget)?), budget)?;
    if nullish(field(character, "extentions", budget)?) { set(character, "extentions", empty_object(budget)?, budget)?; }
    let extensions = field_mut(character, "extentions", budget)?.expect("extentions is set");
    if !matches!(extensions, Value::Object(_)) { return Err(unsupported("extentions")); }
    set(extensions, "moduleNoneImage", Value::Bool(true), budget)?;
    Ok(())
}

/// The metadata `exportModuleLegacy` writes into a RISUM file: `{module, type: 'risuModule'}`, the module with
/// `assets ??= []` and each asset tuple's key blanked. The caller writes the asset frames from the module's own keys.
pub fn risum_metadata(module: &Value, budget: &Budget) -> Result<Value, StageError> {
    if object(Some(module)).is_none() { return Err(unsupported("module")); }
    if has_proto_key(module, budget)? { return Err(unsupported("protoKey")); }
    let mut module = copy(module, budget)?;
    let mut tuples = OwnedVec::new(budget)?;
    match field(&module, "assets", budget)? {
        None | Some(Value::Null) => {}
        Some(Value::Array(assets)) => {
            for asset in assets.as_slice() {
                budget.work(1)?;
                let Value::Array(mut tuple) = copy(asset, budget)? else { return Err(unsupported("assets")) };
                // The asset frames read `asset[1]` as a path, which throws on anything but a string.
                if !matches!(tuple.as_slice().get(1), Some(Value::Text(_))) { return Err(unsupported("assets")); }
                tuple.as_mut_slice()[1] = text("", budget)?;
                tuples.push(Value::Array(tuple))?;
            }
        }
        Some(_) => return Err(unsupported("assets")),
    }
    set(&mut module, "assets", Value::Array(tuples), budget)?;
    let mut metadata = empty_object(budget)?;
    set(&mut metadata, "module", module, budget)?;
    set(&mut metadata, "type", text("risuModule", budget)?, budget)?;
    Ok(metadata)
}

/// The metadata frame's text: `JSON.stringify(metadata, null, 2)` of [`risum_metadata`], which `exportModuleLegacy`
/// encodes as UTF-8.
pub fn risum_metadata_text(module: &Value, budget: &Budget) -> Result<Text, StageError> {
    let metadata = risum_metadata(module, budget)?;
    Ok(stringify_indented(&metadata, 2, budget)?.expect("an object is written"))
}
