//! `mapPreparedNativeCharacterCard` (`src/ts/characterCards.ts:1058-1099`): a card the native import job decoded, with
//! its staged assets, as the character record TS stores. The job has already read the container and staged every asset
//! payload, so assets arrive as `{token, logicalId}` pairs and the mapping only resolves `__asset:`, `embeded://` and
//! `ccdefault:` references; an inline payload is the error TS raises for it. The result is what TS returns before
//! `canonicalClone`, in its property order, with `undefined` where TS leaves a property `undefined`.
//!
//! A CCv2 or CCv3 card goes through `preflightPreparedNativeCard` (`:1122`) and `importCharacterCardSpecWithPolicy`
//! (`:1170`) with the prepared asset policy; anything else (CCv1, old Tavern, a missing `spec`) through
//! `convertOffSpecCards` (`:842`). Both call `convertCharbook` (`:1560`).
//!
//! Errors TS throws on purpose are `Invalid`: `portraitLogicalId` (empty after trim), `assetToken` (empty token or
//! logical ID), `duplicateAssetToken`, `dataUri` (an inline v3 asset), `missingAsset` (a token the job did not stage),
//! `ccdefaultPortrait` (a `ccdefault:` asset without a portrait) and `assetReference` (a v2 emotion, additional asset or
//! VITS file that is not an `__asset:` reference). Declines, where TS throws a TypeError or follows JS coercions: `card`
//! (not an object), `protoKey` (an own `__proto__` key anywhere), `cardData`, `extensions` and `risuai` (not objects),
//! `assets` and `asset` (v3 assets that are not an array of objects with a string `uri`), `emotions`, `additionalAssets`
//! and `vits` (v2 tuples or files of the wrong shape), `module` (a charx module whose trigger, regex or lorebook is not
//! an array), `characterBook`, `lorebookEntry`, `toPrimitive` (an object with an own `toString` turned into a string)
//! and `lowLevelAccess`, where TS asks the user to confirm before it imports.

use crate::charbook;
use crate::ids::Ids;
use crate::js::{
    after, array, coalesce, copy, copy_or_undefined, empty_array, empty_object, field, has_proto_key, invalid, nullish,
    object, set, starts_with, string, text, truthy, unsupported,
};
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use std::collections::HashMap;

/// A staged asset: the token the card's references name and the stored asset key.
#[derive(Debug, Clone, Copy)]
pub struct PreparedAsset<'a> {
    pub token: &'a Text,
    pub logical_id: &'a Text,
}

/// `PreparedNativeCharacterCardInput` (`src/ts/characterCards.ts:1033`).
#[derive(Debug, Clone, Copy)]
pub struct PreparedCard<'a> {
    /// The decoded card JSON.
    pub card: &'a Value,
    pub assets: &'a [PreparedAsset<'a>],
    /// The stored key of the container's portrait, which `ccdefault:` names.
    pub portrait_logical_id: Option<&'a Text>,
    /// The charx `module.risum` contents (`trigger`, `regex`, `lorebook`), which replace the card's scripts and lore.
    pub module: Option<&'a Value>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Spec { V2, V3 }

struct AssetDict<'a>(HashMap<Vec<u16>, &'a Text>);

impl AssetDict<'_> {
    fn get(&self, token: &Text) -> Option<&Text> { self.0.get(&token.units().collect::<Vec<_>>()).copied() }
}

/// `createPreparedNativeAssetDict` (`:1108`).
fn asset_dict<'a>(assets: &[PreparedAsset<'a>], budget: &Budget) -> Result<AssetDict<'a>, StageError> {
    let mut dict = HashMap::with_capacity(assets.len());
    for asset in assets {
        budget.work(asset.token.len_units().saturating_add(1))?;
        if asset.token.trim(budget)?.len_units() == 0 || asset.logical_id.trim(budget)?.len_units() == 0 {
            return Err(invalid("assetToken"));
        }
        if dict.insert(asset.token.units().collect::<Vec<_>>(), asset.logical_id).is_some() {
            return Err(invalid("duplicateAssetToken"));
        }
    }
    Ok(AssetDict(dict))
}

/// The character TS's `mapPreparedNativeCharacterCard` returns, drawing `chats[0].id`, `chaId`, a v2 VITS id and the
/// onboarding question ids from `ids` in TS order. See the module documentation for its errors and declines.
pub fn map_prepared_card(input: &PreparedCard<'_>, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    let dict = asset_dict(input.assets, budget)?;
    if let Some(portrait) = input.portrait_logical_id {
        if portrait.trim(budget)?.len_units() == 0 { return Err(invalid("portraitLogicalId")); }
    }
    let card = input.card;
    if object(Some(card)).is_none() { return Err(unsupported("card")); }
    if has_proto_key(card, budget)? { return Err(unsupported("protoKey")); }
    let spec = match field(card, "spec", budget)? {
        Some(Value::Text(spec)) if spec.equals_ascii(b"chara_card_v2") => Spec::V2,
        Some(Value::Text(spec)) if spec.equals_ascii(b"chara_card_v3") => Spec::V3,
        _ => return off_spec(card, input.portrait_logical_id, ids, budget),
    };
    let data = object(field(card, "data", budget)?).ok_or_else(|| unsupported("cardData"))?;
    let extensions = object(field(data, "extensions", budget)?).ok_or_else(|| unsupported("extensions"))?;
    let stored_risuai = match field(extensions, "risuai", budget)? {
        None | Some(Value::Null) => None,
        Some(risuai @ Value::Object(_)) => Some(risuai),
        Some(_) => return Err(unsupported("risuai")),
    };
    preflight(spec, data, stored_risuai, &dict, input.portrait_logical_id, budget)?;

    // With a module, `data.extensions.risuai ??= {}` takes the module's trigger and regex scripts.
    let module = match input.module {
        Some(module @ Value::Object(_)) => Some(module),
        Some(module) if module.truthy() => return Err(unsupported("module")),
        _ => None,
    };
    let mut module_risuai = None;
    let mut override_lorebook = None;
    if let Some(module) = module {
        if has_proto_key(module, budget)? { return Err(unsupported("protoKey")); }
        let scripts = |name: &str| -> Result<Value, StageError> {
            match field(module, name, budget)? {
                None | Some(Value::Null) => Ok(empty_array(budget)?),
                Some(value @ Value::Array(_)) => Ok(copy(value, budget)?),
                Some(_) => Err(unsupported("module")),
            }
        };
        let trigger = scripts("trigger")?;
        let regex = scripts("regex")?;
        let mut risuai = match stored_risuai { Some(risuai) => copy(risuai, budget)?, None => empty_object(budget)? };
        set(&mut risuai, "triggerscript", trigger, budget)?;
        set(&mut risuai, "customScripts", regex, budget)?;
        module_risuai = Some(risuai);
        override_lorebook = match field(module, "lorebook", budget)? {
            None => None,
            Some(value @ Value::Array(_)) => Some(copy(value, budget)?),
            Some(_) => return Err(unsupported("module")),
        };
    }
    let risuai = module_risuai.as_ref().or(stored_risuai);
    let read = |key: &str| -> Result<Option<&Value>, StageError> {
        Ok(match risuai { Some(risuai) => field(risuai, key, budget)?, None => None })
    };

    let mut image = input.portrait_logical_id.map(|portrait| portrait.copy(budget)).transpose()?;
    let mut emotions = Vec::new();
    let mut extra_assets = Vec::new();
    let mut cc_assets = Vec::new();
    let mut vits = Value::Null;
    let resolve = |token: &Text| -> Result<Text, StageError> {
        Ok(dict.get(token).ok_or_else(|| invalid("missingAsset"))?.copy(budget)?)
    };
    if spec == Spec::V2 && risuai.is_some() {
        for (name, list) in [("emotions", &mut emotions), ("additionalAssets", &mut extra_assets)] {
            let Some(Value::Array(tuples)) = read(name)? else { continue };
            for tuple in tuples.as_slice() {
                budget.work(1)?;
                let tuple = tuple.array().unwrap_or_default();
                let Some(Value::Text(reference)) = tuple.get(1) else { return Err(unsupported(name)) };
                let asset = Value::Text(resolve(&after(reference, "__asset:".len(), budget)?)?);
                let label = copy_or_undefined(tuple.first(), budget)?;
                if name == "emotions" {
                    list.push(array(vec![label, asset], budget)?);
                } else {
                    let file_name = match tuple.get(2) { Some(value) => copy(value, budget)?, None => text("", budget)? };
                    list.push(array(vec![label, asset, file_name], budget)?);
                }
            }
        }
        if let Some(Value::Object(files)) = read("vits")? {
            let mut resolved = OwnedVec::new(budget)?;
            for (key, reference) in files.as_slice() {
                budget.work(1)?;
                let Value::Text(reference) = reference else { return Err(unsupported("vits")) };
                resolved.push((key.copy(budget)?, Value::Text(resolve(&after(reference, "__asset:".len(), budget)?)?)))?;
            }
            if !files.as_slice().is_empty() {
                let id: Vec<u16> = ids.next_id(budget)?.units().filter(|unit| *unit != u16::from(b'-')).collect();
                let mut value = empty_object(budget)?;
                set(&mut value, "name", text("Imported VITS", budget)?, budget)?;
                set(&mut value, "files", Value::Object(resolved), budget)?;
                set(&mut value, "id", Value::Text(Text::from_units(&id, budget)?), budget)?;
                vits = value;
            }
        }
    }
    if spec == Spec::V3 {
        if let Some(Value::Array(assets)) = field(data, "assets", budget)? {
            for asset in assets.as_slice() {
                budget.work(1)?;
                let name = field(asset, "name", budget)?;
                let file_name = if truthy(name) { copy_or_undefined(name, budget)? } else { text("", budget)? };
                let Some(Value::Text(uri)) = field(asset, "uri", budget)? else { return Err(unsupported("asset")) };
                let resolved = if starts_with(uri, "__asset:") {
                    resolve(&after(uri, "__asset:".len(), budget)?)?
                } else if uri.equals_ascii(b"ccdefault:") {
                    image.as_ref().ok_or_else(|| invalid("ccdefaultPortrait"))?.copy(budget)?
                } else if starts_with(uri, "embeded://") {
                    resolve(&after(uri, "embeded://".len(), budget)?)?
                } else if starts_with(uri, "data:") {
                    return Err(invalid("dataUri"));
                } else {
                    continue;
                };
                let kind = field(asset, "type", budget)?;
                let is = |value: Option<&Value>, expected: &[u8]| matches!(value, Some(Value::Text(text)) if text.equals_ascii(expected));
                let ext = |fallback: &str| coalesce(field(asset, "ext", budget)?, || text(fallback, budget), budget);
                if is(kind, b"emotion") {
                    emotions.push(array(vec![file_name, Value::Text(resolved)], budget)?);
                } else if is(kind, b"x-risu-asset") {
                    extra_assets.push(array(vec![file_name, Value::Text(resolved), ext("unknown")?], budget)?);
                } else if is(kind, b"icon") && is(name, b"main") {
                    image = Some(resolved);
                } else {
                    let mut entry = empty_object(budget)?;
                    set(&mut entry, "type", coalesce(kind, || text("asset", budget), budget)?, budget)?;
                    set(&mut entry, "uri", Value::Text(resolved), budget)?;
                    set(&mut entry, "name", file_name, budget)?;
                    set(&mut entry, "ext", ext("unknown")?, budget)?;
                    cc_assets.push(entry);
                }
            }
        }
    }
    let (mut bias, mut view_screen, mut custom_scripts, mut utility_bot, mut sd_data) =
        (empty_array(budget)?, text("none", budget)?, empty_array(budget)?, Value::Bool(false), default_sd_data(budget)?);
    if risuai.is_some() {
        bias = coalesce(read("bias")?, || Ok(bias), budget)?;
        view_screen = coalesce(read("viewScreen")?, || Ok(view_screen), budget)?;
        custom_scripts = coalesce(read("customScripts")?, || Ok(custom_scripts), budget)?;
        utility_bot = coalesce(read("utilityBot")?, || Ok(utility_bot), budget)?;
        sd_data = coalesce(read("sdData")?, || Ok(sd_data), budget)?;
        if truthy(read("lowLevelAccess")?) { return Err(unsupported("lowLevelAccess")); }
    }

    let (mut lorebook, mut lore_settings, mut lore_extensions) = (override_lorebook, Value::Undefined, Value::Undefined);
    let book = field(data, "character_book", budget)?;
    if truthy(book) {
        let converted = charbook::convert(book.expect("truthy"), budget)?;
        if lorebook.is_none() { lorebook = Some(array(converted.lorebook, budget)?); }
        lore_settings = converted.settings;
        lore_extensions = converted.extensions;
    }
    let lorebook = match lorebook { Some(lorebook) => lorebook, None => empty_array(budget)? };

    let mut stored_extensions = OwnedVec::new(budget)?;
    for (key, value) in extensions.object().unwrap_or_default() {
        budget.work(1)?;
        if key.equals_ascii(b"risuai") || key.equals_ascii(b"depth_prompt") { continue; }
        stored_extensions.push((key.copy(budget)?, copy(value, budget)?))?;
    }

    let data_field = |key: &str| field(data, key, budget);
    let or_text = |value: Option<&Value>, fallback: &str| coalesce(value, || text(fallback, budget), budget);
    let or_array = |value: Option<&Value>| coalesce(value, || empty_array(budget), budget);
    let or_bool = |value: Option<&Value>, fallback: bool| coalesce(value, || Ok(Value::Bool(fallback)), budget);
    let mut character = empty_object(budget)?;
    let mut put = |key: &str, value: Value| set(&mut character, key, value, budget);
    put("name", or_text(data_field("name")?, "")?)?;
    put("firstMessage", or_text(data_field("first_mes")?, "")?)?;
    put("desc", or_text(data_field("description")?, "")?)?;
    put("notes", text("", budget)?)?;
    put("chats", array(vec![first_chat(ids, budget)?], budget)?)?;
    put("chatPage", Value::Number(0.0))?;
    put("image", image.map(Value::Text).unwrap_or(Value::Undefined))?;
    put("emotionImages", array(emotions, budget)?)?;
    put("bias", bias)?;
    put("globalLore", lorebook)?;
    put("viewScreen", view_screen)?;
    put("chaId", Value::Text(ids.next_id(budget)?))?;
    put("sdData", sd_data)?;
    put("utilityBot", utility_bot)?;
    put("customscript", custom_scripts)?;
    put("exampleMessage", or_text(data_field("mes_example")?, "")?)?;
    put("creatorNotes", or_text(data_field("creator_notes")?, "")?)?;
    put("systemPrompt", or_text(data_field("system_prompt")?, "")?)?;
    put("postHistoryInstructions", text("", budget)?)?;
    put("alternateGreetings", or_array(data_field("alternate_greetings")?)?)?;
    put("tags", or_array(data_field("tags")?)?)?;
    put("creator", or_text(data_field("creator")?, "")?)?;
    put("characterVersion", Value::Text(version_text(data_field("character_version")?, budget)?))?;
    put("personality", or_text(data_field("personality")?, "")?)?;
    put("scenario", or_text(data_field("scenario")?, "")?)?;
    put("firstMsgIndex", Value::Number(-1.0))?;
    put("removedQuotes", Value::Bool(false))?;
    put("loreSettings", lore_settings)?;
    put("loreExt", lore_extensions)?;
    let mut additional = empty_object(budget)?;
    set(&mut additional, "tag", or_array(data_field("tags")?)?, budget)?;
    set(&mut additional, "creator", copy_or_undefined(data_field("creator")?, budget)?, budget)?;
    set(&mut additional, "character_version", copy_or_undefined(data_field("character_version")?, budget)?, budget)?;
    put("additionalData", additional)?;
    put("additionalAssets", array(extra_assets, budget)?)?;
    put("replaceGlobalNote", or_text(data_field("post_history_instructions")?, "")?)?;
    put("backgroundHTML", copy_or_undefined(read("backgroundHTML")?, budget)?)?;
    put("license", copy_or_undefined(read("license")?, budget)?)?;
    put("triggerscript", or_array(read("triggerscript")?)?)?;
    put("private", or_bool(read("private")?, false)?)?;
    put("additionalText", or_text(read("additionalText")?, "")?)?;
    put("virtualscript", text("", budget)?)?;
    put("extentions", Value::Object(stored_extensions))?;
    put("largePortrait", or_bool(read("largePortrait")?, risuai.is_none())?)?;
    put("lorePlus", or_bool(read("lorePlus")?, false)?)?;
    put("inlayViewScreen", or_bool(read("inlayViewScreen")?, false)?)?;
    put("newGenData", copy_or_undefined(read("newGenData")?, budget)?)?;
    let tts = if vits.truthy() { "vits" } else { "normal" };
    put("vits", vits)?;
    put("ttsMode", text(tts, budget)?)?;
    put("imported", Value::Bool(true))?;
    put("source", or_array(read("source")?)?)?;
    put("ccAssets", array(cc_assets, budget)?)?;
    put("lowLevelAccess", or_bool(read("lowLevelAccess")?, false)?)?;
    put("defaultVariables", or_text(read("defaultVariables")?, "")?)?;
    put("chatFolders", empty_array(budget)?)?;
    put("prebuiltAssetCommand", or_text(read("prebuiltAssetCommand")?, "")?)?;
    put("prebuiltAssetExclude", or_array(read("prebuiltAssetExclude")?)?)?;
    put("prebuiltAssetStyle", or_text(read("prebuiltAssetStyle")?, "")?)?;
    put("customModuleToggle", coalesce(read("toggles")?, || empty_object(budget), budget)?)?;
    put("moduleNamespace", copy_or_undefined(read("moduleNamespace")?, budget)?)?;
    put("hideChatIcon", or_bool(read("hideChatIcon")?, false)?)?;
    put("onboarding", normalize_onboarding(read("onboarding")?, ids, budget)?)?;
    if spec == Spec::V3 {
        put("nickname", or_text(data_field("nickname")?, "")?)?;
        let source = match data_field("source")? { source if !nullish(source) => copy_or_undefined(source, budget)?, _ => or_array(read("source")?)? };
        put("source", source)?;
        put("creation_date", coalesce(data_field("creation_date")?, || Ok(Value::Number(0.0)), budget)?)?;
        put("modification_date", coalesce(data_field("modification_date")?, || Ok(Value::Number(0.0)), budget)?)?;
    }
    Ok(character)
}

/// `preflightPreparedNativeCard` (`:1122`): every reference the mapping will resolve must name a staged asset.
fn preflight(spec: Spec, data: &Value, risuai: Option<&Value>, dict: &AssetDict<'_>, portrait: Option<&Text>, budget: &Budget) -> Result<(), StageError> {
    let require = |token: &Text| if token.len_units() == 0 || dict.get(token).is_none() { Err(invalid("missingAsset")) } else { Ok(()) };
    if spec == Spec::V3 {
        let assets = match field(data, "assets", budget)? {
            None | Some(Value::Null) => return Ok(()),
            Some(Value::Array(assets)) => assets.as_slice(),
            Some(_) => return Err(unsupported("assets")),
        };
        for asset in assets {
            budget.work(1)?;
            if object(Some(asset)).is_none() { return Err(unsupported("asset")); }
            let Some(Value::Text(uri)) = field(asset, "uri", budget)? else { return Err(unsupported("asset")) };
            if starts_with(uri, "data:") { return Err(invalid("dataUri")); }
            if starts_with(uri, "__asset:") {
                require(&after(uri, "__asset:".len(), budget)?)?;
            } else if starts_with(uri, "embeded://") {
                require(&after(uri, "embeded://".len(), budget)?)?;
            } else if uri.equals_ascii(b"ccdefault:") && portrait.is_none() {
                return Err(invalid("ccdefaultPortrait"));
            }
        }
        return Ok(());
    }
    let v2_reference = |reference: &Text| -> Result<(), StageError> {
        if !starts_with(reference, "__asset:") { return Err(invalid("assetReference")); }
        require(&after(reference, "__asset:".len(), budget)?)
    };
    let read = |key: &str| -> Result<Option<&Value>, StageError> {
        Ok(match risuai { Some(risuai) => field(risuai, key, budget)?, None => None })
    };
    for name in ["emotions", "additionalAssets"] {
        let tuples = match read(name)? {
            None | Some(Value::Null) => continue,
            Some(Value::Array(tuples)) => tuples.as_slice(),
            Some(_) => return Err(unsupported(name)),
        };
        for tuple in tuples {
            budget.work(1)?;
            let Some(Value::Text(reference)) = tuple.array().and_then(|tuple| tuple.get(1)) else { return Err(unsupported(name)) };
            v2_reference(reference)?;
        }
    }
    match read("vits")? {
        None | Some(Value::Null) => {}
        Some(Value::Object(files)) => {
            for (_, reference) in files.as_slice() {
                budget.work(1)?;
                let Value::Text(reference) = reference else { return Err(unsupported("vits")) };
                v2_reference(reference)?;
            }
        }
        Some(_) => return Err(unsupported("vits")),
    }
    Ok(())
}

/// `convertOffSpecCards` (`:842`): a card without a CCv2 or CCv3 `spec`.
fn off_spec(card: &Value, portrait: Option<&Text>, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    let v2 = matches!(field(card, "spec_version", budget)?, Some(Value::Text(version)) if version.equals_ascii(b"2.0"));
    let data = if v2 { object(field(card, "data", budget)?).ok_or_else(|| unsupported("cardData"))? } else { card };
    let (mut lorebook, mut lore_settings, mut lore_extensions) = (empty_array(budget)?, Value::Undefined, Value::Undefined);
    let book = if v2 { field(data, "character_book", budget)? } else { None };
    if truthy(book) {
        let converted = charbook::convert(book.expect("truthy"), budget)?;
        lorebook = array(converted.lorebook, budget)?;
        lore_settings = converted.settings;
        lore_extensions = converted.extensions;
    }
    let data_field = |key: &str| field(data, key, budget);
    let or_text = |value: Option<&Value>, fallback: &str| coalesce(value, || text(fallback, budget), budget);
    let mut character = empty_object(budget)?;
    let mut put = |key: &str, value: Value| set(&mut character, key, value, budget);
    put("name", or_text(data_field("name")?, "unknown name")?)?;
    put("firstMessage", or_text(data_field("first_mes")?, "unknown first message")?)?;
    put("desc", or_text(data_field("description")?, "")?)?;
    put("notes", text("", budget)?)?;
    put("chats", array(vec![first_chat(ids, budget)?], budget)?)?;
    put("chatPage", Value::Number(0.0))?;
    put("image", match portrait { Some(portrait) => Value::Text(portrait.copy(budget)?), None => Value::Undefined })?;
    put("emotionImages", empty_array(budget)?)?;
    put("bias", empty_array(budget)?)?;
    put("globalLore", lorebook)?;
    put("viewScreen", text("none", budget)?)?;
    put("chaId", Value::Text(ids.next_id(budget)?))?;
    put("sdData", default_sd_data(budget)?)?;
    put("utilityBot", Value::Bool(false))?;
    put("customscript", empty_array(budget)?)?;
    put("exampleMessage", copy_or_undefined(data_field("mes_example")?, budget)?)?;
    put("creatorNotes", text("", budget)?)?;
    put("systemPrompt", if v2 { or_text(data_field("system_prompt")?, "")? } else { text("", budget)? })?;
    put("postHistoryInstructions", if v2 { or_text(data_field("post_history_instructions")?, "")? } else { text("", budget)? })?;
    put("alternateGreetings", empty_array(budget)?)?;
    put("tags", empty_array(budget)?)?;
    put("creator", text("", budget)?)?;
    put("characterVersion", text("", budget)?)?;
    put("personality", or_text(data_field("personality")?, "")?)?;
    put("scenario", or_text(data_field("scenario")?, "")?)?;
    put("firstMsgIndex", Value::Number(-1.0))?;
    put("replaceGlobalNote", text("", budget)?)?;
    put("triggerscript", empty_array(budget)?)?;
    put("additionalText", text("", budget)?)?;
    put("loreExt", lore_extensions)?;
    put("loreSettings", lore_settings)?;
    put("chatFolders", empty_array(budget)?)?;
    Ok(character)
}

/// `{ message: [], note: '', name: 'Chat 1', localLore: [], id: v4() }`.
fn first_chat(ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    let mut chat = empty_object(budget)?;
    set(&mut chat, "message", empty_array(budget)?, budget)?;
    set(&mut chat, "note", text("", budget)?, budget)?;
    set(&mut chat, "name", text("Chat 1", budget)?, budget)?;
    set(&mut chat, "localLore", empty_array(budget)?, budget)?;
    set(&mut chat, "id", Value::Text(ids.next_id(budget)?), budget)?;
    Ok(chat)
}

/// `` `${version}` || '' ``: an absent version is the text `undefined`.
fn version_text(version: Option<&Value>, budget: &Budget) -> Result<Text, StageError> { string(version, budget) }

/// `defaultSdDataFunc()` (`src/ts/storage/database.svelte.ts:2076-2088`).
pub(crate) fn default_sd_data(budget: &Budget) -> Result<Value, StageError> {
    const PAIRS: [(&str, &str); 7] = [
        ("always", "solo, 1girl"), ("negative", ""), ("|character's appearance", ""), ("current situation", ""),
        ("$character's pose", ""), ("$character's emotion", ""), ("current location", ""),
    ];
    let mut pairs = Vec::with_capacity(PAIRS.len());
    for (key, value) in PAIRS { pairs.push(array(vec![text(key, budget)?, text(value, budget)?], budget)?); }
    Ok(array(pairs, budget)?)
}

/// `normalizeOnboarding` (`src/ts/onboarding.ts:109-136`), drawing an id for each question without a nonempty string id.
fn normalize_onboarding(raw: Option<&Value>, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    let Some(raw) = raw.filter(|raw| raw.is_object()) else { return Ok(Value::Undefined) };
    let string_or_empty = |value: Option<&Value>| -> Result<Value, StageError> {
        Ok(match value { Some(Value::Text(value)) => Value::Text(value.copy(budget)?), _ => text("", budget)? })
    };
    let is_true = |value: Option<&Value>| matches!(value, Some(Value::Bool(true)));
    let is = |value: Option<&Value>, expected: &[u8]| matches!(value, Some(Value::Text(text)) if text.equals_ascii(expected));
    let mut onboarding = empty_object(budget)?;
    set(&mut onboarding, "enabled", Value::Bool(is_true(field(raw, "enabled", budget)?)), budget)?;
    set(&mut onboarding, "mode", text(if is(field(raw, "mode", budget)?, b"html") { "html" } else { "form" }, budget)?, budget)?;
    set(&mut onboarding, "title", string_or_empty(field(raw, "title", budget)?)?, budget)?;
    set(&mut onboarding, "description", string_or_empty(field(raw, "description", budget)?)?, budget)?;
    set(&mut onboarding, "html", string_or_empty(field(raw, "html", budget)?)?, budget)?;
    set(&mut onboarding, "floatingButton", Value::Bool(is_true(field(raw, "floatingButton", budget)?)), budget)?;
    let mut questions = Vec::new();
    if let Some(Value::Array(list)) = field(raw, "questions", budget)? {
        for question in list.as_slice().iter().filter(|question| question.is_object()) {
            budget.work(1)?;
            let read = |key: &str| field(question, key, budget);
            let mut entry = empty_object(budget)?;
            let id = match read("id")? {
                Some(Value::Text(id)) if id.len_units() != 0 => Value::Text(id.copy(budget)?),
                _ => Value::Text(ids.next_id(budget)?),
            };
            set(&mut entry, "id", id, budget)?;
            set(&mut entry, "title", string_or_empty(read("title")?)?, budget)?;
            set(&mut entry, "description", string_or_empty(read("description")?)?, budget)?;
            let kind = read("type")?;
            let kind = if is(kind, b"multiple") || is(kind, b"text") { copy_or_undefined(kind, budget)? } else { text("single", budget)? };
            set(&mut entry, "type", kind, budget)?;
            set(&mut entry, "varName", string_or_empty(read("varName")?)?, budget)?;
            set(&mut entry, "required", Value::Bool(is_true(read("required")?)), budget)?;
            set(&mut entry, "placeholder", string_or_empty(read("placeholder")?)?, budget)?;
            let mut options = Vec::new();
            if let Some(Value::Array(list)) = read("options")? {
                for option in list.as_slice().iter().filter(|option| option.is_object()) {
                    budget.work(1)?;
                    let mut value = empty_object(budget)?;
                    set(&mut value, "label", string_or_empty(field(option, "label", budget)?)?, budget)?;
                    set(&mut value, "value", string_or_empty(field(option, "value", budget)?)?, budget)?;
                    options.push(value);
                }
            }
            set(&mut entry, "options", array(options, budget)?, budget)?;
            questions.push(entry);
        }
    }
    set(&mut onboarding, "questions", array(questions, budget)?, budget)?;
    Ok(onboarding)
}
