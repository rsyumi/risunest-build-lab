//! `formatingOrder` and `promptTemplate` item assembly.

use crate::placement::Placement;
use crate::machine::{clone_message, invalid, js, message, role_name, unsupported, Ctx, Model, Prompt};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::chat_ml::{parse_chat_ml, ChatMlPart};
use risunest_ir::stage::{StageError, TextContext};
use risunest_ir::{Budget, Text, Value};
use risunest_records::character::Character;
use risunest_records::root::Fields;
use risunest_records::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind { Persona, Description, AuthorNote, Lorebook, PostEverything, Plain, Jailbreak, Cot, ChatMl, Chat, Memory, Cache, Other }

/// `rangeStart` or `rangeEnd` as the comparisons and `slice` read it: `Number(None)` is `undefined`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Bound { Number(Option<f64>), End }

/// One `promptTemplate` card with the fields its type reads. A field TS reads as a string is `None` when it is not one,
/// so the card fails only where TS would read it.
#[derive(Debug)]
pub(crate) struct Card {
    kind: Kind,
    role: Option<js::S>,
    /// `role2`: `None` when falsy, `Err` for a name `convertPromptRole` lacks.
    role2: Option<Result<Role, ()>>,
    /// `innerFormat` when truthy; `Err` when it is not a string.
    inner: Option<Result<js::S, ()>>,
    text: Option<js::S>,
    type2: Option<js::S>,
    default_text: Option<Result<js::S, ()>>,
    range_start: Bound,
    range_end: Bound,
    original_on_system: bool,
    /// `depth` of a cache card; NaN for a value that never counts down to zero.
    depth: f64,
}

impl Card {
    fn new(kind: Kind) -> Self {
        Self {
            kind, role: None, role2: None, inner: None, text: None, type2: None, default_text: None,
            range_start: Bound::Number(None), range_end: Bound::Number(None), original_on_system: false, depth: f64::NAN,
        }
    }

    fn read(value: &Value, budget: &Budget) -> Result<Self, StageError> {
        // `card.type` on `null` throws.
        if matches!(value, Value::Null | Value::Undefined) { return Err(invalid("promptTemplate")); }
        let Value::Object(_) = value else { return Ok(Self::new(Kind::Other)) };
        let fields = Fields::new(value, "promptTemplate")?;
        let text = |key: &str| -> Result<Option<js::S>, StageError> {
            match fields.get(key, budget)? { Some(Value::Text(text)) => Ok(Some(js::units(text, budget)?)), _ => Ok(None) }
        };
        let kind = match text("type")? {
            None => Kind::Other,
            Some(kind) => {
                const KINDS: [(&str, Kind); 12] = [("persona", Kind::Persona), ("description", Kind::Description),
                    ("authornote", Kind::AuthorNote), ("lorebook", Kind::Lorebook), ("postEverything", Kind::PostEverything),
                    ("plain", Kind::Plain), ("jailbreak", Kind::Jailbreak), ("cot", Kind::Cot), ("chatML", Kind::ChatMl),
                    ("chat", Kind::Chat), ("memory", Kind::Memory), ("cache", Kind::Cache)];
                KINDS.iter().find(|(name, _)| js::equals(&kind, name)).map_or(Kind::Other, |(_, kind)| *kind)
            }
        };
        let mut card = Self::new(kind);
        card.role = text("role")?;
        card.role2 = match fields.get("role2", budget)? {
            Some(value) if value.truthy() => Some(match value {
                Value::Text(role) if role.equals_ascii(b"user") => Ok(Role::User),
                Value::Text(role) if role.equals_ascii(b"bot") => Ok(Role::Assistant),
                Value::Text(role) if role.equals_ascii(b"system") => Ok(Role::System),
                _ => Err(()),
            }),
            _ => None,
        };
        card.inner = match fields.get("innerFormat", budget)? {
            Some(Value::Text(text)) if text.len_units() != 0 => Some(Ok(js::units(text, budget)?)),
            Some(value) if value.truthy() => Some(Err(())),
            _ => None,
        };
        card.text = text("text")?;
        card.type2 = text("type2")?;
        card.default_text = match fields.get("defaultText", budget)? {
            None | Some(Value::Null) => None,
            Some(Value::Text(text)) => Some(Ok(js::units(text, budget)?)),
            Some(_) => Some(Err(())),
        };
        let bound = |key: &str| -> Result<Bound, StageError> {
            Ok(match fields.get(key, budget)? {
                None => Bound::Number(None),
                Some(Value::Null | Value::Bool(false)) => Bound::Number(Some(0.0)),
                Some(Value::Bool(true)) => Bound::Number(Some(1.0)),
                Some(Value::Number(value)) => Bound::Number(Some(*value)),
                Some(Value::Text(text)) if key == "rangeEnd" && text.equals_ascii(b"end") => Bound::End,
                // JS compares a string bound as a string.
                Some(_) => return Err(unsupported("promptTemplate.range")),
            })
        };
        card.range_start = bound("rangeStart")?;
        card.range_end = bound("rangeEnd")?;
        card.original_on_system = fields.truthy("chatAsOriginalOnSystem", budget)?;
        // `depthRemaining === 0` before `depthRemaining--`: `null` and `false` step down from -1 and never reach zero.
        card.depth = match fields.get("depth", budget)? {
            Some(Value::Number(value)) => *value,
            Some(Value::Bool(true)) => 1.0,
            Some(Value::Text(_)) => return Err(unsupported("promptTemplate.depth")),
            _ => f64::NAN,
        };
        Ok(card)
    }

    fn plain(type2: &str) -> Self {
        let mut card = Self::new(Kind::Plain);
        card.text = Some(Vec::new());
        card.role = Some(js::s("system"));
        card.type2 = Some(js::s(type2));
        card
    }

    // `convertPromptRole[card.role]`.
    fn prompt_role(&self) -> Result<Role, StageError> {
        match self.role.as_deref() {
            Some(role) if js::equals(role, "system") => Ok(Role::System),
            Some(role) if js::equals(role, "user") => Ok(Role::User),
            Some(role) if js::equals(role, "bot") => Ok(Role::Assistant),
            _ => Err(invalid("promptTemplate.role")),
        }
    }
}

/// The template a send uses: `usingPromptTemplate`, and the cards with the postEverything card added and the utility
/// bot template applied; `None` for the `formatingOrder` path.
#[derive(Debug)]
pub(crate) struct Template { pub(crate) using: bool, pub(crate) cards: Option<Vec<Card>> }

impl Template {
    pub(crate) fn read(db: &Settings<'_>, character: Character<'_>, prompt_settings: Fields<'_>, budget: &Budget) -> Result<Self, StageError> {
        let mut cards = match db.get("promptTemplate", budget)? {
            Some(Value::Array(values)) => {
                let mut cards = Vec::with_capacity(values.as_slice().len() + 1);
                for value in values.as_slice() { budget.work(1)?; cards.push(Card::read(value, budget)?); }
                if !cards.iter().any(|card| card.kind == Kind::PostEverything) { cards.push(Card::new(Kind::PostEverything)); }
                Some(cards)
            }
            Some(value) if value.truthy() => return Err(invalid("settings.promptTemplate")),
            _ => None,
        };
        let using = cards.is_some();
        if character.truthy("utilityBot", budget)? && !(using && prompt_settings.truthy("utilOverride", budget)?) {
            let mut chat = Card::new(Kind::Chat);
            chat.range_start = Bound::Number(Some(0.0));
            chat.range_end = Bound::End;
            cards = Some(vec![Card::plain("main"), Card::new(Kind::Description), Card::new(Kind::Lorebook), chat,
                Card::plain("globalNote"), Card::new(Kind::PostEverything)]);
        }
        Ok(Self { using, cards })
    }

    /// `prebuiltAssetCommand` is appended to the global note cards both template passes read.
    pub(crate) fn admit(&self, db: &Settings<'_>, character: Character<'_>, budget: &Budget) -> Result<(), StageError> {
        if !character.truthy("prebuiltAssetCommand", budget)? { return Ok(()); }
        for card in self.cards.iter().flatten() {
            if !matches!(card.kind, Kind::Plain | Kind::Jailbreak | Kind::Cot)
                || !card.type2.as_deref().is_some_and(|type2| js::equals(type2, "globalNote")) { continue; }
            if card.kind == Kind::Jailbreak && !db.truthy("jailbreakToggle", budget)? { continue; }
            if card.kind == Kind::Cot && !db.truthy("chainOfThought", budget)? { continue; }
            let marked = card.text.as_deref().is_some_and(|text| js::find(text, &js::s("{{//@customimageinstruction}}"), 0).is_some());
            if !marked { return Err(unsupported("prebuiltAssetCommand")); }
        }
        Ok(())
    }
}

/// Flags the first template pass sets for the second.
#[derive(Debug, Default)]
pub(crate) struct CardState { pub(crate) memory_card_used: bool, pub(crate) has_cache_point: bool }

/// The `}}` that ends a lazy `(.+?)}}` group starting at `start`: at least one unit, and with `dot_all` false no line
/// terminator inside.
pub(crate) fn closing(text: &[u16], start: usize, dot_all: bool) -> Option<usize> {
    let mut index = start;
    while index < text.len() {
        if index > start && text[index..].starts_with(&[0x7d, 0x7d]) { return Some(index); }
        if !dot_all && js::is_line_terminator(text[index]) { return None; }
        index += 1;
    }
    None
}

// `applyPromptBlockRole(chats, card.role2)`.
fn block_role(messages: &mut [PromptMessage], role2: &Option<Result<Role, ()>>) -> Result<(), StageError> {
    match role2 {
        None => Ok(()),
        Some(Err(())) if messages.is_empty() => Ok(()),
        Some(Err(())) => Err(invalid("promptTemplate.role2")),
        Some(Ok(role)) => { for message in messages { message.role = *role; } Ok(()) }
    }
}

// The persona, description, author note and memory cards: copies of the prepared messages, wrapped in `innerFormat`.
fn block(ctx: &mut Ctx, lore: &Placement, mut messages: Vec<PromptMessage>, card: &Card, positioned: bool) -> Result<Vec<PromptMessage>, StageError> {
    let budget = ctx.budget();
    if card.kind != Kind::Description { block_role(&mut messages, &card.role2)?; }
    let Some(inner) = &card.inner else { return Ok(messages) };
    if messages.is_empty() { return Ok(messages); }
    let inner = inner.as_ref().map_err(|()| invalid("promptTemplate.innerFormat"))?;
    for message in &mut messages {
        let location = match card.kind { Kind::Persona => "persona", Kind::Description => "description", Kind::AuthorNote => "authornote", _ => "" };
        let format = if positioned { lore.position(inner, Some(&js::s(location)), &budget)? } else { inner.clone() };
        let format = ctx.parse_plain(&format)?;
        let mut content = js::units(&message.content, &budget)?;
        if card.kind == Kind::AuthorNote && content.is_empty() {
            content = match &card.default_text {
                Some(Ok(text)) => text.clone(),
                Some(Err(())) => return Err(invalid("promptTemplate.defaultText")),
                None => Vec::new(),
            };
        }
        message.content = js::text(&js::replace(&format, &js::s("{{slot}}"), &content, false, &budget)?, &budget)?;
    }
    Ok(messages)
}

fn copies(prompt: &Prompt, indices: &[usize], budget: &Budget) -> Result<Vec<PromptMessage>, StageError> {
    indices.iter().map(|index| clone_message(&prompt.arena[*index], budget)).collect()
}

// `getDescriptionPrompts(card.role2)`: only the base description takes the block role.
fn description(prompt: &Prompt, base: usize, card: &Card, budget: &Budget) -> Result<Vec<PromptMessage>, StageError> {
    let mut messages = copies(prompt, &prompt.groups.description, budget)?;
    if let Some(at) = prompt.groups.description.iter().position(|index| *index == base) {
        block_role(&mut messages[at..at + 1], &card.role2)?;
    }
    Ok(messages)
}

// The plain, jailbreak and cot cards; `None` for a card the settings skip.
fn plain(ctx: &mut Ctx, lore: &Placement, card: &Card, db: &Settings<'_>, character: Character<'_>) -> Result<Option<PromptMessage>, StageError> {
    let budget = ctx.budget();
    if card.kind == Kind::Jailbreak && !db.truthy("jailbreakToggle", &budget)? { return Ok(None); }
    if card.kind == Kind::Cot && !db.truthy("chainOfThought", &budget)? { return Ok(None); }
    let text = card.text.as_deref().ok_or_else(|| invalid("promptTemplate.text"))?;
    let location = match card.kind {
        Kind::Jailbreak => Some(js::s("jailbreak")), Kind::Cot => Some(js::s("cot")), _ => card.type2.clone(),
    };
    let mut content = lore.position(text, location.as_deref(), &budget)?;
    if card.type2.as_deref().is_some_and(|type2| js::equals(type2, "globalNote")) {
        if let Some(replace) = character.text("replaceGlobalNote", &budget)?.filter(|text| text.len_units() != 0) {
            let replace = lore.position(&js::units(replace, &budget)?, location.as_deref(), &budget)?;
            content = js::replace(&replace, &js::s("{{original}}"), &content, true, &budget)?;
        }
        if character.truthy("prebuiltAssetCommand", &budget)? && js::find(text, &js::s("{{//@customimageinstruction}}"), 0).is_none() {
            return Err(unsupported("prebuiltAssetCommand"));
        }
    }
    let role = card.role.as_deref().map(|role| js::text(role, &budget)).transpose()?;
    let content = ctx.parse(&content, TextContext { role, ..TextContext::default() })?;
    Ok(Some(message(card.prompt_role()?, &content, &budget)?))
}

fn chat_ml(ctx: &mut Ctx, card: &Card) -> Result<Vec<PromptMessage>, StageError> {
    let budget = ctx.budget();
    let text = card.text.as_deref().ok_or_else(|| invalid("promptTemplate.text"))?;
    // A text without the starter makes `parseChatML` return `null`, which TS then iterates.
    let parts = parse_chat_ml(text, &budget)?.ok_or_else(|| invalid("promptTemplate.chatML"))?;
    let mut messages = Vec::with_capacity(parts.len());
    for ChatMlPart { role, content, thoughts } in parts {
        let content = ctx.parse_plain(&content)?;
        let mut entry = message(role, &content, &budget)?;
        entry.thoughts = Some(thoughts.iter().map(|thought| js::text(thought, &budget)).collect::<Result<_, _>>()?);
        messages.push(entry);
    }
    Ok(messages)
}

fn post_end(using: bool, prompt_settings: Fields<'_>, budget: &Budget) -> Result<Option<PromptMessage>, StageError> {
    if !using { return Ok(None); }
    match prompt_settings.get("postEndInnerFormat", budget)? {
        Some(Value::Text(text)) if text.len_units() != 0 => Ok(Some(PromptMessage::new(Role::System, text.copy(budget)?))),
        Some(value) if value.truthy() => Err(invalid("promptSettings.postEndInnerFormat")),
        _ => Ok(None),
    }
}

/// The first template pass, or the `unformated` count without a template: the tokens of every prepared item.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn count_items(ctx: &mut Ctx, prompt: &Prompt, lore: &Placement, template: &Template, base: usize, db: &Settings<'_>,
    character: Character<'_>, prompt_settings: Fields<'_>, state: &mut CardState) -> Result<f64, StageError> {
    let budget = ctx.budget();
    let mut tokens = 0.0;
    let Some(cards) = &template.cards else {
        for group in prompt.groups.in_order() {
            for index in group { tokens += ctx.count(&prompt.arena[*index]).await?; }
        }
        return Ok(tokens);
    };
    for card in cards {
        budget.work(1)?;
        let messages = match card.kind {
            Kind::Persona => block(ctx, lore, copies(prompt, &prompt.groups.persona_prompt, &budget)?, card, true)?,
            Kind::Description => block(ctx, lore, description(prompt, base, card, &budget)?, card, true)?,
            Kind::AuthorNote => block(ctx, lore, copies(prompt, &prompt.groups.author_note, &budget)?, card, true)?,
            Kind::Lorebook => copies(prompt, &prompt.groups.lorebook, &budget)?,
            Kind::PostEverything => {
                for index in &prompt.groups.post_everything { tokens += ctx.count(&prompt.arena[*index]).await?; }
                post_end(template.using, prompt_settings, &budget)?.into_iter().collect()
            }
            Kind::Plain | Kind::Jailbreak | Kind::Cot => plain(ctx, lore, card, db, character)?.into_iter().collect(),
            Kind::ChatMl => chat_ml(ctx, card)?,
            // `unformated.chats` is still empty in this pass.
            Kind::Chat | Kind::Other => Vec::new(),
            Kind::Memory => { state.memory_card_used = true; Vec::new() }
            Kind::Cache => { state.has_cache_point = true; Vec::new() }
        };
        for message in &messages { tokens += ctx.count(message).await?; }
    }
    Ok(tokens)
}

/// `pushPrompts(cha)`: blank entries are skipped, and for the models that merge them a system entry joins the previous
/// system entry with the same `memo` and `name`.
pub(crate) fn push_prompts(prompt: &mut Prompt, formated: &mut Vec<usize>, entries: &[usize], model: Model, budget: &Budget)
    -> Result<(), StageError> {
    for &index in entries {
        budget.work(1)?;
        let entry = &prompt.arena[index];
        if !crate::machine::sendable(entry) { continue; }
        if !model.merges_system || entry.role != Role::System {
            formated.push(index);
            continue;
        }
        let merge = formated.last().copied().filter(|last| {
            let previous = &prompt.arena[*last];
            previous.role == Role::System && same(&previous.memo, &entry.memo) && same(&previous.name, &entry.name)
        });
        match merge {
            // The same object can be pushed twice, so both sides are read before the write.
            Some(last) => {
                let joined = js::concat(&[&js::units(&prompt.arena[last].content, budget)?, &js::s("\n\n"),
                    &js::units(&prompt.arena[index].content, budget)?]);
                prompt.arena[last].content = js::text(&joined, budget)?;
            }
            None => formated.push(index),
        }
    }
    Ok(())
}

fn same(left: &Option<Text>, right: &Option<Text>) -> bool {
    match (left, right) { (None, None) => true, (Some(left), Some(right)) => left.equals(right), _ => false }
}

// `systemizeChat(chats)`, in place on the shared entries.
fn systemize(prompt: &mut Prompt, entries: &[usize], budget: &Budget) -> Result<(), StageError> {
    for &index in entries {
        let entry = &mut prompt.arena[index];
        if !matches!(entry.role, Role::User | Role::Assistant) { continue; }
        let content = js::units(&entry.content, budget)?;
        let name = entry.name.as_ref().map(|name| js::units(name, budget)).transpose()?;
        let named = entry.attr.as_ref().is_some_and(|attr| attr.iter().any(|value| value.equals_ascii(b"nameAdded")));
        let prefix = match name {
            Some(name) if name.starts_with(&js::s("example_")) => Some(name),
            _ if !named => Some(js::s(role_name(entry.role))),
            _ => None,
        };
        if let Some(prefix) = prefix { entry.content = js::text(&js::concat(&[&prefix, &js::s(": "), &content]), budget)?; }
        entry.role = Role::System;
        entry.memo = None;
        entry.name = None;
    }
    Ok(())
}

// Sets `cachePoint` on up to `depth` matching entries, walking back from the end.
fn mark_cache(prompt: &mut Prompt, formated: &[usize], depth: f64, matches: impl Fn(Role) -> bool) {
    let mut remaining = depth;
    for &index in formated.iter().rev() {
        if remaining == 0.0 { break; }
        if matches(prompt.arena[index].role) {
            prompt.arena[index].cache_point = Some(true);
            remaining -= 1.0;
        }
    }
}

// The chat card's `unformated.chats.slice(start, end)`, or `None` when `start >= end`.
fn chat_range(card: &Card, length: usize) -> Option<(usize, usize)> {
    let total = length as f64;
    let Bound::Number(mut start) = card.range_start else { return None };
    let mut end = match card.range_end { Bound::End => Some(total), Bound::Number(end) => end };
    if start == Some(-1000.0) {
        start = Some(0.0);
        end = Some(total);
    }
    if let Some(value) = start.filter(|value| *value < 0.0) { start = Some((total + value).max(0.0)); }
    if let Some(value) = end.filter(|value| *value < 0.0) { end = Some((total + value).max(0.0)); }
    if let (Some(start), Some(end)) = (start, end) { if start >= end { return None; } }
    let relative = |value: Option<f64>, absent: usize| -> usize {
        let Some(value) = value else { return absent };
        let value = value.trunc();
        if value < 0.0 { (total + value).max(0.0) as usize } else { value.min(total) as usize }
    };
    let (start, end) = (relative(start, 0), relative(end, length));
    Some((start, end.max(start)))
}

/// The second template pass, or the `formatingOrder` pass: the prompt array before its final trim.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn assemble(ctx: &mut Ctx, prompt: &mut Prompt, lore: &Placement, template: &Template, base: usize, memories: &[usize],
    db: &Settings<'_>, character: Character<'_>, prompt_settings: Fields<'_>, model: Model, state: &CardState)
    -> Result<Vec<usize>, StageError> {
    let budget = ctx.budget();
    let mut formated = Vec::new();
    let Some(cards) = &template.cards else {
        let Some(Value::Array(order)) = db.get("formatingOrder", &budget)? else { return Err(invalid("settings.formatingOrder")) };
        let mut keys = Vec::with_capacity(order.as_slice().len() + 1);
        for key in order.as_slice() {
            // `unformated[key]` for a key it lacks is not iterable.
            let Value::Text(key) = key else { return Err(invalid("settings.formatingOrder")) };
            keys.push(js::units(key, &budget)?);
        }
        keys.push(js::s("postEverything"));
        for key in keys {
            let entries = prompt.groups.by_key(&key).ok_or_else(|| invalid("settings.formatingOrder"))?.clone();
            push_prompts(prompt, &mut formated, &entries, model, &budget)?;
        }
        return Ok(formated);
    };
    let send_chat_as_system = template.using && prompt_settings.truthy("sendChatAsSystem", &budget)?;
    let automatic_cache = db.truthy("automaticCachePoint", &budget)?;
    for card in cards {
        budget.work(1)?;
        let added = match card.kind {
            Kind::Persona => block(ctx, lore, copies(prompt, &prompt.groups.persona_prompt, &budget)?, card, true)?,
            Kind::Description => block(ctx, lore, description(prompt, base, card, &budget)?, card, true)?,
            Kind::AuthorNote => block(ctx, lore, copies(prompt, &prompt.groups.author_note, &budget)?, card, true)?,
            Kind::Memory => block(ctx, lore, copies(prompt, memories, &budget)?, card, false)?,
            Kind::Lorebook => {
                let entries = prompt.groups.lorebook.clone();
                push_prompts(prompt, &mut formated, &entries, model, &budget)?;
                continue;
            }
            Kind::PostEverything => {
                let entries = prompt.groups.post_everything.clone();
                push_prompts(prompt, &mut formated, &entries, model, &budget)?;
                post_end(template.using, prompt_settings, &budget)?.into_iter().collect()
            }
            Kind::Plain | Kind::Jailbreak | Kind::Cot => plain(ctx, lore, card, db, character)?.into_iter().collect(),
            Kind::ChatMl => chat_ml(ctx, card)?,
            Kind::Chat => {
                let Some((start, end)) = chat_range(card, prompt.groups.chats.len()) else { continue };
                let entries = prompt.groups.chats[start..end].to_vec();
                if send_chat_as_system && !card.original_on_system { systemize(prompt, &entries, &budget)?; }
                push_prompts(prompt, &mut formated, &entries, model, &budget)?;
                if automatic_cache && !state.has_cache_point { mark_cache(prompt, &formated, 3.0, |role| role == Role::User); }
                continue;
            }
            Kind::Cache => {
                let role = card.role.as_deref();
                mark_cache(prompt, &formated, card.depth, |entry| {
                    role.is_some_and(|role| js::equals(role, "all") || js::equals(role, role_name(entry)))
                });
                continue;
            }
            Kind::Other => continue,
        };
        let entries: Vec<usize> = added.into_iter().map(|entry| prompt.add(entry)).collect();
        push_prompts(prompt, &mut formated, &entries, model, &budget)?;
    }
    Ok(formated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(units: &[u16]) -> String { String::from_utf16_lossy(units) }

    #[test]
    fn position_markers_resolve_to_nothing_across_rounds() {
        let budget = crate::machine::tests::budget();
        let resolve = |input: &str| text(&Placement::new(Vec::new()).resolve(&js::s(input), &budget).unwrap());
        assert_eq!(resolve("a{{position::x}}b"), "ab");
        // A removal can join a new marker; a marker with a line break inside stays.
        assert_eq!(resolve("{{posi{{position::a}}tion::b}}c"), "c");
        assert_eq!(resolve("{{position::}}}x"), "x");
        assert_eq!(resolve("{{position::a\nb}}"), "{{position::a\nb}}");
    }

    fn fragment(position: &str, content: &str, budget: &Budget) -> risunest_ir::stage::LoreFragment {
        let text = |value| Text::from_str(value, budget).unwrap();
        risunest_ir::stage::LoreFragment { position: text(position), prompt: text(content), role: Role::System,
            depth: 0.0, order: 0.0, priority: 0.0, tokens: 0, source: text("synthetic"), inject: None }
    }

    #[test]
    fn named_positions_join_literal_sources_and_stop_after_five_rounds() {
        let budget = crate::machine::tests::budget();
        let lore = Placement::new(vec![
            fragment("pt_name", "$&", &budget), fragment("pt_name", "{{position::tail}}", &budget),
            fragment("pt_tail", "end", &budget), fragment("pt_cycle", "C{{position::cycle}}", &budget),
            fragment("pt_one", "{{position::two}}", &budget), fragment("pt_two", "{{position::three}}", &budget),
            fragment("pt_three", "{{position::four}}", &budget), fragment("pt_four", "{{position::five}}", &budget),
            fragment("pt_five", "5{{position::six}}", &budget), fragment("pt_six", "6", &budget),
        ]);
        let resolve = |input: &str| text(&lore.resolve(&js::s(input), &budget).unwrap());
        assert_eq!(resolve("{{position::name}}/{{position::missing}}"), "$&\nend/");
        assert_eq!(resolve("{{position::cycle}}"), "CCCCC");
        assert_eq!(resolve("{{position::one}}"), "5");
        assert_eq!(resolve("{{position::tail\n}}"), "{{position::tail\n}}");
    }

    #[test]
    fn injections_use_ordered_first_replacement_before_position_resolution() {
        use risunest_ir::stage::{InjectOperation, LoreInject};
        let budget = crate::machine::tests::budget();
        let inject = |operation, param, content, location, lore| {
            let mut entry = fragment("", content, &budget);
            entry.inject = Some(LoreInject { operation, location: Text::from_str(location, &budget).unwrap(),
                param: Text::from_str(param, &budget).unwrap(), lore });
            entry
        };
        let lore = Placement::new(vec![
            inject(InjectOperation::Replace, "x", "[$$][$&][$`][$']", "main", false),
            inject(InjectOperation::Prepend, "", "P", "main", false),
            inject(InjectOperation::Append, "", "{{position::tail}}", "main", false),
            inject(InjectOperation::Append, "", "wrong site", "persona", false),
            inject(InjectOperation::Append, "", "already applied", "main", true),
            fragment("pt_tail", "T", &budget),
        ]);
        assert_eq!(text(&lore.position(&js::s("x-x"), Some(&js::s("main")), &budget).unwrap()), "P [$][x][][-x]-x T");
        assert_eq!(text(&lore.position(&js::s("x-x"), None, &budget).unwrap()), "x-x");
        let empty = Placement::new(vec![inject(InjectOperation::Replace, "", "<$&$`$'>", "main", false)]);
        assert_eq!(text(&empty.position(&js::s("x-x"), Some(&js::s("main")), &budget).unwrap()), "<x-x>x-x");
    }

    #[test]
    fn a_nonempty_memory_block_skips_injections_and_named_positions() {
        use risunest_cbs_experimental::cbs::processor::{Processor, Snapshot};
        use risunest_ir::stage::{InjectOperation, LoreInject};
        let (mut ctx, budget) = crate::machine::tests::text_context();
        ctx.stages.text = Box::new(Processor::new(Snapshot::default(), risunest_ir::rng::Tape::new(Vec::new())));
        let mut injection = fragment("", "must not appear", &budget);
        injection.inject = Some(LoreInject { operation: InjectOperation::Append, location: Text::from_str("memory", &budget).unwrap(),
            param: Text::from_str("", &budget).unwrap(), lore: false });
        let lore = Placement::new(vec![injection, fragment("pt_named", "must not resolve", &budget)]);
        let mut card = Card::new(Kind::Memory);
        card.inner = Some(Ok(js::s("Memory {{slot}} {{position::named}}")));
        let messages = block(&mut ctx, &lore, vec![message(Role::System, &js::s("summary."), &budget).unwrap()], &card, false).unwrap();
        assert_eq!(text(&js::units(&messages[0].content, &budget).unwrap()), "Memory summary. {{position::named}}");
    }

    #[test]
    fn chat_ranges_follow_the_js_comparisons() {
        let card = |start: Option<f64>, end: Bound| {
            let mut card = Card::new(Kind::Chat);
            card.range_start = Bound::Number(start);
            card.range_end = end;
            card
        };
        assert_eq!(chat_range(&card(Some(0.0), Bound::End), 5), Some((0, 5)));
        assert_eq!(chat_range(&card(Some(-1000.0), Bound::Number(Some(1.0))), 5), Some((0, 5)));
        assert_eq!(chat_range(&card(Some(-2.0), Bound::End), 5), Some((3, 5)));
        assert_eq!(chat_range(&card(Some(-9.0), Bound::Number(Some(-1.0))), 5), Some((0, 4)));
        assert_eq!(chat_range(&card(Some(3.0), Bound::Number(Some(3.0))), 5), None);
        assert_eq!(chat_range(&card(None, Bound::Number(None)), 5), Some((0, 5)));
        assert_eq!(chat_range(&card(Some(1.0), Bound::Number(Some(9.0))), 5), Some((1, 5)));
    }
}
