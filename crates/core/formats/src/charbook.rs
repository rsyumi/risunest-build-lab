//! `convertCharbook` (`src/ts/characterCards.ts:1560-1665`): a card's `character_book` as stored lore entries, lore
//! settings and the book's `extensions`, with SillyTavern entry extensions rewritten into `@@` decorators.

use crate::js::{
    coalesce, concat, copy, copy_or_undefined, empty_object, field, join, nullish, object, remove, set, starts_with, string,
    text, truthy, unsupported,
};
use risunest_ir::budget::Budget;
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;

pub(crate) struct Converted {
    /// The `globalLore` entries in book order.
    pub(crate) lorebook: Vec<Value>,
    /// `loreSettings`, `undefined` unless the book sets all of scan depth, token budget and recursive scanning.
    pub(crate) settings: Value,
    /// `loreExt`: the book's `extensions` as it is.
    pub(crate) extensions: Value,
}

/// Declines `characterBook` for a book that is not an object or whose `entries` is not an array, and `lorebookEntry`
/// for an entry TS would throw on or read with JS coercions (non-object entry, `keys` not an array, `secondary_keys`
/// neither nullish nor an array, `extensions` neither nullish nor an object, or a `use_regex` entry whose first key is
/// not a string).
pub(crate) fn convert(book: &Value, budget: &Budget) -> Result<Converted, StageError> {
    if object(Some(book)).is_none() { return Err(unsupported("characterBook")); }
    let entries = match field(book, "entries", budget)? {
        Some(Value::Array(entries)) => entries.as_slice(),
        _ => return Err(unsupported("characterBook")),
    };
    let book_extensions = field(book, "extensions", budget)?;
    let settings = if !nullish(field(book, "recursive_scanning", budget)?) && !nullish(field(book, "scan_depth", budget)?)
        && !nullish(field(book, "token_budget", budget)?) {
        let mut settings = empty_object(budget)?;
        set(&mut settings, "tokenBudget", copy_or_undefined(field(book, "token_budget", budget)?, budget)?, budget)?;
        set(&mut settings, "scanDepth", copy_or_undefined(field(book, "scan_depth", budget)?, budget)?, budget)?;
        set(&mut settings, "recursiveScanning", copy_or_undefined(field(book, "recursive_scanning", budget)?, budget)?, budget)?;
        let full_word = match book_extensions { Some(extensions) => field(extensions, "risu_fullWordMatching", budget)?, None => None };
        set(&mut settings, "fullWordMatching", coalesce(full_word, || Ok(Value::Bool(false)), budget)?, budget)?;
        settings
    } else {
        Value::Undefined
    };
    let mut lorebook = Vec::with_capacity(entries.len());
    for entry in entries {
        budget.work(1)?;
        lorebook.push(convert_entry(entry, budget)?);
    }
    Ok(Converted { lorebook, settings, extensions: copy_or_undefined(book_extensions, budget)? })
}

fn convert_entry(entry: &Value, budget: &Budget) -> Result<Value, StageError> {
    let declined = || unsupported("lorebookEntry");
    if object(Some(entry)).is_none() { return Err(declined()); }
    let Some(Value::Array(keys)) = field(entry, "keys", budget)? else { return Err(declined()) };
    let keys = keys.as_slice();
    let secondary = match field(entry, "secondary_keys", budget)? {
        Some(Value::Array(values)) => Some(values.as_slice()),
        None | Some(Value::Null) => None,
        Some(_) => return Err(declined()),
    };
    let mut content = copy_or_undefined(field(entry, "content", budget)?, budget)?;
    let mut selective = copy_or_undefined(field(entry, "selective", budget)?, budget)?;

    // `book.use_regex && !book.keys?.[0]?.startsWith('/')` turns the flag off.
    let use_regex_field = field(entry, "use_regex", budget)?;
    let mut use_regex = copy_or_undefined(use_regex_field, budget)?;
    if truthy(use_regex_field) {
        let regex_key = match keys.first() {
            None | Some(Value::Undefined | Value::Null) => false,
            Some(Value::Text(first)) => starts_with(first, "/"),
            Some(_) => return Err(declined()),
        };
        if !regex_key { use_regex = Value::Bool(false); }
    }

    // `book.extensions ?? {}`; the same object when present, so its deletions show in `book.extensions?.…` below.
    let stored = field(entry, "extensions", budget)?;
    let present = !nullish(stored);
    let mut extensions = match stored {
        None | Some(Value::Null) => empty_object(budget)?,
        Some(stored @ Value::Object(_)) => copy(stored, budget)?,
        Some(_) => return Err(declined()),
    };
    let prefix = |content: &Value, parts: &[&Text]| -> Result<Value, StageError> { Ok(Value::Text(concat(parts, Some(content), budget)?)) };
    let line = |value: &str| Text::from_str(value, budget);

    let probability = field(&extensions, "probability", budget)?;
    if truthy(field(&extensions, "useProbability", budget)?) && probability.is_some() && !matches!(probability, Some(Value::Number(value)) if *value == 100.0) {
        let value = string(probability, budget)?;
        content = prefix(&content, &[&line("@@probability ")?, &value, &line("\n")?])?;
        remove(&mut extensions, "useProbability", budget)?;
        remove(&mut extensions, "probability", budget)?;
    }
    if let (Some(Value::Number(position)), Some(Value::Number(_)), Some(Value::Number(role))) =
        (field(&extensions, "position", budget)?, field(&extensions, "depth", budget)?, field(&extensions, "role", budget)?) {
        if *position == 4.0 {
            let role = match *role { value if value == 0.0 => "system", value if value == 1.0 => "user", value if value == 2.0 => "assistant", _ => "undefined" };
            let depth = string(field(&extensions, "depth", budget)?, budget)?;
            content = prefix(&content, &[&line("@@depth ")?, &depth, &line("\n@@role ")?, &line(role)?, &line("\n")?])?;
            remove(&mut extensions, "position", budget)?;
            remove(&mut extensions, "depth", budget)?;
            remove(&mut extensions, "role", budget)?;
        }
    }
    if let (Some(Value::Number(logic)), Some(secondary)) = (field(&extensions, "selectiveLogic", budget)?, secondary) {
        if !secondary.is_empty() {
            let logic = *logic;
            if logic == 1.0 {
                selective = Value::Bool(false);
                content = prefix(&content, &[&line("@@exclude_keys_all ")?, &join(secondary, ",", budget)?, &line("\n")?])?;
            } else if logic == 2.0 || logic == 3.0 {
                selective = Value::Bool(false);
                let decorator = line(if logic == 2.0 { "@@exclude_keys " } else { "@@additional_keys " })?;
                for key in secondary {
                    budget.work(1)?;
                    content = prefix(&content, &[&decorator, &string(Some(key), budget)?, &line("\n")?])?;
                }
            }
        }
    }
    if let Some(Value::Number(delay)) = field(&extensions, "delay", budget)? {
        if *delay > 0.0 {
            let delay = string(field(&extensions, "delay", budget)?, budget)?;
            content = prefix(&content, &[&line("@@activate_only_after ")?, &delay, &line("\n")?])?;
            remove(&mut extensions, "delay", budget)?;
        }
    }
    if let Some(Value::Bool(whole)) = field(&extensions, "match_whole_words", budget)? {
        let decorator = if *whole { "@@match_full_word\n" } else { "@@match_partial_word\n" };
        content = prefix(&content, &[&line(decorator)?])?;
        remove(&mut extensions, "match_whole_words", budget)?;
    }

    let activation = if present { copy_or_undefined(field(&extensions, "risu_activationPercent", budget)?, budget)? } else { Value::Undefined };
    let lore_cache = if present { coalesce(field(&extensions, "risu_loreCache", budget)?, || Ok(Value::Null), budget)? } else { Value::Null };
    let mut spread = extensions;
    set(&mut spread, "risu_case_sensitive", copy_or_undefined(field(entry, "case_sensitive", budget)?, budget)?, budget)?;

    let mut lore = empty_object(budget)?;
    set(&mut lore, "key", Value::Text(join(keys, ", ", budget)?), budget)?;
    let secondkey = match secondary { Some(values) => Value::Text(join(values, ", ", budget)?), None => text("", budget)? };
    set(&mut lore, "secondkey", secondkey, budget)?;
    set(&mut lore, "insertorder", copy_or_undefined(field(entry, "insertion_order", budget)?, budget)?, budget)?;
    let comment = match field(entry, "name", budget)? {
        name if !nullish(name) => copy_or_undefined(name, budget)?,
        _ => coalesce(field(entry, "comment", budget)?, || text("", budget), budget)?,
    };
    set(&mut lore, "comment", comment, budget)?;
    set(&mut lore, "content", content, budget)?;
    set(&mut lore, "mode", coalesce(field(entry, "mode", budget)?, || text("normal", budget), budget)?, budget)?;
    set(&mut lore, "alwaysActive", coalesce(field(entry, "constant", budget)?, || Ok(Value::Bool(false)), budget)?, budget)?;
    set(&mut lore, "selective", coalesce(Some(&selective), || Ok(Value::Bool(false)), budget)?, budget)?;
    set(&mut lore, "extentions", spread, budget)?;
    set(&mut lore, "activationPercent", activation, budget)?;
    set(&mut lore, "loreCache", lore_cache, budget)?;
    set(&mut lore, "useRegex", coalesce(Some(&use_regex), || Ok(Value::Bool(false)), budget)?, budget)?;
    set(&mut lore, "folder", copy_or_undefined(field(entry, "folder", budget)?, budget)?, budget)?;
    Ok(lore)
}
