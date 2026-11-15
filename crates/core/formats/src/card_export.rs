//! The cards a stored character exports as: `createBaseV3` (`src/ts/characterCards.ts:2060-2207`), `createBaseV2`
//! (`:1669-1764`) and the CharX `{card, module}` projection (`exportChar`, `:919-939`). The native JSON, PNG and charx
//! export jobs write the CCv3 card with its asset URIs still naming stored assets (`ccdefault:` for the portrait);
//! embedding the payloads is the job's part. Each result is in the TS property order, with `undefined` where TS leaves
//! a property `undefined`, so `json::stringify` of it is the compact `JSON.stringify` of what TS returns.
//!
//! Declines, where TS throws a TypeError or follows JS coercions: `character` (not an object), `protoKey` (an own
//! `__proto__` key anywhere; TS's `card.data.extensions[key] = value` would set the prototype), `ccAssets`,
//! `additionalAssets` and `emotionImages` (not arrays of tuples; CCv3 only, `createBaseV2` never reads them),
//! `globalLore` (not an array of objects with a string `key`, a string `secondkey` when selective, and object
//! `extentions`), `loreExt` and `extentions` (not objects) and `toPrimitive` (an object with an own `toString` turned
//! into a string).

use crate::ids::Ids;
use crate::js::{
    array, coalesce, concat_texts, copy, copy_or_undefined, empty_array, empty_object, field, field_mut, has_proto_key,
    nullish, object, push, set, split_trimmed, string, text, truthy, unsupported,
};
use risunest_ir::budget::Budget;
use risunest_ir::clock::Clock;
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;

/// The card `createBaseV3(character)` returns, with `modification_date` from `clock`.
pub fn create_base_v3(character: &Value, clock: &dyn Clock, budget: &Budget) -> Result<Value, StageError> {
    create_base(character, Some(clock), budget)
}

/// The card `createBaseV2(character)` returns: the CCv3 body without the assets, the CCv3-only `risuai` keys from
/// `lowLevelAccess` on, the `data` keys after `extensions`, and the lore entries' `use_regex`.
pub fn create_base_v2(character: &Value, budget: &Budget) -> Result<Value, StageError> {
    create_base(character, None, budget)
}

/// What the CharX export closure in `exportChar` returns: the CCv3 card without its `triggerscript` and
/// `customScripts`, which move into the `module.risum` module with the stored lorebook.
#[derive(Debug)]
pub struct CharxProjection {
    pub card: Value,
    pub module: Value,
}

/// The CharX projection of `character`, drawing the module id from `ids` after the card is built. The same module
/// is what the TS-only CharX export writes (`exportCharacterCard`, `:2016-2031`).
pub fn project_charx(character: &Value, ids: &mut dyn Ids, clock: &dyn Clock, budget: &Budget) -> Result<CharxProjection, StageError> {
    let mut card = create_base_v3(character, clock, budget)?;
    // `${name} Module` and `Module for ${name}`.
    let name = string(field(character, "name", budget)?, budget)?;
    let module_name = concat_texts(&[&name, &Text::from_str(" Module", budget)?], budget)?;
    let description = concat_texts(&[&Text::from_str("Module for ", budget)?, &name], budget)?;
    let id = ids.next_id(budget)?;
    let risuai = field_mut(&mut card, "data", budget)?
        .map(|data| field_mut(data, "extensions", budget)).transpose()?.flatten()
        .map(|extensions| field_mut(extensions, "risuai", budget)).transpose()?.flatten()
        .expect("createBaseV3 builds data.extensions.risuai");
    // The card's values are copies of the character's; `delete` takes them out of the card.
    let or_empty = |value: Option<Value>| -> Result<Value, StageError> {
        match value { Some(value) if !nullish(Some(&value)) => Ok(value), _ => Ok(empty_array(budget)?) }
    };
    let trigger = or_empty(risuai.take(b"triggerscript", budget)?)?;
    let regex = or_empty(risuai.take(b"customScripts", budget)?)?;
    // `createBaseV3` declined a `globalLore` that is not an array, so `?? []` never applies.
    let lorebook = copy(field(character, "globalLore", budget)?.expect("an array"), budget)?;
    let mut module = empty_object(budget)?;
    set(&mut module, "name", Value::Text(module_name), budget)?;
    set(&mut module, "description", Value::Text(description), budget)?;
    set(&mut module, "id", Value::Text(id), budget)?;
    set(&mut module, "trigger", trigger, budget)?;
    set(&mut module, "regex", regex, budget)?;
    set(&mut module, "lorebook", lorebook, budget)?;
    Ok(CharxProjection { card, module })
}

/// The shared body; `v3` is the clock of a CCv3 card and `None` builds the CCv2 card.
fn create_base(character: &Value, v3: Option<&dyn Clock>, budget: &Budget) -> Result<Value, StageError> {
    if object(Some(character)).is_none() { return Err(unsupported("character")); }
    if has_proto_key(character, budget)? { return Err(unsupported("protoKey")); }
    let read = |key: &str| field(character, key, budget);

    // Only `createBaseV3` reads the asset lists, before the lore.
    let assets = match v3 { Some(_) => Some(card_assets(character, budget)?), None => None };

    let Some(Value::Array(lore)) = read("globalLore")? else { return Err(unsupported("globalLore")) };
    let mut entries = Vec::with_capacity(lore.as_slice().len());
    for lore in lore.as_slice() {
        budget.work(1)?;
        entries.push(book_entry(lore, v3.is_some(), budget)?);
    }

    let settings = read("loreSettings")?;
    let setting = |key: &str| -> Result<Value, StageError> {
        Ok(copy_or_undefined(match settings { Some(settings) => field(settings, key, budget)?, None => None }, budget)?)
    };
    // `char.loreExt ??= {}` and its `risu_fullWordMatching` become the book's extensions.
    let mut book_extensions = match read("loreExt")? {
        None | Some(Value::Null) => empty_object(budget)?,
        Some(stored @ Value::Object(_)) => copy(stored, budget)?,
        Some(_) => return Err(unsupported("loreExt")),
    };
    let full_word = match settings { Some(settings) => field(settings, "fullWordMatching", budget)?, None => None };
    set(&mut book_extensions, "risu_fullWordMatching", coalesce(full_word, || Ok(Value::Bool(false)), budget)?, budget)?;
    let mut book = empty_object(budget)?;
    set(&mut book, "scan_depth", setting("scanDepth")?, budget)?;
    set(&mut book, "token_budget", setting("tokenBudget")?, budget)?;
    set(&mut book, "recursive_scanning", setting("recursiveScanning")?, budget)?;
    set(&mut book, "extensions", book_extensions, budget)?;
    set(&mut book, "entries", array(entries, budget)?, budget)?;

    let additional = read("additionalData")?;
    let additional_field = |key: &str| -> Result<Option<&Value>, StageError> {
        Ok(match additional { Some(additional) => field(additional, key, budget)?, None => None })
    };
    let or_text = |value: Option<&Value>, fallback: &str| coalesce(value, || text(fallback, budget), budget);
    let or_array = |value: Option<&Value>| coalesce(value, || empty_array(budget), budget);
    let or_bool = |value: Option<&Value>, fallback: bool| coalesce(value, || Ok(Value::Bool(fallback)), budget);
    let raw = |key: &str| -> Result<Value, StageError> { Ok(copy_or_undefined(read(key)?, budget)?) };

    let mut risuai = empty_object(budget)?;
    {
        let mut put = |key: &str, value: Value| set(&mut risuai, key, value, budget);
        put("bias", raw("bias")?)?;
        put("viewScreen", raw("viewScreen")?)?;
        put("customScripts", raw("customscript")?)?;
        put("utilityBot", raw("utilityBot")?)?;
        put("sdData", raw("sdData")?)?;
        put("backgroundHTML", raw("backgroundHTML")?)?;
        put("license", raw("license")?)?;
        put("triggerscript", raw("triggerscript")?)?;
        put("additionalText", raw("additionalText")?)?;
        put("virtualscript", text("", budget)?)?;
        put("largePortrait", raw("largePortrait")?)?;
        put("lorePlus", raw("lorePlus")?)?;
        put("inlayViewScreen", raw("inlayViewScreen")?)?;
        put("newGenData", raw("newGenData")?)?;
        put("vits", empty_object(budget)?)?;
        if v3.is_some() {
            put("lowLevelAccess", or_bool(read("lowLevelAccess")?, false)?)?;
            put("defaultVariables", or_text(read("defaultVariables")?, "")?)?;
            put("prebuiltAssetCommand", or_text(read("prebuiltAssetCommand")?, "")?)?;
            put("prebuiltAssetExclude", or_array(read("prebuiltAssetExclude")?)?)?;
            put("prebuiltAssetStyle", or_text(read("prebuiltAssetStyle")?, "")?)?;
            put("toggles", or_text(read("customModuleToggle")?, "")?)?;
            put("moduleNamespace", raw("moduleNamespace")?)?;
            put("hideChatIcon", or_bool(read("hideChatIcon")?, false)?)?;
            put("onboarding", raw("onboarding")?)?;
        }
    }
    let mut card_extensions = empty_object(budget)?;
    set(&mut card_extensions, "risuai", risuai, budget)?;
    set(&mut card_extensions, "depth_prompt", raw("depth_prompt")?, budget)?;

    let mut data = empty_object(budget)?;
    {
        let mut put = |key: &str, value: Value| set(&mut data, key, value, budget);
        put("name", raw("name")?)?;
        put("description", or_text(read("desc")?, "")?)?;
        put("personality", or_text(read("personality")?, "")?)?;
        put("scenario", or_text(read("scenario")?, "")?)?;
        put("first_mes", or_text(read("firstMessage")?, "")?)?;
        put("mes_example", or_text(read("exampleMessage")?, "")?)?;
        put("creator_notes", or_text(read("creatorNotes")?, "")?)?;
        put("system_prompt", or_text(read("systemPrompt")?, "")?)?;
        put("post_history_instructions", or_text(read("replaceGlobalNote")?, "")?)?;
        put("alternate_greetings", or_array(read("alternateGreetings")?)?)?;
        put("character_book", book)?;
        put("tags", or_array(read("tags")?)?)?;
        put("creator", or_text(additional_field("creator")?, "")?)?;
        // `` `${char.additionalData?.character_version}` || '' ``: an absent version is the text `undefined`.
        put("character_version", Value::Text(string(additional_field("character_version")?, budget)?))?;
        put("extensions", card_extensions)?;
        if let (Some(clock), Some(assets)) = (v3, assets) {
            put("group_only_greetings", empty_array(budget)?)?;
            put("nickname", or_text(read("nickname")?, "")?)?;
            put("source", or_array(read("source")?)?)?;
            put("creation_date", coalesce(read("creation_date")?, || Ok(Value::Number(0.0)), budget)?)?;
            put("modification_date", Value::Number((clock.now_ms() / 1000.0).floor()))?;
            put("assets", assets)?;
        }
    }

    // Every other `extentions` key joins `data.extensions`, an array-index key ahead of `risuai`.
    let stored = read("extentions")?;
    if truthy(stored) {
        let Some(Value::Object(stored)) = stored else { return Err(unsupported("extentions")) };
        let extensions = field_mut(&mut data, "extensions", budget)?.expect("data.extensions");
        for (key, value) in stored.as_slice() {
            budget.work(1)?;
            if key.equals_ascii(b"risuai") || key.equals_ascii(b"depth_prompt") { continue; }
            extensions.define(key.copy(budget)?, copy(value, budget)?, budget)?;
        }
    }

    let mut card = empty_object(budget)?;
    set(&mut card, "spec", text(if v3.is_some() { "chara_card_v3" } else { "chara_card_v2" }, budget)?, budget)?;
    set(&mut card, "spec_version", text(if v3.is_some() { "3.0" } else { "2.0" }, budget)?, budget)?;
    set(&mut card, "data", data, budget)?;
    Ok(card)
}

/// `data.assets` of a CCv3 card: the stored `ccAssets`, then the additional assets, then the emotions and, with any
/// emotion list, the `ccdefault:` portrait.
fn card_assets(character: &Value, budget: &Budget) -> Result<Value, StageError> {
    let read = |key: &str| field(character, key, budget);
    let mut assets = match read("ccAssets")? {
        None | Some(Value::Null) => empty_array(budget)?,
        Some(stored @ Value::Array(_)) => copy(stored, budget)?,
        Some(_) => return Err(unsupported("ccAssets")),
    };
    let tuples = |name: &'static str| -> Result<Option<&[Value]>, StageError> {
        let value = read(name)?;
        if !truthy(value) { return Ok(None); }
        match value {
            Some(Value::Array(tuples)) if tuples.as_slice().iter().all(|tuple| matches!(tuple, Value::Array(_))) => Ok(Some(tuples.as_slice())),
            _ => Err(unsupported(name)),
        }
    };
    let asset = |kind: &str, uri: Option<&Value>, name: Option<&Value>, ext: Value| -> Result<Value, StageError> {
        let mut asset = empty_object(budget)?;
        set(&mut asset, "type", text(kind, budget)?, budget)?;
        set(&mut asset, "uri", copy_or_undefined(uri, budget)?, budget)?;
        set(&mut asset, "name", copy_or_undefined(name, budget)?, budget)?;
        set(&mut asset, "ext", ext, budget)?;
        Ok(asset)
    };
    if let Some(additional) = tuples("additionalAssets")? {
        for tuple in additional {
            budget.work(1)?;
            let tuple = tuple.array().unwrap_or_default();
            let ext = if truthy(tuple.get(2)) { copy_or_undefined(tuple.get(2), budget)? } else { text("png", budget)? };
            push(&mut assets, asset("x-risu-asset", tuple.get(1), tuple.first(), ext)?)?;
        }
    }
    if let Some(emotions) = tuples("emotionImages")? {
        for tuple in emotions {
            budget.work(1)?;
            let tuple = tuple.array().unwrap_or_default();
            push(&mut assets, asset("emotion", tuple.get(1), tuple.first(), text("png", budget)?)?)?;
        }
        let portrait = text("ccdefault:", budget)?;
        let main = text("main", budget)?;
        push(&mut assets, asset("icon", Some(&portrait), Some(&main), text("png", budget)?)?)?;
    }
    Ok(assets)
}

/// One `character_book.entries` element for a stored lore entry; only the CCv3 entry has `use_regex`.
fn book_entry(lore: &Value, v3: bool, budget: &Budget) -> Result<Value, StageError> {
    let declined = || unsupported("globalLore");
    if object(Some(lore)).is_none() { return Err(declined()); }
    let read = |key: &str| field(lore, key, budget);
    let mut extensions = match read("extentions")? {
        None | Some(Value::Null) => empty_object(budget)?,
        Some(stored @ Value::Object(_)) => copy(stored, budget)?,
        Some(_) => return Err(declined()),
    };
    let case_sensitive = coalesce(field(&extensions, "risu_case_sensitive", budget)?, || Ok(Value::Bool(false)), budget)?;
    set(&mut extensions, "risu_activationPercent", copy_or_undefined(read("activationPercent")?, budget)?, budget)?;
    set(&mut extensions, "risu_loreCache", copy_or_undefined(read("loreCache")?, budget)?, budget)?;
    let Some(Value::Text(key)) = read("key")? else { return Err(declined()) };
    let selective = read("selective")?;
    let secondary = if truthy(selective) {
        let Some(Value::Text(secondary)) = read("secondkey")? else { return Err(declined()) };
        split_trimmed(secondary, budget)?
    } else {
        Value::Undefined
    };
    let comment = read("comment")?;
    let mut entry = empty_object(budget)?;
    set(&mut entry, "keys", split_trimmed(key, budget)?, budget)?;
    set(&mut entry, "secondary_keys", secondary, budget)?;
    set(&mut entry, "content", copy_or_undefined(read("content")?, budget)?, budget)?;
    set(&mut entry, "extensions", extensions, budget)?;
    set(&mut entry, "enabled", Value::Bool(true), budget)?;
    set(&mut entry, "insertion_order", copy_or_undefined(read("insertorder")?, budget)?, budget)?;
    set(&mut entry, "constant", copy_or_undefined(read("alwaysActive")?, budget)?, budget)?;
    set(&mut entry, "selective", copy_or_undefined(selective, budget)?, budget)?;
    set(&mut entry, "name", copy_or_undefined(comment, budget)?, budget)?;
    set(&mut entry, "comment", copy_or_undefined(comment, budget)?, budget)?;
    set(&mut entry, "case_sensitive", case_sensitive, budget)?;
    if v3 { set(&mut entry, "use_regex", coalesce(read("useRegex")?, || Ok(Value::Bool(false)), budget)?, budget)?; }
    set(&mut entry, "mode", coalesce(read("mode")?, || text("normal", budget), budget)?, budget)?;
    set(&mut entry, "folder", copy_or_undefined(read("folder")?, budget)?, budget)?;
    Ok(entry)
}
