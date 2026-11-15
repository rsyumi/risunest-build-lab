//! Chat history formatting and selection.

use crate::items::{closing, Template};
use crate::machine::{invalid, js, message, unsupported, Ctx, Prompt};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::chat_ml::take_thoughts;
use risunest_ir::stage::{CbsConditions, ScriptMode, ScriptRequest, StageError, TextContext};
use risunest_ir::{Budget, Text, Value};
use risunest_records::character::Character;
use risunest_records::conversation::Conversation;
use risunest_records::message::{Disabled, Message};
use risunest_records::root::{is_text, Fields};
use risunest_records::settings::Settings;

/// The current chat parser pass (`runCurrentChatParserPass`): CBS with `runVar` over every stored message, disabled
/// ones included. TS stores what it returns and the chat variables it writes; a message the pass would rewrite, or a
/// write it makes, is declined.
pub(crate) fn parse_current_chat(ctx: &mut Ctx, rows: &[Message<'_>]) -> Result<(), StageError> {
    let budget = ctx.budget();
    for row in rows {
        let data = js::units(row.data(&budget)?, &budget)?;
        let written = ctx.effects.len();
        let parsed = ctx.parse(&data, TextContext { run_var: true, ..TextContext::default() })?;
        if parsed != data || ctx.effects.len() != written { return Err(unsupported("historyRewrite")); }
    }
    Ok(())
}

/// `selectPromptHistory`: the history after the last `allBefore` message, without disabled messages.
#[derive(Debug)]
pub(crate) struct Selection {
    pub(crate) start: usize,
    pub(crate) reset_by_all_before: bool,
    /// Enabled messages after `start`.
    pub(crate) message_count: usize,
}

pub(crate) fn select(rows: &[Message<'_>], budget: &Budget) -> Result<Selection, StageError> {
    let mut selection = Selection { start: 0, reset_by_all_before: false, message_count: 0 };
    for (index, row) in rows.iter().enumerate().rev() {
        budget.work(1)?;
        match row.disabled(budget)? {
            Disabled::Yes => {}
            Disabled::AllBefore => { selection.start = index + 1; selection.reset_by_all_before = true; break; }
            Disabled::No => selection.message_count += 1,
        }
    }
    // TS gives a nullish `chatId` a fresh ID before preparation, and a falsy one when the message is read for the
    // prompt (`message.chatId || createId()`); both are stored changes. It keeps a truthy non-string one as the memo.
    for (index, row) in rows.iter().enumerate() {
        let read = index >= selection.start && row.disabled(budget)? == Disabled::No;
        match row.fields().get("chatId", budget)? {
            None | Some(Value::Null) => return Err(unsupported("messageIdNormalization")),
            Some(id) if read && !id.truthy() => return Err(unsupported("messageIdNormalization")),
            Some(Value::Text(_)) => {}
            Some(_) if read => return Err(unsupported("message.chatId")),
            Some(_) => {}
        }
    }
    Ok(selection)
}

/// `fmIndex === -1 ? firstMessage : alternateGreetings[fmIndex]`. TS parses whatever this finds, so an index that
/// names no string, or one into a stored list that is not an array, is declined.
pub(crate) fn greeting_source<'a>(character: Character<'a>, conversation: Conversation<'_>, budget: &Budget) -> Result<&'a Text, StageError> {
    let greeting = match conversation.get("fmIndex", budget)? {
        Some(Value::Number(index)) if *index == -1.0 => character.get("firstMessage", budget)?,
        Some(Value::Number(index)) if *index >= 0.0 && index.fract() == 0.0 => match character.get("alternateGreetings", budget)? {
            Some(Value::Array(greetings)) => greetings.as_slice().get(*index as usize),
            _ => None,
        },
        _ => None,
    };
    match greeting { Some(Value::Text(greeting)) => Ok(greeting), _ => Err(unsupported("fmIndex")) }
}

/// The greeting from [`greeting_source`] through CBS and `editprocess`.
pub(crate) async fn greeting(ctx: &mut Ctx, greeting: &Text, character: Character<'_>, template: &Template,
    prompt_settings: Fields<'_>) -> Result<PromptMessage, StageError> {
    let budget = ctx.budget();
    let parsed = ctx.parse_plain(&js::units(greeting, &budget)?)?;
    let request = ScriptRequest { mode: ScriptMode::EditProcess, input: js::text(&parsed, &budget)?, chat_index: -1,
        conditions: CbsConditions::default() };
    let mut content = ctx.script(request).await?;
    let budget = ctx.budget();
    let mut entry = message(Role::Assistant, &[], &budget)?;
    if template.using && prompt_settings.truthy("sendName", &budget)? {
        content = js::concat(&[&js::units(character.name(&budget)?, &budget)?, &js::s(": "), &content]);
        entry.attr = Some(vec![Text::from_str("nameAdded", &budget)?]);
    }
    entry.content = js::text(&content, &budget)?;
    Ok(entry)
}

/// The inlay markers in a history message: `/{{(inlay|inlayed|inlayeddata)::(.+?)}}/g` as `(start, end, data)`.
fn inlays(text: &[u16]) -> Vec<(usize, usize, bool)> {
    let mut output = Vec::new();
    let mut from = 0;
    let open = js::s("{{inlay");
    while let Some(at) = js::find(text, &open, from) {
        let rest = &text[at + open.len()..];
        let (data, name) = if rest.starts_with(&js::s("::")) { (false, 0) }
            else if rest.starts_with(&js::s("ed::")) { (false, 2) }
            else if rest.starts_with(&js::s("eddata::")) { (true, 6) }
            else { from = at + 1; continue };
        let start = at + open.len() + name + 2;
        match closing(text, start, false) {
            Some(end) => { output.push((at, end + 2, data)); from = end + 2; }
            None => from = at + 1,
        }
    }
    output
}

// `/\{\{asset_?prompt::(.+?)\}\}/gmsiu` matches as `(start, end, name)`; `i` with `u` also folds U+017F to `s`.
fn asset_prompts(text: &[u16]) -> Vec<(usize, usize, js::S)> {
    fn letter(unit: u16, expected: u8) -> bool {
        let lower = if (0x41..=0x5a).contains(&unit) { unit + 0x20 } else { unit };
        lower == u16::from(expected) || (expected == b's' && unit == 0x17f)
    }
    fn word(text: &[u16], at: usize, expected: &str) -> Option<usize> {
        let end = at + expected.len();
        (end <= text.len() && expected.bytes().zip(&text[at..end]).all(|(expected, unit)| letter(*unit, expected))).then_some(end)
    }
    let mut output = Vec::new();
    let mut at = 0;
    while at + 1 < text.len() {
        let matched = (|| {
            if text[at] != 0x7b || text[at + 1] != 0x7b { return None; }
            let mut index = word(text, at + 2, "asset")?;
            if text.get(index) == Some(&0x5f) && word(text, index + 1, "prompt").is_some() { index += 1; }
            let index = word(text, index, "prompt")?;
            if !text[index..].starts_with(&js::s("::")) { return None; }
            let end = closing(text, index + 2, true)?;
            Some((at, end + 2, text[index + 2..end].to_vec()))
        })();
        match matched {
            Some(found) => { at = found.1; output.push(found); }
            None => at += 1,
        }
    }
    output
}

// An inlay needs its stored asset: the character's own `inlay` and `inlayed` markers are removed, any other is declined.
fn check_inlays(found: &[(usize, usize, bool)], role: &Text) -> Result<(), StageError> {
    let declined = if role.equals_ascii(b"char") { found.iter().any(|(_, _, data)| *data) } else { !found.is_empty() };
    if declined { Err(unsupported("inlay")) } else { Ok(()) }
}

// An asset prompt that names a stored asset, or `icon`, adds the image; the others are removed.
fn check_asset_prompts(prompts: &[(usize, usize, js::S)], assets: &[Value]) -> Result<(), StageError> {
    for (_, _, name) in prompts {
        let stored = assets.iter().any(|asset| matches!(asset.array().and_then(|asset| asset.first()),
            Some(Value::Text(asset)) if asset.units().eq(name.iter().copied())));
        if stored || js::equals(name, "icon") { return Err(unsupported("assetPrompt")); }
    }
    Ok(())
}

/// The inlay and asset prompt declines on the stored text of the messages the prompt reads. The history loop checks
/// the text again after CBS and the `editprocess` hooks.
pub(crate) fn admit(rows: &[Message<'_>], selection: &Selection, character: Character<'_>, budget: &Budget) -> Result<(), StageError> {
    let assets = character.array("additionalAssets", budget)?.unwrap_or_default();
    for row in &rows[selection.start..] {
        if row.disabled(budget)? != Disabled::No { continue; }
        let data = js::units(row.data(budget)?, budget)?;
        // TS sends a role other than `user` as an assistant message, and CBS reads it as stored.
        let Some(Value::Text(role)) = row.fields().get("role", budget)? else { return Err(unsupported("message.role")) };
        check_inlays(&inlays(&data), role)?;
        check_asset_prompts(&asset_prompts(&data), assets)?;
    }
    Ok(())
}

/// `promptSettings?.maxThoughtTagDepth ?? -1` as the depth check computes with it: `depth === -1 || depth - count <=
/// index` reads any other value as `Number(depth)`, and one equal to -1 keeps every thought as well.
pub(crate) fn max_thought_depth(prompt_settings: Fields<'_>, budget: &Budget) -> Result<f64, StageError> {
    match prompt_settings.get("maxThoughtTagDepth", budget)? {
        None | Some(Value::Null) => Ok(-1.0),
        depth => js::number(depth, "promptSettings.maxThoughtTagDepth", budget),
    }
}

fn remove(text: &[u16], ranges: impl Iterator<Item = (usize, usize)>) -> js::S {
    let mut output = Vec::with_capacity(text.len());
    let mut last = 0;
    for (start, end) in ranges {
        output.extend_from_slice(&text[last..start]);
        last = end;
    }
    output.extend_from_slice(&text[last..]);
    output
}

/// The prompt history loop: each selected message through CBS, `editprocess`, the name template, thoughts and asset
/// prompts, counted as it is added to `chats`. Returns the tokens it adds.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn messages(ctx: &mut Ctx, prompt: &mut Prompt, chats: &mut Vec<usize>, rows: &[Message<'_>], selection: &Selection,
    character: Character<'_>, db: &Settings<'_>, template: &Template, prompt_settings: Fields<'_>, max_thought_depth: f64)
    -> Result<f64, StageError> {
    let budget = ctx.budget();
    let send_name = template.using && prompt_settings.truthy("sendName", &budget)?;
    let assets = character.array("additionalAssets", &budget)?.unwrap_or_default();
    let mut tokens = 0.0;
    let mut relative = 0usize;
    for row in &rows[selection.start..] {
        if row.disabled(&budget)? != Disabled::No { continue; }
        let index = relative;
        relative += 1;
        let budget = ctx.budget();
        let role = row.role(&budget)?;
        let data = js::units(row.data(&budget)?, &budget)?;
        let parsed = ctx.parse(&data, TextContext { role: Some(role.copy(&budget)?), ..TextContext::default() })?;
        let request = ScriptRequest { mode: ScriptMode::EditProcess, input: js::text(&parsed, &budget)?, chat_index: index as i64,
            conditions: CbsConditions { first_message: None, chat_role: Some(role.copy(&budget)?) } };
        let mut content = ctx.script(request).await?;
        let budget = ctx.budget();
        let found = inlays(&content);
        check_inlays(&found, role)?;
        content = remove(&content, found.iter().map(|(start, end, _)| (*start, *end)));
        let mut entry_role = if role.equals_ascii(b"user") { Role::User } else { Role::Assistant };
        if send_name {
            // `messageNameTemplate || default`; CBS throws on a truthy non-string.
            let form = match db.get("messageNameTemplate", &budget)? {
                Some(Value::Text(form)) if form.len_units() != 0 => js::units(form, &budget)?,
                Some(form) if form.truthy() => return Err(invalid("settings.messageNameTemplate")),
                _ => js::s("<{{char}}'s Message>\n{{slot}}\n</{{char}}'s Message>"),
            };
            let form = ctx.parse_plain(&form)?;
            content = js::replace(&form, &js::s("{{slot}}"), &content, false, &budget)?;
            let named = db.get("namedMessageRole", &budget)?;
            entry_role = [("user", Role::User), ("assistant", Role::Assistant), ("system", Role::System)].into_iter()
                .find(|(name, _)| is_text(named, name)).map_or(Role::Assistant, |(_, role)| role);
        }
        let (rest, thought) = take_thoughts(&content);
        content = rest;
        let keep = max_thought_depth == -1.0 || max_thought_depth - selection.message_count as f64 <= index as f64;
        let thoughts = match thought { Some(thought) if keep => vec![js::text(&thought, &budget)?], _ => Vec::new() };
        let prompts = asset_prompts(&content);
        check_asset_prompts(&prompts, assets)?;
        content = remove(&content, prompts.iter().map(|(start, end, _)| (*start, *end)));
        let mut entry = message(entry_role, &content, &budget)?;
        entry.memo = Some(row.id(&budget)?.ok_or_else(|| invalid("message.chatId"))?.copy(&budget)?);
        entry.attr = Some(Vec::new());
        entry.thoughts = Some(thoughts);
        tokens += ctx.count(&entry).await?;
        chats.push(prompt.add(entry));
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(text: &str) -> (Vec<(usize, usize, bool)>, Vec<(usize, usize, String)>) {
        let units = js::s(text);
        (inlays(&units), asset_prompts(&units).into_iter().map(|(start, end, name)| (start, end, String::from_utf16(&name).unwrap())).collect())
    }

    #[test]
    fn inlay_and_asset_markers_follow_their_patterns() {
        assert_eq!(view("a{{inlay::x}}{{inlayeddata::y}}{{inlayed::\n}}").0, vec![(1, 13, false), (13, 31, true)]);
        assert_eq!(view("{{ASSET_PROMPT::a\nb}}{{aſſetprompt::c}}{{asset_prompt::}}").1,
            vec![(0, 21, "a\nb".into()), (21, 39, "c".into())]);
        assert_eq!(view("{{asset_prompt::}}}").1, vec![(0, 19, "}".into())]);
    }
}
