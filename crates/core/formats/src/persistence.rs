//! `prepareDatabaseForPersistence` (`src/ts/storage/databasePreparation.ts:215-222, 240-248`), which every database
//! passes before the persistent store takes it: a canonical JSON clone (`canonicalJson`,
//! `src/ts/storage/saveCoordinatorHelpers.ts:7-28`, [`canonical_clone`]), then `normalizeDatabaseDefaults`
//! ([`crate::database_defaults`]), `checkNewFormat` (`databasePreparation.ts:17-144`, [`check_new_format`]),
//! `assignIds` and `checkCharOrder` ([`crate::risusave`]). The web runtime calls it with no options
//! (`src/ts/storage/persistentDataRuntime.svelte.ts:504`), so `clock` stands for `Date.now()` and `ids` for `uuid`'s
//! `v4()`, which serves every draw of the four steps in TS order. [`prepare_for_persistence`] declines what each step
//! declines: the `normalizeDatabaseDefaults` declines, the `checkNewFormat` ones below, and the `assignIds` and
//! `checkCharOrder` ones listed in [`crate::risusave`].
//!
//! `checkNewFormat` declines, where TS throws a `TypeError`, writes a property JSON cannot show or opens a dialog:
//! `database` (not an object), `protoKey` (an own `__proto__` key anywhere), `characters` (not an array, or a truthy
//! entry that is not a plain object), `modules` (truthy and not an array), `moduleLorebook` (a module whose truthy
//! `lorebook` is not an array: TS reports the corrupted lorebook in a dialog and asks whether to send a report and
//! whether to reset it), `lorebook` (a module lorebook entry that is not a plain object), `bookVersion` (neither a
//! number nor nullish), `content` (not a string where TS rewrites it), `personas` (truthy and not an array of plain
//! objects), `customBackground`, `userIcon` and `image` (not a string where a save without `formatversion` has its
//! asset paths cleaned), `emotionImages` (not an array there, a string entry of two or more units, any object entry,
//! or an array entry whose second item is not a string), `formatversion` (truthy and not a number), `loreBookToken`
//! (not a number, `null` or `undefined` where TS raises it), `trashTime` (truthy and not a number) and `toPrimitive`
//! (an activation percentage or a lorebook content that is an object with an own `toString`).

use crate::database_defaults::{constant, constants, normalize, Platform};
use crate::ids::Ids;
use crate::js::{copy, field, field_mut, has_proto_key, nullish, set, string, truthy, unsupported};
use crate::risusave::{assign_ids_in, check_char_order_in, retain};
use risunest_ir::budget::{Budget, Error, OwnedVec};
use risunest_ir::clock::Clock;
use risunest_ir::stage::StageError;
use risunest_ir::text::{Text, TextBuilder};
use risunest_ir::value::Value;

/// The character fields `checkNewFormat` fills with `??=`, in TS order; the values are the recorded `character`
/// constant.
const CHARACTER_DEFAULTS: [&str; 17] = [
    "type", "chatPage", "chats", "customscript", "firstMessage", "globalLore", "name", "viewScreen", "emotionImages",
    "bias", "characterVersion", "creator", "desc", "utilityBot", "tags", "systemPrompt", "scenario",
];

/// How long a character stays in the trash, `1000 * 60 * 60 * 24 * 3`.
const TRASH_MS: f64 = 259_200_000.0;

/// `prepareDatabaseForPersistence(database)` on `platform`.
pub fn prepare_for_persistence(database: &Value, platform: Platform, clock: &dyn Clock, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(database, Value::Object(_)) { return Err(unsupported("database")); }
    if has_proto_key(database, budget)? { return Err(unsupported("protoKey")); }
    let mut database = canonical_clone(database, budget)?;
    let constants = constants(budget)?;
    normalize(&mut database, platform, ids, &constants, budget)?;
    check(&mut database, clock, ids, &constants, budget)?;
    assign_ids_in(&mut database, ids, budget)?;
    check_char_order_in(&mut database, budget)?;
    Ok(database)
}

/// `JSON.parse(canonicalJson(value))`: every object's keys sorted by UTF-16 code units, with array-index keys first in
/// ascending order as `JSON.parse` places them, `undefined` members dropped, `undefined` and non-finite array items
/// and numbers as `null`, and `-0` as `0`. An `undefined` value, which `canonicalJson` refuses with a `TypeError`, is
/// the `undefined` decline.
pub fn canonical_clone(value: &Value, budget: &Budget) -> Result<Value, StageError> {
    if matches!(value, Value::Undefined) { return Err(unsupported("undefined")); }
    Ok(canonical_at(value, 0, budget)?)
}

fn canonical_at(value: &Value, depth: usize, budget: &Budget) -> Result<Value, Error> {
    budget.work(1)?;
    if depth > budget.limits().max_depth { return Err(Error::Limit("json-depth")); }
    Ok(match value {
        Value::Undefined | Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(value) if !value.is_finite() => Value::Null,
        Value::Number(value) => Value::Number(if *value == 0.0 { 0.0 } else { *value }),
        Value::Text(value) => Value::Text(value.copy(budget)?),
        Value::Array(values) => {
            let mut copy = OwnedVec::new(budget)?;
            for value in values.as_slice() { copy.push(canonical_at(value, depth + 1, budget)?)?; }
            Value::Array(copy)
        }
        Value::Object(entries) => {
            let mut members: Vec<(Option<u32>, &Text, &Value)> = entries
                .as_slice()
                .iter()
                .filter(|(_, value)| !matches!(value, Value::Undefined))
                .map(|(key, value)| (index_key(key), key, value))
                .collect();
            let length = members.len();
            budget.work(length.saturating_mul((usize::BITS - length.leading_zeros()) as usize))?;
            members.sort_by(|(left_index, left, _), (right_index, right, _)| match (left_index, right_index) {
                (Some(left), Some(right)) => left.cmp(right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.units().cmp(right.units()),
            });
            let mut copy = OwnedVec::new(budget)?;
            for (_, key, value) in members { copy.push((key.copy(budget)?, canonical_at(value, depth + 1, budget)?))?; }
            Value::Object(copy)
        }
    })
}

/// The index of a canonical array-index key (`"0"` to `"4294967294"`), which objects order before other keys.
fn index_key(key: &Text) -> Option<u32> {
    let length = key.len_units();
    if length == 0 || length > 10 { return None; }
    let mut units = key.units();
    let first = units.next()?;
    if first == u16::from(b'0') { return (length == 1).then_some(0); }
    if !(u16::from(b'1')..=u16::from(b'9')).contains(&first) { return None; }
    let mut value = u64::from(first - u16::from(b'0'));
    for unit in units {
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&unit) { return None; }
        value = value * 10 + u64::from(unit - u16::from(b'0'));
    }
    u32::try_from(value).ok().filter(|value| *value < u32::MAX)
}

/// `checkNewFormat(database, {now: clock})`. `ids` serves one draw per persona without an id.
pub fn check_new_format(database: &Value, clock: &dyn Clock, ids: &mut dyn Ids, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(database, Value::Object(_)) { return Err(unsupported("database")); }
    if has_proto_key(database, budget)? { return Err(unsupported("protoKey")); }
    let mut database = copy(database, budget)?;
    check(&mut database, clock, ids, &constants(budget)?, budget)?;
    Ok(database)
}

/// The steps of `checkNewFormat` on `data`, in TS order.
pub(crate) fn check(data: &mut Value, clock: &dyn Clock, ids: &mut dyn Ids, constants: &Value, budget: &Budget) -> Result<(), StageError> {
    let defaults = constant(constants, &["character"], budget)?;
    let Some(Value::Array(characters)) = field_mut(data, "characters", budget)? else { return Err(unsupported("characters")) };
    retain(characters, budget, |character| {
        if !character.truthy() { return Ok(false); }
        if !matches!(character, Value::Object(_)) { return Err(unsupported("characters")); }
        for key in CHARACTER_DEFAULTS {
            if nullish(field(character, key, budget)?) { set(character, key, copy(constant(defaults, &[key], budget)?, budget)?, budget)?; }
        }
        Ok(true)
    })?;
    modules(data, budget)?;
    personas(data, ids, budget)?;
    if !truthy(field(data, "formatversion", budget)?) {
        clean_asset_paths(data, budget)?;
        set(data, "formatversion", Value::Number(2.0), budget)?;
    }
    let mut version = match field(data, "formatversion", budget)? {
        Some(Value::Number(version)) => *version,
        _ => return Err(unsupported("formatversion")),
    };
    if version < 3.0 {
        let sd_data = constant(constants, &["sdData"], budget)?;
        for character in characters_mut(data, budget)? {
            budget.work(1)?;
            if nullish(field(character, "sdData", budget)?) { set(character, "sdData", copy(sd_data, budget)?, budget)?; }
        }
        version = 3.0;
        set(data, "formatversion", Value::Number(version), budget)?;
    }
    if version < 4.0 {
        version = 4.0;
        set(data, "formatversion", Value::Number(version), budget)?;
    }
    if version < 5.0 {
        // `undefined < 8000` is false and `null < 8000` is true.
        let raise = match field(data, "loreBookToken", budget)? {
            None => false,
            Some(Value::Null) => true,
            Some(Value::Number(tokens)) => *tokens < 8000.0,
            Some(_) => return Err(unsupported("loreBookToken")),
        };
        if raise { set(data, "loreBookToken", Value::Number(8000.0), budget)?; }
        set(data, "formatversion", Value::Number(5.0), budget)?;
    }
    if nullish(field(data, "characterOrder", budget)?) { set(data, "characterOrder", Value::Array(OwnedVec::new(budget)?), budget)?; }
    let prompts = constant(constants, &["prompts"], budget)?;
    for (old, current) in [("oldMainPrompt", "defaultMainPrompt"), ("oldJailbreak", "defaultJailbreak")] {
        let Value::Text(old) = constant(prompts, &[old], budget)? else { unreachable!("the prompts are strings") };
        if matches!(field(data, "mainPrompt", budget)?, Some(Value::Text(prompt)) if prompt.equals(old)) {
            set(data, "mainPrompt", copy(constant(prompts, &[current], budget)?, budget)?, budget)?;
        }
    }
    let now = clock.now_ms();
    let Some(Value::Array(characters)) = field_mut(data, "characters", budget)? else { unreachable!("checked above") };
    retain(characters, budget, |character| match field(character, "trashTime", budget)? {
        time if !truthy(time) => Ok(true),
        Some(Value::Number(time)) => Ok(*time + TRASH_MS >= now),
        _ => Err(unsupported("trashTime")),
    })?;
    Ok(())
}

/// The characters, every one a plain object once the defaults step has run.
fn characters_mut<'a>(data: &'a mut Value, budget: &Budget) -> Result<&'a mut [Value], StageError> {
    let Some(Value::Array(characters)) = field_mut(data, "characters", budget)? else { unreachable!("checked above") };
    Ok(characters.as_mut_slice())
}

/// `database.modules = (database.modules ?? []).map(...)`, then drops `null` and `undefined` modules. A module with an
/// array `lorebook` has it upgraded by `updateLorebooks`; any other truthy `lorebook` is the dialog path.
fn modules(data: &mut Value, budget: &Budget) -> Result<(), StageError> {
    if nullish(field(data, "modules", budget)?) { set(data, "modules", Value::Array(OwnedVec::new(budget)?), budget)?; }
    let Some(Value::Array(modules)) = field_mut(data, "modules", budget)? else { return Err(unsupported("modules")) };
    retain(modules, budget, |module| {
        if matches!(module, Value::Undefined | Value::Null) { return Ok(false); }
        match field_mut(module, "lorebook", budget)? {
            Some(Value::Array(book)) => update_lorebooks(book, budget)?,
            Some(lorebook) if lorebook.truthy() => return Err(unsupported("moduleLorebook")),
            _ => {}
        }
        Ok(true)
    })
}

/// `updateLorebooks(book)` (`src/ts/characters.ts:763-780`): an entry before version 2 has its activation
/// percentage moved into a `@@probability` line and its old decorators and placeholders rewritten.
fn update_lorebooks(book: &mut OwnedVec<Value>, budget: &Budget) -> Result<(), StageError> {
    for entry in book.as_mut_slice() {
        budget.work(1)?;
        if !matches!(entry, Value::Object(_)) { return Err(unsupported("lorebook")); }
        let version = match field(entry, "bookVersion", budget)? {
            None | Some(Value::Null) => {
                set(entry, "bookVersion", Value::Number(1.0), budget)?;
                1.0
            }
            Some(Value::Number(version)) => *version,
            Some(_) => return Err(unsupported("bookVersion")),
        };
        if version >= 2.0 { continue; }
        if truthy(field(entry, "activationPercent", budget)?) {
            let percent = string(field(entry, "activationPercent", budget)?, budget)?;
            let content = string(field(entry, "content", budget)?, budget)?;
            set(entry, "activationPercent", Value::Null, budget)?;
            let mut units: Vec<u16> = "@@probability ".encode_utf16().collect();
            budget.work(percent.len_units().saturating_add(content.len_units()))?;
            units.extend(percent.units());
            units.push(u16::from(b'\n'));
            units.extend(content.units());
            set(entry, "content", Value::Text(Text::from_units(&units, budget)?), budget)?;
        }
        let Some(Value::Text(content)) = field(entry, "content", budget)? else { return Err(unsupported("content")) };
        let content = replace_all(content, &["@@@end", "@@end"], "@@depth 0", budget)?;
        let content = replace_all(&content, &["<char>", "<bot>"], "{{char}}", budget)?;
        let content = replace_all(&content, &["<user>"], "{{user}}", budget)?;
        set(entry, "content", Value::Text(content), budget)?;
        set(entry, "bookVersion", Value::Number(2.0), budget)?;
    }
    Ok(())
}

/// `value.replace(/a|b/g, replacement)` for literal alternatives tried in order at each position, the first match
/// taken and the scan resumed after it.
fn replace_all(value: &Text, patterns: &[&str], replacement: &str, budget: &Budget) -> Result<Text, Error> {
    let units: Vec<u16> = value.units().collect();
    let patterns: Vec<Vec<u16>> = patterns.iter().map(|pattern| pattern.encode_utf16().collect()).collect();
    budget.work(units.len().saturating_mul(patterns.len()))?;
    let mut output = TextBuilder::new(budget)?;
    let mut at = 0;
    while at < units.len() {
        if let Some(pattern) = patterns.iter().find(|pattern| units[at..].starts_with(pattern)) {
            for unit in replacement.encode_utf16() { output.unit(unit)?; }
            at += pattern.len();
        } else {
            output.unit(units[at])?;
            at += 1;
        }
    }
    output.finish()
}

/// `database.personas = (database.personas ?? []).map(persona => { persona.id ??= v4() })`.
fn personas(data: &mut Value, ids: &mut dyn Ids, budget: &Budget) -> Result<(), StageError> {
    if nullish(field(data, "personas", budget)?) { set(data, "personas", Value::Array(OwnedVec::new(budget)?), budget)?; }
    let Some(Value::Array(personas)) = field_mut(data, "personas", budget)? else { return Err(unsupported("personas")) };
    for persona in personas.as_mut_slice() {
        budget.work(1)?;
        if !matches!(persona, Value::Object(_)) { return Err(unsupported("personas")); }
        if nullish(field(persona, "id", budget)?) { set(persona, "id", Value::Text(ids.next_id(budget)?), budget)?; }
    }
    Ok(())
}

/// The asset path cleanup of a save without `formatversion`: the background, the user icon, each character image and
/// the second item of each emotion image.
fn clean_asset_paths(data: &mut Value, budget: &Budget) -> Result<(), StageError> {
    for key in ["customBackground", "userIcon"] {
        let Some(Value::Text(path)) = field(data, key, budget)? else { return Err(unsupported(key)) };
        let path = clean_asset_path(path, budget)?;
        set(data, key, Value::Text(path), budget)?;
    }
    for character in characters_mut(data, budget)? {
        budget.work(1)?;
        if truthy(field(character, "image", budget)?) {
            let Some(Value::Text(image)) = field(character, "image", budget)? else { return Err(unsupported("image")) };
            let image = clean_asset_path(image, budget)?;
            set(character, "image", Value::Text(image), budget)?;
        }
        // `for (const emotionImage of character.emotionImages ?? [])`; the defaults step left no nullish list.
        let Some(Value::Array(images)) = field_mut(character, "emotionImages", budget)? else { return Err(unsupported("emotionImages")) };
        for image in images.as_mut_slice() {
            budget.work(1)?;
            match image {
                // `emotionImage?.length >= 2`, then `emotionImage[1] = cleanAssetPath(emotionImage[1])`.
                Value::Array(pair) if pair.as_slice().len() >= 2 => {
                    let Value::Text(path) = &pair.as_slice()[1] else { return Err(unsupported("emotionImages")) };
                    let path = clean_asset_path(path, budget)?;
                    pair.as_mut_slice()[1] = Value::Text(path);
                }
                Value::Array(_) => {}
                // A string of two or more units cannot be written into, and an object may have a `length`.
                Value::Text(text) if text.len_units() >= 2 => return Err(unsupported("emotionImages")),
                Value::Object(_) => return Err(unsupported("emotionImages")),
                _ => {}
            }
        }
    }
    Ok(())
}

/// `cleanAssetPath(value)`: a path of three or more units not under `assets` becomes `assets/` and what follows the
/// first `assets/` once backslashes are slashes, up to the next one, or `assets/undefined` when there is none.
fn clean_asset_path(value: &Text, budget: &Budget) -> Result<Text, Error> {
    let units: Vec<u16> = value.units().collect();
    budget.work(units.len())?;
    let prefix: Vec<u16> = "assets".encode_utf16().collect();
    if units.starts_with(&prefix) || units.len() < 3 { return value.copy(budget); }
    let slashed: Vec<u16> = units.iter().map(|unit| if *unit == u16::from(b'\\') { u16::from(b'/') } else { *unit }).collect();
    let separator: Vec<u16> = "assets/".encode_utf16().collect();
    let find = |from: usize| (from..=slashed.len().saturating_sub(separator.len())).find(|at| slashed[*at..].starts_with(&separator));
    let mut output: Vec<u16> = separator.clone();
    match find(0) {
        Some(start) => {
            let start = start + separator.len();
            output.extend_from_slice(&slashed[start..find(start).unwrap_or(slashed.len())]);
        }
        None => output.extend("undefined".encode_utf16()),
    }
    Text::from_units(&output, budget)
}
