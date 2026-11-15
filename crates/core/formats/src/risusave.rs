//! The steps a RisuSave import runs between the decoded database and the persistent store. The web `.risudat` import
//! (`src/ts/storage/risuSaveFileRoute.ts:382-414`) and the account backup restore (`src/ts/drive/accounter.ts:161`) run
//! `prepareExternalDatabaseImport` (`src/ts/storage/upstreamImport.ts:26-52`, [`prepare_external_import`]), then
//! `prepareUpstreamImport` (`src/ts/storage/importedIdentity.ts:6-20` with `prepareImportedIdentityState`,
//! `src/ts/storage/effectiveIdentityState.ts:211-229`, [`prepare_upstream_import`]). `prepareDatabaseForPersistence`
//! ([`crate::persistence`]) then runs `normalizeDatabaseDefaults`, `checkNewFormat`, and `assignIds` and
//! `checkCharOrder` (`src/ts/storage/databasePreparation.ts:146-213`, [`assign_ids`], [`check_char_order`]).
//! [`prepare_web_file_import`] chains the steps as the web `.risudat` import runs them. Decoding and cold storage reads
//! stay with the caller. TS runs `importColdPluginStorage` (`src/ts/process/coldPayloadExpansion.ts:156`) between the
//! two import steps, so a native caller runs it between [`prepare_external_import`] and [`prepare_upstream_import`], or
//! declines the import when `pluginCustomStorage` has an own `_coldpluginIndex` or `_coldplugin` key (the cold plugin
//! references; without them the step does nothing), as [`prepare_web_file_import`] does. Each function returns the
//! database TS leaves behind, in its property order; a key TS sets to `undefined` holds [`Value::Undefined`], which
//! `json::stringify` drops as `JSON.stringify` does.
//!
//! Declines, where TS throws a `TypeError` or writes a property JSON cannot show: `database` (not an object),
//! `protoKey` (an own `__proto__` key anywhere), `characters` (not an array of objects; `prepareExternalDatabaseImport`
//! only refuses `null` entries), `characterOrder` (an entry that is neither a string nor an object with an array
//! `data`, or a value `checkCharOrder` cannot push to), `loadouts`, `characterIds`, `botPresets`, `personas`,
//! `modules` and `customModels` (a list that is not an array, or whose entries are not objects), `botPresetsId` and
//! `selectedPersona` (a selection that is not a number, `null`, a boolean or a canonical index string),
//! `globalChatVariables` (a string or an array, which the spread copies by index), `protectedPresetValues` (not an
//! object where TS writes into it), `embeddedModule` (truthy and not an object), `chats` (not an array of objects),
//! `chaId` and `chatId` (a truthy id that is not a string), and in [`prepare_web_file_import`] `coldPluginStorage`
//! (cold plugin references, whose values TS drops) and the declines of [`crate::persistence`].

use crate::js::{copy, copy_or_undefined, empty_array, empty_object, field, field_mut, has_proto_key, nullish, set, truthy, unsupported};
use risunest_ir::budget::{Budget, Error, OwnedVec};
use risunest_ir::json::parse_json;
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use std::collections::HashSet;

use crate::database_defaults::Platform;
use crate::ids::Ids;
use crate::persistence::prepare_for_persistence;
use risunest_ir::clock::Clock;

/// `{presetMirrorMap, personaMirrorMap, protectedPresetGroups}` (`effectiveIdentityState.ts:8-20, 37`), written by
/// `benchmarks/rust-core-oracles/formats-constants.mjs` from the value the RisuSave oracle records.
const CONSTANTS: &str = include_str!("risusave_constants.json");

/// The message name settings under their upstream and local names (`upstreamImport.ts:3-23`).
pub(crate) const MESSAGE_NAMES: [(&str, &str); 2] = [("groupTemplate", "messageNameTemplate"), ("groupOtherBotRole", "namedMessageRole")];

/// The mirror maps and protected preset groups, as `{presetMirrorMap, personaMirrorMap, protectedPresetGroups}`.
pub fn constants(budget: &Budget) -> Result<Value, StageError> { Ok(parse_json(CONSTANTS, budget)?) }

/// The `[key, value]` entries of one constant map, in `Object.entries` order.
fn pairs(name: &str, budget: &Budget) -> Result<Vec<(String, String)>, StageError> {
    let constants = constants(budget)?;
    let Some(Value::Object(entries)) = constants.get(name.as_bytes(), budget)? else { unreachable!("the constants hold {name}") };
    Ok(entries.as_slice().iter().map(|(key, value)| match value {
        Value::Text(value) => (units_string(key), units_string(value)),
        _ => unreachable!("the constant maps hold strings"),
    }).collect())
}

fn units_string(text: &Text) -> String { String::from_utf16_lossy(&units(text)) }
fn units(text: &Text) -> Vec<u16> { text.units().collect() }

/// `importMessageNameSettings` (`upstreamImport.ts:3-11`). Reading or deleting a key of a primitive or an array does
/// nothing, as in TS.
pub(crate) fn import_message_names(value: &mut Value, budget: &Budget) -> Result<(), Error> {
    for (upstream, local) in MESSAGE_NAMES {
        if let Some(setting) = field(value, upstream, budget)? {
            let setting = copy(setting, budget)?;
            set(value, local, setting, budget)?;
        }
        value.take(upstream.as_bytes(), budget)?;
    }
    Ok(())
}

/// Moves the elements of `values` that `keep` accepts into a new array, in order.
pub(crate) fn retain(values: &mut OwnedVec<Value>, budget: &Budget, mut keep: impl FnMut(&mut Value) -> Result<bool, StageError>) -> Result<(), StageError> {
    let mut kept = OwnedVec::new(budget)?;
    for value in values.as_mut_slice() {
        budget.work(1)?;
        if keep(value)? { kept.push(std::mem::replace(value, Value::Undefined))?; }
    }
    *values = kept;
    Ok(())
}

fn is_text(value: &Value, expected: &Text) -> bool { matches!(value, Value::Text(text) if text.equals(expected)) }

/// `prepareExternalDatabaseImport(database)`: drops group characters and the `§temp` character, removes their ids from
/// `characterOrder` folders and loadouts (a folder left empty goes too), and moves the message name settings of the
/// root, each preset and `protectedPresetValues` to their local names.
pub fn prepare_external_import(database: &Value, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(database, Value::Object(_)) { return Err(unsupported("database")); }
    if has_proto_key(database, budget)? { return Err(unsupported("protoKey")); }
    let mut database = copy(database, budget)?;
    let temp = Text::from_str("§temp", budget)?;
    let mut excluded: HashSet<Vec<u16>> = HashSet::from([units(&temp)]);
    let Some(Value::Array(characters)) = field_mut(&mut database, "characters", budget)? else { return Err(unsupported("characters")) };
    retain(characters, budget, |character| {
        if nullish(Some(&*character)) { return Err(unsupported("characters")); }
        let group = matches!(field(character, "type", budget)?, Some(Value::Text(kind)) if kind.equals_ascii(b"group"));
        let id = field(character, "chaId", budget)?;
        if !group && !id.is_some_and(|id| is_text(id, &temp)) { return Ok(true); }
        if let Some(Value::Text(id)) = id { excluded.insert(units(id)); }
        Ok(false)
    })?;
    let excluded_id = |value: &Value| matches!(value, Value::Text(id) if excluded.contains(&units(id)));
    if let Some(Value::Array(order)) = field_mut(&mut database, "characterOrder", budget)? {
        retain(order, budget, |entry| {
            if matches!(entry, Value::Text(_)) { return Ok(!excluded_id(&*entry)); }
            // Only an object can hold the array `data`; `null`, numbers and arrays throw in TS.
            let Some(Value::Array(ids)) = field_mut(entry, "data", budget)? else { return Err(unsupported("characterOrder")) };
            retain(ids, budget, |id| Ok(!excluded_id(&*id)))?;
            Ok(!ids.as_slice().is_empty())
        })?;
    }
    match field_mut(&mut database, "loadouts", budget)? {
        None | Some(Value::Null | Value::Undefined) => {}
        Some(Value::Array(loadouts)) => {
            for loadout in loadouts.as_mut_slice() {
                budget.work(1)?;
                if !matches!(loadout, Value::Object(_)) { return Err(unsupported("loadouts")); }
                // `loadout.characterIds = loadout.characterIds?.filter(...)`
                let filtered = match field_mut(loadout, "characterIds", budget)? {
                    None | Some(Value::Null | Value::Undefined) => Value::Undefined,
                    Some(Value::Array(ids)) => {
                        let mut ids = std::mem::replace(ids, OwnedVec::new(budget)?);
                        retain(&mut ids, budget, |id| Ok(!excluded_id(&*id)))?;
                        Value::Array(ids)
                    }
                    Some(_) => return Err(unsupported("characterIds")),
                };
                set(loadout, "characterIds", filtered, budget)?;
            }
        }
        Some(_) => return Err(unsupported("loadouts")),
    }
    import_message_names(&mut database, budget)?;
    match field_mut(&mut database, "botPresets", budget)? {
        None | Some(Value::Null | Value::Undefined) => {}
        Some(Value::Array(presets)) => {
            for preset in presets.as_mut_slice() {
                budget.work(1)?;
                if nullish(Some(&*preset)) { return Err(unsupported("botPresets")); }
                import_message_names(preset, budget)?;
            }
        }
        Some(_) => return Err(unsupported("botPresets")),
    }
    if let Some(values) = field_mut(&mut database, "protectedPresetValues", budget)? {
        if values.truthy() { import_message_names(values, budget)?; }
    }
    Ok(database)
}

/// Gives each record of `database[key]` without a string id, or with an empty or repeated one, a fresh id.
fn assign_record_ids(database: &mut Value, key: &'static str, ids: &mut dyn Ids, budget: &Budget) -> Result<(), StageError> {
    let records = match field_mut(database, key, budget)? {
        None | Some(Value::Null | Value::Undefined) => return Ok(()),
        Some(Value::Array(records)) => records,
        Some(_) => return Err(unsupported(key)),
    };
    let mut used: HashSet<Vec<u16>> = HashSet::new();
    for record in records.as_mut_slice() {
        budget.work(1)?;
        if !matches!(record, Value::Object(_)) { return Err(unsupported(key)); }
        let id = match field(record, "id", budget)? {
            Some(Value::Text(id)) if id.len_units() > 0 && !used.contains(&units(id)) => units(id),
            _ => {
                let id = ids.next_id(budget)?;
                let fresh = units(&id);
                set(record, "id", Value::Text(id), budget)?;
                fresh
            }
        };
        used.insert(id);
    }
    Ok(())
}

/// The index `list?.[selection]` reads for an array `list` of objects, `None` when it reads `undefined`.
fn selected_index(database: &Value, list: &str, selection: &'static str, budget: &Budget) -> Result<Option<usize>, StageError> {
    let Some(Value::Array(records)) = field(database, list, budget)? else { return Ok(None) };
    let length = records.as_slice().len();
    let index = match field(database, selection, budget)? {
        None | Some(Value::Null | Value::Bool(_)) => return Ok(None),
        Some(Value::Number(index)) => {
            if index.fract() != 0.0 || *index < 0.0 || *index >= length as f64 { return Ok(None); }
            *index as usize
        }
        Some(Value::Text(index)) => {
            let digits = units_string(index);
            let canonical = digits == "0" || (!digits.is_empty() && !digits.starts_with('0') && digits.len() <= 10 && digits.bytes().all(|byte| byte.is_ascii_digit()));
            match canonical.then(|| digits.parse::<u64>().ok()).flatten() {
                Some(index) if index < u64::from(u32::MAX) => {
                    if index >= length as u64 { return Ok(None); }
                    index as usize
                }
                // Other strings name array or object prototype properties or nothing.
                _ => return Err(unsupported(selection)),
            }
        }
        Some(_) => return Err(unsupported(selection)),
    };
    Ok(Some(index))
}

/// `record[field] = clone(database[key])` for each `[key, field]` of `map` whose key the database has as its own.
fn mirror(database: &mut Value, list: &str, selection: &'static str, map: &str, budget: &Budget) -> Result<(), StageError> {
    let Some(index) = selected_index(database, list, selection, budget)? else { return Ok(()) };
    let mut values = Vec::new();
    for (key, field_name) in pairs(map, budget)? {
        budget.work(1)?;
        if let Some(value) = database.get(key.as_bytes(), budget)? { values.push((field_name, copy(value, budget)?)); }
    }
    let Some(Value::Array(records)) = field_mut(database, list, budget)? else { unreachable!("selected_index found the list") };
    let record = &mut records.as_mut_slice()[index];
    for (field_name, value) in values { set(record, &field_name, value, budget)?; }
    Ok(())
}

/// `prepareUpstreamImport(database)`: `prepareImportedIdentityState` (fresh preset and persona ids, the selected preset
/// and persona mirrors, `explicitGlobalChatVariables`, the protected preset values), then fresh ids for modules,
/// loadouts and custom models and for each persona's embedded module without one. `ids` serves the draws in that order.
pub fn prepare_upstream_import(database: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(database, Value::Object(_)) { return Err(unsupported("database")); }
    if has_proto_key(database, budget)? { return Err(unsupported("protoKey")); }
    let mut database = copy(database, budget)?;
    for key in ["botPresets", "personas"] { assign_record_ids(&mut database, key, ids, budget)?; }
    mirror(&mut database, "botPresets", "botPresetsId", "presetMirrorMap", budget)?;
    mirror(&mut database, "personas", "selectedPersona", "personaMirrorMap", budget)?;
    // `db.explicitGlobalChatVariables = { ...db.globalChatVariables }`
    let variables = match field(&database, "globalChatVariables", budget)? {
        Some(Value::Text(_) | Value::Array(_)) => return Err(unsupported("globalChatVariables")),
        Some(variables @ Value::Object(_)) => copy(variables, budget)?,
        _ => empty_object(budget)?,
    };
    set(&mut database, "explicitGlobalChatVariables", variables, budget)?;
    for (key, flag) in pairs("protectedPresetGroups", budget)? {
        budget.work(1)?;
        if !truthy(field(&database, &flag, budget)?) { continue; }
        if nullish(field(&database, "protectedPresetValues", budget)?) {
            set(&mut database, "protectedPresetValues", empty_object(budget)?, budget)?;
        }
        let value = copy_or_undefined(field(&database, &key, budget)?, budget)?;
        match field_mut(&mut database, "protectedPresetValues", budget)? {
            Some(values @ Value::Object(_)) => set(values, &key, value, budget)?,
            _ => return Err(unsupported("protectedPresetValues")),
        }
    }
    for key in ["modules", "loadouts", "customModels"] { assign_record_ids(&mut database, key, ids, budget)?; }
    if let Some(Value::Array(personas)) = field_mut(&mut database, "personas", budget)? {
        for persona in personas.as_mut_slice() {
            budget.work(1)?;
            let Some(module) = field_mut(persona, "embeddedModule", budget)? else { continue };
            if !module.truthy() { continue; }
            if !matches!(module, Value::Object(_)) { return Err(unsupported("embeddedModule")); }
            if !truthy(field(module, "id", budget)?) { set(module, "id", Value::Text(ids.next_id(budget)?), budget)?; }
        }
    }
    Ok(database)
}

/// A truthy id of `record[key]` that `assigned` already holds, or a falsy one, needs a fresh id.
fn needs_id(record: &Value, key: &str, decline: &'static str, assigned: &HashSet<Vec<u16>>, budget: &Budget) -> Result<bool, StageError> {
    match field(record, key, budget)? {
        id if !truthy(id) => Ok(true),
        Some(Value::Text(id)) => Ok(assigned.contains(&units(id))),
        _ => Err(unsupported(decline)),
    }
}

/// `nextUniqueId`: draws until an id is nonempty and not yet assigned.
fn unique_id(ids: &mut dyn Ids, assigned: &HashSet<Vec<u16>>, budget: &Budget) -> Result<Text, StageError> {
    loop {
        budget.work(1)?;
        let id = ids.next_id(budget)?;
        if id.len_units() > 0 && !assigned.contains(&units(&id)) { return Ok(id); }
    }
}

/// Sets `record[key]` to a fresh id when it needs one, then records the id as assigned.
fn assign(record: &mut Value, key: &str, decline: &'static str, assigned: &mut HashSet<Vec<u16>>, ids: &mut dyn Ids, budget: &Budget) -> Result<(), StageError> {
    if !matches!(record, Value::Object(_)) { return Err(unsupported(if key == "chaId" { "characters" } else { "chats" })); }
    if needs_id(record, key, decline, assigned, budget)? {
        let id = unique_id(ids, assigned, budget)?;
        set(record, key, Value::Text(id), budget)?;
    }
    let Some(Value::Text(id)) = field(record, key, budget)? else { unreachable!("the id is a string now") };
    assigned.insert(units(id));
    Ok(())
}

/// `assignIds(database, createId)`: a fresh id for each character and chat whose id is empty or already used, in
/// order, with `ids` as `createId`.
pub fn assign_ids(database: &Value, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(database, Value::Object(_)) { return Err(unsupported("database")); }
    if has_proto_key(database, budget)? { return Err(unsupported("protoKey")); }
    let mut database = copy(database, budget)?;
    assign_ids_in(&mut database, ids, budget)?;
    Ok(database)
}

/// [`assign_ids`] on `database` itself.
pub(crate) fn assign_ids_in(database: &mut Value, ids: &mut dyn Ids, budget: &Budget) -> Result<(), StageError> {
    let mut assigned = HashSet::new();
    let Some(Value::Array(characters)) = field_mut(database, "characters", budget)? else { return Err(unsupported("characters")) };
    for character in characters.as_mut_slice() {
        budget.work(1)?;
        assign(character, "chaId", "chaId", &mut assigned, ids, budget)?;
        let Some(Value::Array(chats)) = field_mut(character, "chats", budget)? else { return Err(unsupported("chats")) };
        for chat in chats.as_mut_slice() { assign(chat, "id", "chatId", &mut assigned, ids, budget)?; }
    }
    Ok(())
}

/// `checkCharOrder(database)`: appends each live character missing from `characterOrder` (except `§playground`), then
/// drops entries and folder members that name no live character, and folders that were empty before that.
pub fn check_char_order(database: &Value, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(database, Value::Object(_)) { return Err(unsupported("database")); }
    if has_proto_key(database, budget)? { return Err(unsupported("protoKey")); }
    let mut database = copy(database, budget)?;
    check_char_order_in(&mut database, budget)?;
    Ok(database)
}

/// [`check_char_order`] on `database` itself.
pub(crate) fn check_char_order_in(database: &mut Value, budget: &Budget) -> Result<(), StageError> {
    if nullish(field(database, "characterOrder", budget)?) { set(database, "characterOrder", empty_array(budget)?, budget)?; }
    let mut ordered = HashSet::new();
    let Some(Value::Array(order)) = field(database, "characterOrder", budget)? else { return Err(unsupported("characterOrder")) };
    for entry in order.as_slice() {
        budget.work(1)?;
        match entry {
            Value::Text(id) => { ordered.insert(units(id)); }
            Value::Object(_) => {
                let Some(Value::Array(ids)) = field(entry, "data", budget)? else { return Err(unsupported("characterOrder")) };
                for id in ids.as_slice() { if let Value::Text(id) = id { ordered.insert(units(id)); } }
            }
            entry if !entry.truthy() => {}
            _ => return Err(unsupported("characterOrder")),
        }
    }
    let playground = Text::from_str("§playground", budget)?;
    let mut live = HashSet::new();
    let mut missing = Vec::new();
    let Some(Value::Array(characters)) = field(database, "characters", budget)? else { return Err(unsupported("characters")) };
    for character in characters.as_slice() {
        budget.work(1)?;
        if !matches!(character, Value::Object(_)) { return Err(unsupported("characters")); }
        if truthy(field(character, "trashTime", budget)?) { continue; }
        let Some(Value::Text(id)) = field(character, "chaId", budget)? else { return Err(unsupported("chaId")) };
        live.insert(units(id));
        if !id.equals(&playground) && !ordered.contains(&units(id)) { missing.push(id.copy(budget)?); }
    }
    let Some(Value::Array(order)) = field_mut(database, "characterOrder", budget)? else { unreachable!("checked above") };
    for id in missing { order.push(Value::Text(id))?; }
    let named = |value: &Value| matches!(value, Value::Text(id) if live.contains(&units(id)));
    retain(order, budget, |entry| {
        if matches!(entry, Value::Text(_)) { return Ok(named(&*entry)); }
        // A falsy entry goes; every truthy one is an object with an array `data`, checked above.
        let Some(Value::Array(ids)) = field_mut(entry, "data", budget)? else { return Ok(false) };
        if ids.as_slice().is_empty() { return Ok(false); }
        retain(ids, budget, |id| Ok(named(&*id)))?;
        Ok(true)
    })?;
    Ok(())
}

/// The web `.risudat` import once the save is decoded (`src/ts/storage/risuSaveFileRoute.ts:382-414`), through the web
/// runtime's `replacePersistentDatabase` (`src/ts/storage/persistentDataRuntime.ts:2406-2410`):
/// [`prepare_external_import`], `importColdPluginStorage`, [`prepare_upstream_import`], then
/// [`prepare_for_persistence`] on the web, with `clock` and `ids` serving every step. A save file carries no cold
/// items, so TS drops the values the cold plugin references point to and warns; that is the `coldPluginStorage`
/// decline, for a plain-object `pluginCustomStorage` with an own `_coldpluginIndex` or `_coldplugin` key.
pub fn prepare_web_file_import(database: &Value, clock: &dyn Clock, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    let database = prepare_external_import(database, budget)?;
    if let Some(storage @ Value::Object(_)) = database.get(b"pluginCustomStorage", budget)? {
        if storage.get(b"_coldpluginIndex", budget)?.is_some() || storage.get(b"_coldplugin", budget)?.is_some() {
            return Err(unsupported("coldPluginStorage"));
        }
    }
    let database = prepare_upstream_import(&database, ids, budget)?;
    prepare_for_persistence(&database, Platform::Web, clock, ids, budget)
}
