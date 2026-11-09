//! `parseChatML` (`src/ts/parser/chatML.ts`) up to its CBS pass, on UTF-16 units.

use crate::budget::{Budget, Error};
use crate::chat::Role;
use crate::text::Text;

/// One `parseChatML` entry before `risuChatParser` runs on its content.
#[derive(Debug, PartialEq, Eq)]
pub struct ChatMlPart {
    pub role: Role,
    pub content: Vec<u16>,
    pub thoughts: Vec<Vec<u16>>,
}

fn units(text: &str) -> Vec<u16> { text.encode_utf16().collect() }

fn find(units: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.len() > units.len() { return None; }
    (from..=units.len() - needle.len()).find(|index| units[*index..].starts_with(needle))
}

// `String.prototype.trim`.
fn trim(units: &[u16], budget: &Budget) -> Result<Vec<u16>, Error> {
    let text = Text::from_units(units, budget)?.trim(budget)?;
    budget.work(text.len_units())?;
    Ok(text.units().collect())
}

/// `text.replace(/<Thoughts>(.+)<\/Thoughts>/gms, ...)`: the greedy group runs from the first opening tag to the last
/// closing tag, so there is at most one match. Returns the text without it and the thought.
pub fn take_thoughts(text: &[u16]) -> (Vec<u16>, Option<Vec<u16>>) {
    let open = units("<Thoughts>");
    let close = units("</Thoughts>");
    let Some(at) = find(text, &open, 0) else { return (text.to_vec(), None) };
    let start = at + open.len();
    let end = (start + 1..=text.len().saturating_sub(close.len())).rev().find(|index| text[*index..].starts_with(&close));
    let Some(end) = end else { return (text.to_vec(), None) };
    ([&text[..at], &text[end + close.len()..]].concat(), Some(text[start..end].to_vec()))
}

/// `parseChatML(data)` up to its CBS pass: `None` when the trimmed text does not start with `<|im_start|>`.
pub fn parse_chat_ml(data: &[u16], budget: &Budget) -> Result<Option<Vec<ChatMlPart>>, Error> {
    let starter = units("<|im_start|>");
    let separator = units("<|im_sep|>");
    let ender = units("<|im_end|>");
    let trimmed = trim(data, budget)?;
    if !trimmed.starts_with(&starter) { return Ok(None); }
    let mut parts = Vec::new();
    let mut last = 0;
    while let Some(at) = find(&trimmed, &starter, last) {
        parts.push(&trimmed[last..at]);
        last = at + starter.len();
    }
    parts.push(&trimmed[last..]);
    const ROLES: [(&str, Role); 3] = [("user", Role::User), ("system", Role::System), ("assistant", Role::Assistant)];
    let mut output = Vec::new();
    for part in parts.into_iter().filter(|part| !part.is_empty()) {
        let mut role = Role::User;
        let mut value = part;
        if let Some((name, named)) = ROLES.iter().find(|(name, _)| value.starts_with(&[units(name), separator.clone()].concat())) {
            role = *named;
            value = &value[name.len() + separator.len()..];
        } else if let Some((name, named)) = ROLES.iter().find(|(name, _)| {
            value.starts_with(&units(name)) && matches!(value.get(name.len()), Some(0x20 | 0x0a))
        }) {
            role = *named;
            value = &value[name.len() + 1..];
        }
        let mut value = trim(value, budget)?;
        if value.ends_with(&ender) { value.truncate(value.len() - ender.len()); }
        let (content, thought) = take_thoughts(&value);
        output.push(ChatMlPart { role, content, thoughts: thought.into_iter().collect() });
    }
    Ok(Some(output))
}
