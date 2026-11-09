//! Global note, author note, depth prompt, post-history instructions and additional text.
//!
//! Also the other prepared items of `sendChatInternal` that are not chat history: the main and jailbreak prompts, the
//! description, the persona prompt, the inlay instructions and the biases, and the inputs the machine declines.

use crate::history::{self, Selection};
use crate::machine::{invalid, js, message, unsupported, Ctx, Prompt};
use risunest_ir::chat::Role;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};
use risunest_records::character::Character;
use risunest_records::conversation::Conversation;
use risunest_records::message::Message;
use risunest_records::modules::{ModuleSources, SelectedModules, select_conversation_modules_with_integration};
use risunest_records::persona;
use risunest_records::root::is_text;
use risunest_records::settings::Settings;

/// Refuses the inputs whose TS behavior this stage does not reproduce yet, from the stored records alone.
pub(crate) fn admit(db: &Settings<'_>, character: Character<'_>, conversation: Conversation<'_>, rows: &[Message<'_>],
    selection: &Selection, modules: Option<&SelectedModules>, budget: &Budget) -> Result<(), StageError> {
    // `sendChat` prepares a group chat through its members.
    if character.is_group(budget)? { return Err(unsupported("groupChat")); }
    // `activatePresetChainForRequest` picks a preset at random before preparation; a falsy chain picks none.
    if db.truthy("presetChain", budget)? { return Err(unsupported("presetChain")); }
    // Stores the prompt text on the response and runs the Lua `editRequest` listeners a second time.
    if db.truthy("promptInfoInsideChat", budget)? { return Err(unsupported("promptInfoInsideChat")); }
    match modules {
        Some(modules) => admit_selected_modules(modules, budget)?,
        None if modules_active(db, character, conversation, budget)? => return Err(unsupported("modules")),
        None => {}
    }
    if uses_memory(db, character, budget)? && !db.truthy("hanuraiEnable", budget)? && !db.truthy("hypav2", budget)?
        && db.truthy("hypaV3", budget)? {
        return Err(unsupported("hypaV3"));
    }
    // `additionalInformations` searches the text with embeddings.
    if character.truthy("additionalText", budget)? { return Err(unsupported("additionalText")); }
    // `characterFormatUpdate` moves these into the author note before a send.
    if character.truthy("postHistoryInstructions", budget)? { return Err(unsupported("postHistoryInstructions")); }
    // The composed selector validates and prepares each stored lore entry.
    for lore in [character.get("globalLore", budget)?, conversation.get("localLore", budget)?] {
        match lore {
            None | Some(Value::Null) => {}
            Some(Value::Array(_)) => {}
            Some(_) => return Err(unsupported("lorebook")),
        }
    }
    history::admit(rows, selection, character, budget)
}

// Legacy constructors remain closed to every canonically selected module.
fn modules_active(db: &Settings<'_>, character: Character<'_>, conversation: Conversation<'_>, budget: &Budget) -> Result<bool, StageError> {
    let sources = ModuleSources::for_conversation(db.root().value(), character.value(), conversation.value(), budget)?;
    Ok(!select_conversation_modules_with_integration(&sources, db.get("moduleIntergration", budget)?, budget)?.is_empty())
}

pub(crate) fn admit_selected_modules(modules: &SelectedModules, budget: &Budget) -> Result<(), StageError> {
    for module in modules.records() {
        budget.work(1)?;
        for key in [b"trigger".as_slice(), b"assets"] {
            if let Some(value) = module.get(key, budget)?.filter(|value| value.truthy()) {
                if !matches!(value, Value::Array(values) if values.as_slice().is_empty()) {
                    return Err(unsupported("modules"));
                }
            }
        }
        if module.get(b"customModuleToggle", budget)?.is_some_and(Value::truthy) {
            return Err(unsupported("modules"));
        }
        if let Some(mcp) = module.get(b"mcp", budget)? {
            if mcp.get(b"url", budget)?.is_some_and(Value::truthy) { return Err(unsupported("modules")); }
        }
    }
    Ok(())
}

/// Whether the send takes the memory branch instead of trimming.
pub(crate) fn uses_memory(db: &Settings<'_>, character: Character<'_>, budget: &Budget) -> Result<bool, StageError> {
    Ok(character.truthy("supaMemory", budget)?
        && (!is_text(db.get("supaModelType", budget)?, "none") || db.truthy("hanuraiEnable", budget)?
            || db.truthy("hypav2", budget)? || db.truthy("hypaV3", budget)?))
}

// `risuChatParser(value)` on a value that is not a string is left to TS.
fn text_or(value: Option<&Value>, name: &'static str, budget: &Budget) -> Result<js::S, StageError> {
    match value { Some(Value::Text(text)) => js::units(text, budget), _ => Err(unsupported(name)) }
}

// `formatPrompt(data)`: `@@role` markers split the text into messages; text before the first marker is dropped.
fn format_prompt(data: &[u16], budget: &Budget) -> Result<Vec<(Role, js::S)>, StageError> {
    let prefixed;
    let data = if data.starts_with(&js::s("@@")) { data } else {
        prefixed = js::concat(&[&js::s("@@system\n"), data]);
        &prefixed
    };
    let mut markers = Vec::new();
    let mut at = 0;
    while at < data.len() {
        match marker(data, at) {
            Some((end, role)) => { markers.push((at, end, role)); at = end; }
            None => at += 1,
        }
    }
    let mut output = Vec::with_capacity(markers.len());
    for (index, (_, end, role)) in markers.iter().enumerate() {
        let next = markers.get(index + 1).map_or(data.len(), |next| next.0);
        output.push((*role, js::trim(&data[*end..next], budget)?));
    }
    Ok(output)
}

// `/@@@?(user|assistant|system)\n/` at `at`.
fn marker(data: &[u16], at: usize) -> Option<(usize, Role)> {
    if !data[at..].starts_with(&js::s("@@")) { return None; }
    let after = at + 2;
    let starts: &[usize] = if data.get(after) == Some(&0x40) { &[after + 1, after] } else { &[after] };
    for &start in starts {
        for (name, role) in [("user", Role::User), ("assistant", Role::Assistant), ("system", Role::System)] {
            let name = js::concat(&[&js::s(name), &[0x0a]]);
            if data[start..].starts_with(&name) { return Some((start + name.len(), role)); }
        }
    }
    None
}

// `systemPrompt?.replaceAll('{{original}}', original) || original`.
fn replace_original(replacement: Option<&Text>, original: &[u16], budget: &Budget) -> Result<js::S, StageError> {
    if let Some(replacement) = replacement {
        let replaced = js::replace(&js::units(replacement, budget)?, &js::s("{{original}}"), original, true, budget)?;
        if !replaced.is_empty() { return Ok(replaced); }
    }
    Ok(original.to_vec())
}

/// The main prompt, the jailbreak and the global note, which only the `formatingOrder` path prepares.
pub(crate) fn main_prompts(ctx: &mut Ctx, prompt: &mut Prompt, db: &Settings<'_>, character: Character<'_>) -> Result<(), StageError> {
    let budget = ctx.budget();
    // `mainp + ...` and the template literal read these as `String(value)`.
    let main = js::string(db.get("mainPrompt", &budget)?, "settings.mainPrompt", &budget)?;
    let mut main = replace_original(character.text("systemPrompt", &budget)?, &main, &budget)?;
    let additional = db.get("additionalPrompt", &budget)?;
    if db.truthy("promptPreprocess", &budget)? && !matches!(additional, Some(Value::Text(text)) if text.len_units() == 0) {
        main = js::concat(&[&main, &[0x0a], &js::string(additional, "settings.additionalPrompt", &budget)?]);
    }
    let parsed = ctx.parse_plain(&main)?;
    for (role, content) in format_prompt(&parsed, &budget)? {
        let index = prompt.add(message(role, &content, &budget)?);
        prompt.groups.main.push(index);
    }
    if db.truthy("jailbreakToggle", &budget)? {
        let jailbreak = text_or(db.get("jailbreak", &budget)?, "settings.jailbreak", &budget)?;
        let parsed = ctx.parse_plain(&jailbreak)?;
        for (role, content) in format_prompt(&parsed, &budget)? {
            let index = prompt.add(message(role, &content, &budget)?);
            prompt.groups.jailbreak.push(index);
        }
    }
    let note = text_or(db.get("globalNote", &budget)?, "settings.globalNote", &budget)?;
    let note = replace_original(character.text("replaceGlobalNote", &budget)?, &note, &budget)?;
    let parsed = ctx.parse_plain(&note)?;
    for (role, content) in format_prompt(&parsed, &budget)? {
        let index = prompt.add(message(role, &content, &budget)?);
        prompt.groups.global_note.push(index);
    }
    Ok(())
}

// `getAuthorNoteDefaultText()`: the first author note card's `defaultText` in the stored template.
fn author_note_default(db: &Settings<'_>, budget: &Budget) -> Result<js::S, StageError> {
    let cards = match db.get("promptTemplate", budget)? {
        Some(Value::Array(cards)) => cards.as_slice(),
        Some(value) if value.truthy() => return Err(invalid("settings.promptTemplate")),
        _ => return Ok(Vec::new()),
    };
    for card in cards {
        budget.work(1)?;
        if matches!(card, Value::Null | Value::Undefined) { return Err(invalid("promptTemplate")); }
        if !is_text(card.get(b"type", budget)?, "authornote") { continue; }
        return match card.get(b"defaultText", budget)? {
            None | Some(Value::Undefined | Value::Null) => Ok(Vec::new()),
            Some(Value::Text(text)) => js::units(text, budget),
            Some(_) => Err(invalid("promptTemplate.defaultText")),
        };
    }
    Ok(Vec::new())
}

/// The conversation's author note, or the template's default text.
pub(crate) fn author_note(ctx: &mut Ctx, prompt: &mut Prompt, db: &Settings<'_>, conversation: Conversation<'_>) -> Result<(), StageError> {
    let budget = ctx.budget();
    // `if (currentChat.note)`: a falsy note takes the default, and CBS throws on a truthy non-string.
    let note = match conversation.get("note", &budget)? {
        Some(Value::Text(note)) if note.len_units() != 0 => js::units(note, &budget)?,
        Some(note) if note.truthy() => return Err(invalid("conversation.note")),
        _ => {
            let default = author_note_default(db, &budget)?;
            if default.is_empty() { return Ok(()); }
            default
        }
    };
    let content = ctx.parse_plain(&note)?;
    let index = prompt.add(message(Role::System, &content, &budget)?);
    prompt.groups.author_note.push(index);
    Ok(())
}

/// `baseDescriptionPrompt`: the description with its prefix, personality and scenario. Returns its arena index.
pub(crate) fn description(ctx: &mut Ctx, prompt: &mut Prompt, db: &Settings<'_>, character: Character<'_>) -> Result<usize, StageError> {
    let budget = ctx.budget();
    // `(promptPreprocess ? descriptionPrefix : '') + desc`, a JS `+`: it concatenates when either operand is a string, array
    // or object, and adds two other values as numbers.
    let desc = character.get("desc", &budget)?;
    let prefix = if db.truthy("promptPreprocess", &budget)? {
        let prefix = db.get("descriptionPrefix", &budget)?;
        let joins = |value: Option<&Value>| matches!(value, Some(Value::Text(_) | Value::Array(_) | Value::Object(_)));
        if !joins(prefix) && !joins(desc) { return Err(unsupported("character.desc")); }
        js::string(prefix, "settings.descriptionPrefix", &budget)?
    } else { Vec::new() };
    let desc = js::string(desc, "character.desc", &budget)?;
    let mut content = ctx.parse_plain(&js::concat(&[&prefix, &desc]))?;
    for (field, label) in [("personality", "\n\nDescription of {{char}}: "), ("scenario", "\n\nCircumstances and context of the dialogue: ")] {
        let Some(value) = character.text(field, &budget)?.filter(|value| value.len_units() != 0) else { continue };
        let parsed = ctx.parse_plain(&js::concat(&[&js::s(label), &js::units(value, &budget)?]))?;
        content.extend_from_slice(&parsed);
    }
    let index = prompt.add(message(Role::System, &content, &budget)?);
    prompt.groups.description.push(index);
    Ok(index)
}

/// The persona prompt: `getPersonaPrompt()` when the effective `personaPrompt` is set.
pub(crate) fn persona(ctx: &mut Ctx, prompt: &mut Prompt, db: &Settings<'_>, conversation: Conversation<'_>) -> Result<(), StageError> {
    let budget = ctx.budget();
    if !db.truthy("personaPrompt", &budget)? { return Ok(()); }
    let bound = persona::bound(db.root(), conversation, &budget)?;
    let content = match bound {
        Some(bound) => match bound.get("personaPrompt", &budget)? {
            Some(Value::Text(text)) => js::units(text, &budget)?,
            _ => return Err(invalid("persona.personaPrompt")),
        },
        None => db.text("personaPrompt", &budget)?.map_or(Ok(Vec::new()), |text| js::units(text, &budget))?,
    };
    let content = ctx.parse_plain(&content)?;
    let index = prompt.add(message(Role::System, &content, &budget)?);
    prompt.groups.persona_prompt.push(index);
    Ok(())
}

/// The emotion and image view instructions, which TS adds without a CBS pass.
pub(crate) fn inlay_instructions(prompt: &mut Prompt, character: Character<'_>, budget: &Budget) -> Result<(), StageError> {
    if !character.truthy("inlayViewScreen", budget)? { return Ok(()); }
    let view = character.get("viewScreen", budget)?;
    let content = if is_text(view, "emotion") {
        let data = character.new_gen_data(budget)?.ok_or_else(|| invalid("character.newGenData"))?;
        let instructions = js::units(data.required_text("emotionInstructions", budget)?, budget)?;
        let mut names = Vec::new();
        for (index, image) in character.array("emotionImages", budget)?.unwrap_or_default().iter().enumerate() {
            if index > 0 { names.extend_from_slice(&js::s(", ")); }
            match image.array().and_then(|image| image.first()) {
                Some(Value::Text(name)) => names.extend(js::units(name, budget)?),
                Some(Value::Null | Value::Undefined) | None if image.array().is_some() => {}
                _ => return Err(invalid("character.emotionImages")),
            }
        }
        js::replace(&instructions, &js::s("{{slot}}"), &names, true, budget)?
    } else if is_text(view, "imggen") {
        let data = character.new_gen_data(budget)?.ok_or_else(|| invalid("character.newGenData"))?;
        js::units(data.required_text("instructions", budget)?, budget)?
    } else {
        return Ok(());
    };
    let index = prompt.add(message(Role::System, &content, budget)?);
    prompt.groups.post_everything.push(index);
    Ok(())
}

/// `biases`: the stored and character biases with their escapes and CBS evaluated.
pub(crate) fn biases(ctx: &mut Ctx, db: &Settings<'_>, character: Character<'_>) -> Result<Vec<(Text, f64)>, StageError> {
    let budget = ctx.budget();
    let stored = match db.get("bias", &budget)? { Some(Value::Array(values)) => values.as_slice(), _ => return Err(invalid("settings.bias")) };
    // `concat` spreads an array and appends any other value as one entry; `concat(undefined)` appends `undefined`,
    // whose `v[0]` throws.
    let own = match character.get("bias", &budget)? {
        Some(Value::Array(values)) => values.as_slice(),
        None | Some(Value::Null) => return Err(invalid("character.bias")),
        Some(value) => std::slice::from_ref(value),
    };
    let mut output = Vec::with_capacity(stored.len() + own.len());
    for entry in stored.iter().chain(own) {
        // Each entry is read as `[v[0].replaceAll(...), v[1]]`: a string or object entry indexes too, a key other than a
        // string throws, and the weight passes unchecked.
        let pair = match entry.array() {
            Some(pair) => pair,
            None if matches!(entry, Value::Text(_) | Value::Object(_)) => return Err(unsupported("bias")),
            None => return Err(invalid("bias")),
        };
        let Some(Value::Text(key)) = pair.first() else { return Err(invalid("bias")) };
        let Some(Value::Number(weight)) = pair.get(1) else { return Err(unsupported("bias")) };
        let mut key = js::units(key, &budget)?;
        for (from, to) in [("\\n", "\n"), ("\\r", "\r"), ("\\\\", "\\")] {
            key = js::replace(&key, &js::s(from), &js::s(to), true, &budget)?;
        }
        let key = ctx.parse_plain(&key)?;
        output.push((js::text(&key, &budget)?, *weight));
    }
    Ok(output)
}

/// `depth_prompt`: spliced at `formated.length - depth` after the final trim.
pub(crate) fn depth_prompt(ctx: &mut Ctx, prompt: &mut Prompt, formated: &mut Vec<usize>, character: Character<'_>) -> Result<(), StageError> {
    let budget = ctx.budget();
    let Some(depth_prompt @ Value::Object(_)) = character.get("depth_prompt", &budget)? else { return Ok(()) };
    let text = match depth_prompt.get(b"prompt", &budget)? {
        Some(Value::Text(text)) if text.len_units() != 0 => js::units(text, &budget)?,
        // A falsy prompt, or one without a `length`, is skipped.
        None | Some(Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) | Value::Text(_)) => return Ok(()),
        Some(_) => return Err(invalid("character.depth_prompt.prompt")),
    };
    let depth = match depth_prompt.get(b"depth", &budget)? {
        Some(Value::Number(depth)) => *depth,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(value)) => f64::from(u8::from(*value)),
        Some(Value::Text(depth)) => risunest_ir::num::to_number(depth, &budget)?,
        _ => f64::NAN,
    };
    // `splice(start, 0, item)`: NaN reads as 0, a negative start counts from the end.
    let length = formated.len() as f64;
    let start = length - depth;
    let start = if start.is_nan() { 0.0 } else { start.trunc() };
    let at = if start < 0.0 { (length + start).max(0.0) } else { start.min(length) } as usize;
    let content = ctx.parse_plain(&text)?;
    let index = prompt.add(message(Role::System, &content, &budget)?);
    formated.insert(at, index);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_prompt_splits_on_role_markers() {
        let view = |input: &str| -> Vec<(Role, String)> {
            format_prompt(&js::s(input), &crate::machine::tests::budget()).unwrap().into_iter().map(|(role, content)| (role, String::from_utf16(&content).unwrap())).collect()
        };
        assert_eq!(view(" A "), vec![(Role::System, "A".into())]);
        assert_eq!(view("@@@user\n B \n@@assistant\nC"), vec![(Role::User, "B".into()), (Role::Assistant, "C".into())]);
        // Text before the first marker is dropped, including a marker this pattern does not read.
        assert_eq!(view("@@other\nX@@@@system\nY"), vec![(Role::System, "Y".into())]);
        assert_eq!(view("@@user\n"), vec![(Role::User, "".into())]);
    }
}
