//! Text shaping the text-completion families (Kobold, Ooba, NovelAI, NovelList, Horde) share:
//! `getStopStrings`, `getUnstringlizerChunks` and `unstringlizeChat` (`src/ts/process/stringlize.ts`).

use crate::common::js::{self, J};
use risunest_ir::chat::PromptMessage;
use risunest_ir::{Budget, Error, Text, Value};

#[cfg(test)]
pub(crate) mod testkit;

/// `getStopStrings(false)`. `user_prefix` is `db.ooba.formating.userPrefix` (`None` when it is `undefined`, which
/// `JSON.stringify` writes as `null`) and `username` is `db.username`. Duplicates keep their first position, as
/// `[...new Set(..)]` does.
pub fn stop_strings(user_prefix: Option<&Text>, username: &Text, budget: &Budget) -> Result<Vec<Option<Text>>, Error> {
    let mut output: Vec<Option<Text>> = Vec::new();
    let push = |value: Option<Text>, output: &mut Vec<Option<Text>>| {
        let duplicate = output.iter().any(|known| match (known, &value) {
            (None, None) => true,
            (Some(known), Some(value)) => known.equals(value),
            _ => false,
        });
        if !duplicate { output.push(value); }
    };
    for fixed in ["GPT4 User", "</s>", "<|end", "<|im_end"] { push(Some(js::text(fixed, budget)?), &mut output); }
    push(user_prefix.map(|prefix| prefix.copy(budget)).transpose()?, &mut output);
    push(Some(js::concat(username, &js::text(":", budget)?, budget)?), &mut output);
    for user in ["user", "human", "input", "inst", "instruction"] {
        // `user.toLowerCase()`, `user.toUpperCase()` and the title case of `user.replace(/\w\S*/g, toTitleCase)`.
        let title = format!("{}{}", &user[..1].to_ascii_uppercase(), &user[1..]);
        for form in [user.to_owned(), user.to_ascii_uppercase(), title] {
            for shaped in [format!("{form}:"), format!("<<{form}>>"), format!("### {form}")] {
                push(Some(js::text(&shaped, budget)?), &mut output);
            }
        }
    }
    Ok(output)
}

/// `object[name]` of a stored settings object (`db.ooba`, `db.ainconfig`), `undefined` when it is missing.
pub(crate) fn field(object: &Value, name: &str, budget: &Budget) -> Result<J, Error> {
    let Value::Object(entries) = object else { return Ok(J::Undefined) };
    match entries.as_slice().iter().find(|(key, _)| key.equals_ascii(name.as_bytes())) {
        Some((_, value)) => js::from_value(value, budget),
        None => Ok(J::Undefined),
    }
}

/// Which name suffixes `getUnstringlizerChunks` builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkMode { Normal, Ain }

/// `getUnstringlizerChunks(formated, char, mode)`: `(chunks, extChunk)`. `user_name` is `getUserName()`.
pub fn unstringlizer_chunks(
    formated: &[PromptMessage], char_name: &Text, user_name: &Text, mode: ChunkMode, budget: &Budget,
) -> Result<(Vec<Vec<u16>>, Vec<Vec<u16>>), Error> {
    let mut chunks: Vec<Vec<u16>> = ["system note:", "system:", "system note\u{ff1a}", "system\u{ff1a}"].iter().map(|text| js::utf16(text)).collect();
    let mut names: Vec<Vec<u16>> = Vec::new();
    let suffixes: &[&str] = match mode { ChunkMode::Ain => &[" ", "\u{3000}"], ChunkMode::Normal => &[":", "\u{ff1a}", ": ", "\u{ff1a} "] };
    let with = |name: &[u16], suffix: &str| -> Vec<u16> { let mut value = name.to_vec(); value.extend(js::utf16(suffix)); value };
    for name in [char_name, user_name] {
        if name.len_units() == 0 { continue; }
        budget.work(1)?;
        let name = js::units(name);
        for suffix in suffixes { chunks.push(with(&name, suffix)); }
        names.push(name);
    }
    for form in formated {
        let Some(name) = form.name.as_ref().filter(|name| name.len_units() != 0) else { continue };
        budget.work(chunks.len().saturating_add(1))?;
        let name = js::units(name);
        if !chunks.contains(&with(&name, suffixes[0])) {
            for suffix in suffixes { chunks.push(with(&name, suffix)); }
        }
        names.push(name);
    }
    names.extend(chunks.iter().cloned());
    Ok((chunks, names))
}

/// `unstringlizeChat(text, formated, char)`: the text before the earliest name or system chunk, trimmed.
pub fn unstringlize_chat(text: &Text, formated: &[PromptMessage], char_name: &Text, user_name: &Text, budget: &Budget) -> Result<Text, Error> {
    let (chunks, _) = unstringlizer_chunks(formated, char_name, user_name, ChunkMode::Normal, budget)?;
    let units = js::units(text);
    budget.work(units.len().saturating_mul(chunks.len()))?;
    let earliest = chunks.iter().filter_map(|chunk| js::find_units(&units, chunk, 0)).min();
    match earliest {
        Some(at) => js::from_units(&units[..at], budget)?.trim(budget),
        None => text.copy(budget),
    }
}
