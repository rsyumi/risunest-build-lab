//! How the stages' effects change the stored conversation, as TS stores them.

use crate::modules::strict_equals;
use crate::root::{copy, nullish};
use risunest_ir::chat::MessageRole;
use risunest_ir::stage::{Effect, MessageEdit, StageError};
use risunest_ir::{Budget, OwnedVec, Text, Value};
use std::borrow::Cow;

fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }

/// `'user'` or `'char'`.
pub fn role_text(role: MessageRole, budget: &Budget) -> Result<Text, StageError> {
    Ok(Text::from_str(match role { MessageRole::User => "user", MessageRole::Char => "char" }, budget)?)
}

fn define(target: &mut Value, key: &str, value: Value, budget: &Budget) -> Result<(), StageError> {
    Ok(target.define(Text::from_str(key, budget)?, value, budget)?)
}

fn property_mut<'a>(value: &'a mut Value, key: &str, budget: &Budget) -> Result<Option<&'a mut Value>, StageError> {
    budget.work(1)?;
    let Value::Object(properties) = value else { return Ok(None) };
    for (name, value) in properties.as_mut_slice() {
        budget.work(name.len_units().min(key.len()).saturating_add(1))?;
        if name.equals_ascii(key.as_bytes()) { return Ok(Some(value)); }
    }
    Ok(None)
}

/// `Array.prototype.at(index)`: `ToIntegerOrInfinity`, counted from the end when negative.
pub fn at_index(index: f64, length: usize) -> Option<usize> {
    let relative = if index.is_nan() { 0.0 } else { index.trunc() };
    let absolute = if relative < 0.0 { length as f64 + relative } else { relative };
    (absolute >= 0.0 && absolute < length as f64).then_some(absolute as usize)
}

/// A relative `slice` or `splice` start, clamped into `0..=length`.
pub fn relative_index(index: f64, length: usize) -> usize {
    let relative = if index.is_nan() { 0.0 } else { index.trunc() };
    if relative < 0.0 { (length as f64 + relative).max(0.0) as usize } else { relative.min(length as f64) as usize }
}

/// `{ role, data, chatId }`, the message the Lua and trigger edits add.
fn edit_message(role: MessageRole, data: &Value, chat_id: &Text, budget: &Budget) -> Result<Value, StageError> {
    let mut message = Value::Object(OwnedVec::new(budget)?);
    define(&mut message, "role", Value::Text(role_text(role, budget)?), budget)?;
    define(&mut message, "data", copy(data, budget)?, budget)?;
    define(&mut message, "chatId", Value::Text(chat_id.copy(budget)?), budget)?;
    Ok(message)
}

/// Rebuilds the message array keeping the messages in `range` but `skip`, with `insert` going in at its position.
fn rebuild(messages: &mut OwnedVec<Value>, range: std::ops::Range<usize>, skip: Option<usize>, insert: Option<(usize, Value)>,
    budget: &Budget) -> Result<(), StageError> {
    let mut old = std::mem::replace(messages, OwnedVec::new(budget)?);
    let mut insert = insert;
    let length = old.as_slice().len();
    for (index, message) in old.as_mut_slice().iter_mut().enumerate() {
        budget.work(1)?;
        if insert.as_ref().is_some_and(|(at, _)| *at == index) { messages.push(insert.take().expect("insert").1)?; }
        if !range.contains(&index) || skip == Some(index) { continue; }
        messages.push(std::mem::replace(message, Value::Null))?;
    }
    if let Some((at, message)) = insert {
        debug_assert_eq!(at, length);
        messages.push(message)?;
    }
    Ok(())
}

/// Applies one edit to a conversation's stored `message` by the rule its [`MessageEdit`] variant names. `Replace`
/// assigns whatever was stored; every other edit needs an array (`Invalid("conversation.message")`), and a `SetData` or
/// `SetRole` that finds a message that is not an object is `Invalid("message")`, where TS throws assigning to it.
pub fn edit_messages(stored: &mut Value, edit: &MessageEdit, budget: &Budget) -> Result<(), StageError> {
    if let MessageEdit::Replace { messages: replacement } = edit {
        if !matches!(replacement, Value::Array(_)) { return Err(invalid("message-replace")); }
        *stored = copy(replacement, budget)?;
        return Ok(());
    }
    let Value::Array(messages) = stored else { return Err(invalid("conversation.message")) };
    match edit {
        MessageEdit::SetData { index, data } => {
            let Some(at) = at_index(*index, messages.as_slice().len()) else { return Ok(()) };
            let message = &mut messages.as_mut_slice()[at];
            if !matches!(message, Value::Object(_)) { return Err(invalid("message")); }
            define(message, "data", copy(data, budget)?, budget)
        }
        MessageEdit::SetRole { index, role } => {
            let Some(at) = at_index(*index, messages.as_slice().len()) else { return Ok(()) };
            let message = &mut messages.as_mut_slice()[at];
            if !matches!(message, Value::Object(_)) { return Err(invalid("message")); }
            define(message, "role", Value::Text(role_text(*role, budget)?), budget)
        }
        MessageEdit::Cut { start, end } => {
            let length = messages.as_slice().len();
            let from = relative_index(*start, length);
            let to = end.map_or(length, |end| relative_index(end, length));
            rebuild(messages, from..to.max(from), None, None, budget)
        }
        MessageEdit::Remove { index } => {
            let length = messages.as_slice().len();
            let at = relative_index(*index, length);
            rebuild(messages, 0..length, Some(at), None, budget)
        }
        MessageEdit::Append { role, data, chat_id } => {
            let message = edit_message(*role, data, chat_id, budget)?;
            Ok(messages.push(message)?)
        }
        MessageEdit::Insert { index, role, data, chat_id } => {
            let message = edit_message(*role, data, chat_id, budget)?;
            let length = messages.as_slice().len();
            let at = relative_index(*index, length);
            rebuild(messages, 0..length, None, Some((at, message)), budget)
        }
        _ => Err(unsupported("message-edit")),
    }
}

/// Applies one `ir` effect to the stored conversation as TS stores it. `setChatVar` writes `scriptstate['$' + name]`
/// (`setChatVarOnConversation`, `src/ts/parser/chatVar.svelte.ts:29-39`); a character field belongs to the character,
/// not the conversation, and changes nothing here.
pub fn apply_conversation_effect(conversation: &mut Value, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
    match effect {
        Effect::ChatVariable { name, value } => {
            if nullish(conversation.get(b"scriptstate", budget)?) {
                let empty = Value::Object(OwnedVec::new(budget)?);
                define(conversation, "scriptstate", empty, budget)?;
            }
            let state = property_mut(conversation, "scriptstate", budget)?.expect("scriptstate");
            if !matches!(state, Value::Object(_)) { return Err(invalid("conversation.scriptstate")); }
            let mut key = Text::from_str("$", budget)?;
            key.append(name)?;
            budget.work(state.object().expect("object").len())?;
            let same = state.object().expect("object").iter().find(|(stored, _)| stored.equals(&key))
                .is_some_and(|(_, stored)| strict_equals(stored, value));
            if same { return Ok(()); }
            Ok(state.define(key, copy(value, budget)?, budget)?)
        }
        Effect::ChatField { field, value } => {
            let value = copy(value, budget)?;
            Ok(conversation.define(field.copy(budget)?, value, budget)?)
        }
        Effect::Message(edit) => {
            let stored = property_mut(conversation, "message", budget)?.ok_or_else(|| invalid("conversation.message"))?;
            edit_messages(stored, edit, budget)
        }
        Effect::CharacterField { .. } => Ok(()),
        _ => Err(unsupported("effect")),
    }
}
