//! The conversation the response changes: how its effects apply, the messages it commits, and the reads it answers.

use super::ResponseEffect;
use crate::machine::{invalid, unsupported};
use risunest_ir::chat::MessageRole;
use risunest_ir::stage::{Effect, HostReply, MessageEdit, StageError};
use risunest_ir::{Budget, OwnedVec, Text, Value};
pub use risunest_records::edits::apply_conversation_effect;
pub(super) use risunest_records::edits::role_text;
use risunest_records::modules::strict_equals;
use risunest_records::root::{copy, nullish};

fn object(budget: &Budget) -> Result<Value, StageError> { Ok(Value::Object(OwnedVec::new(budget)?)) }

fn define(target: &mut Value, key: &str, value: Value, budget: &Budget) -> Result<(), StageError> {
    Ok(target.define(Text::from_str(key, budget)?, value, budget)?)
}

pub(super) fn property<'a>(value: &'a Value, key: &str, budget: &Budget) -> Result<Option<&'a Value>, StageError> {
    Ok(value.get(key.as_bytes(), budget)?)
}

pub(super) fn property_mut<'a>(value: &'a mut Value, key: &str, budget: &Budget) -> Result<Option<&'a mut Value>, StageError> {
    budget.work(1)?;
    let Value::Object(properties) = value else { return Ok(None) };
    for (name, value) in properties.as_mut_slice() {
        budget.work(name.len_units().min(key.len()).saturating_add(1))?;
        if name.equals_ascii(key.as_bytes()) { return Ok(Some(value)); }
    }
    Ok(None)
}

/// `chat.message`.
pub(super) fn messages<'a>(conversation: &'a Value, budget: &Budget) -> Result<&'a [Value], StageError> {
    match property(conversation, "message", budget)? {
        Some(Value::Array(messages)) => Ok(messages.as_slice()),
        _ => Err(invalid("conversation.message")),
    }
}

fn messages_mut<'a>(conversation: &'a mut Value, budget: &Budget) -> Result<&'a mut OwnedVec<Value>, StageError> {
    match property_mut(conversation, "message", budget)? {
        Some(Value::Array(messages)) => Ok(messages),
        _ => Err(invalid("conversation.message")),
    }
}

/// A message's `chatId` when it is a string; any other value reads as no id.
pub(super) fn message_id<'a>(message: &'a Value, budget: &Budget) -> Result<Option<&'a Text>, StageError> {
    Ok(match property(message, "chatId", budget)? { Some(Value::Text(id)) => Some(id), _ => None })
}

/// Applies one response effect to the stored conversation, in the order the stage returned them. An emotion changes
/// the `CharEmotion` store, not the conversation.
pub fn apply_effect(conversation: &mut Value, effect: &ResponseEffect, budget: &Budget) -> Result<(), StageError> {
    match effect {
        ResponseEffect::Conversation(effect) => apply_conversation_effect(conversation, effect, budget),
        ResponseEffect::AppendMessage(message) => {
            let message = copy(message, budget)?;
            Ok(messages_mut(conversation, budget)?.push(message)?)
        }
        ResponseEffect::SetMessage { index, message } => {
            let message = copy(message, budget)?;
            let messages = messages_mut(conversation, budget)?;
            let slot = messages.as_mut_slice().get_mut(*index).ok_or_else(|| invalid("message-index"))?;
            *slot = message;
            Ok(())
        }
        ResponseEffect::Emotion(_) => Ok(()),
    }
}

/// What an output commit writes besides its data and role.
pub(super) struct Commit<'a> {
    pub saying: &'a Text,
    pub time: f64,
    pub generation_info: &'a Value,
    pub prompt_info: &'a Value,
    pub chat_id: &'a Text,
}

impl Commit<'_> {
    fn define_rest(&self, message: &mut Value, budget: &Budget) -> Result<(), StageError> {
        define(message, "saying", Value::Text(self.saying.copy(budget)?), budget)?;
        define(message, "time", Value::Number(self.time), budget)?;
        define(message, "generationInfo", copy(self.generation_info, budget)?, budget)?;
        define(message, "promptInfo", copy(self.prompt_info, budget)?, budget)?;
        define(message, "chatId", Value::Text(self.chat_id.copy(budget)?), budget)
    }

    /// `{ role, data, saying, time, generationInfo, promptInfo, chatId }`, the appended output.
    pub fn appended(&self, role: MessageRole, data: &Text, budget: &Budget) -> Result<Value, StageError> {
        let mut message = object(budget)?;
        define(&mut message, "role", Value::Text(role_text(role, budget)?), budget)?;
        define(&mut message, "data", Value::Text(data.copy(budget)?), budget)?;
        self.define_rest(&mut message, budget)?;
        Ok(message)
    }

    /// `{ ...snapshot, role: 'char', data, saying, time, generationInfo, promptInfo, chatId }`, the continued message.
    pub fn continued(&self, snapshot: &Value, data: &Text, budget: &Budget) -> Result<Value, StageError> {
        if !matches!(snapshot, Value::Object(_)) { return Err(invalid("message")); }
        let mut message = copy(snapshot, budget)?;
        define(&mut message, "role", Value::Text(Text::from_str("char", budget)?), budget)?;
        define(&mut message, "data", Value::Text(data.copy(budget)?), budget)?;
        self.define_rest(&mut message, budget)?;
        Ok(message)
    }
}

/// `{ ...snapshot, data }` (`commitData`).
pub(super) fn with_data(snapshot: &Value, data: &Text, budget: &Budget) -> Result<Value, StageError> {
    if !matches!(snapshot, Value::Object(_)) { return Err(invalid("message")); }
    let mut message = copy(snapshot, budget)?;
    define(&mut message, "data", Value::Text(data.copy(budget)?), budget)?;
    Ok(message)
}

/// `precomputedResponseVariants(message, values)` (`src/ts/responseVariants.ts:56-64`) for two or more values.
pub(super) fn with_variants(message: &Value, values: &[Text], budget: &Budget) -> Result<Value, StageError> {
    // `message.chatId ?? message.generationInfo?.generationId ?? 'response'`, as a template literal reads it.
    let group = match property(message, "chatId", budget)? {
        Some(id) if !nullish(Some(id)) => Some(id),
        _ => match property(message, "generationInfo", budget)? {
            Some(info) if !nullish(Some(info)) => property(info, "generationId", budget)?.filter(|id| !nullish(Some(*id))),
            _ => None,
        },
    };
    let group = match group {
        Some(id) => id.to_text(budget).map_err(|_| unsupported("responseVariants.groupId"))?,
        None => Text::from_str("response", budget)?,
    };
    let mut candidates = OwnedVec::new(budget)?;
    let mut first = None;
    for (index, value) in values.iter().enumerate() {
        let mut id = group.copy(budget)?;
        id.append(&Text::from_str(&format!(":{index}"), budget)?)?;
        // `snapshotResponse([{ ...message, data }])`: the copy without `responseVariants`.
        let mut snapshot = with_data(message, value, budget)?;
        snapshot.take(b"responseVariants", budget)?;
        let mut list = OwnedVec::new(budget)?;
        list.push(snapshot)?;
        let mut candidate = object(budget)?;
        if first.is_none() { first = Some(id.copy(budget)?); }
        define(&mut candidate, "id", Value::Text(id), budget)?;
        define(&mut candidate, "messages", Value::Array(list), budget)?;
        candidates.push(candidate)?;
    }
    let mut variants = object(budget)?;
    define(&mut variants, "groupId", Value::Text(group), budget)?;
    define(&mut variants, "selectedId", Value::Text(first.ok_or_else(|| invalid("responseVariants"))?), budget)?;
    define(&mut variants, "candidates", Value::Array(candidates), budget)?;
    let mut result = copy(message, budget)?;
    define(&mut result, "responseVariants", variants, budget)?;
    Ok(result)
}

/// `trackRerollOutput(chat, message)` (`src/ts/responseVariants.ts:78-82`): while a reroll is generating, its recovery
/// keeps a copy of each output commit under the message's `chatId`. Returns the new `rerollRecovery`, or `None` when
/// nothing is tracked.
pub(super) fn tracked_reroll(conversation: &Value, message: &Value, budget: &Budget) -> Result<Option<Value>, StageError> {
    let Some(recovery) = property(conversation, "rerollRecovery", budget)? else { return Ok(None) };
    if nullish(Some(recovery)) { return Ok(None); }
    let generating = matches!(property(recovery, "phase", budget)?, Some(Value::Text(phase)) if phase.equals_ascii(b"generating"));
    let id = property(message, "chatId", budget)?;
    if !generating || !id.is_some_and(Value::truthy) { return Ok(None); }
    let key = id.expect("chatId").to_text(budget).map_err(|_| unsupported("rerollRecovery.outputs"))?;
    let mut recovery = copy(recovery, budget)?;
    let outputs = property_mut(&mut recovery, "outputs", budget)?.ok_or_else(|| invalid("rerollRecovery.outputs"))?;
    if !matches!(outputs, Value::Object(_)) { return Err(invalid("rerollRecovery.outputs")); }
    outputs.define(key, copy(message, budget)?, budget)?;
    Ok(Some(recovery))
}

const MISSING_ROLE: &str = "Cannot read properties of undefined (reading 'role')";

/// `@@repeat_back`'s `lastChat` for the message at `chat_index` (`src/ts/process/scripts.ts:1034-1052`) over the
/// complete conversation: the `data` of the nearest earlier message with that message's role, else the selected
/// greeting. The walk reads the message at `chat_index`, which throws when it does not exist.
pub(super) fn repeat_back_source(conversation: &Value, character: &Value, chat_index: i64, budget: &Budget)
    -> Result<HostReply, StageError> {
    let messages = messages(conversation, budget)?;
    // `message.role`, which throws for `null` or `undefined`.
    let role = |message: &Value| -> Result<Option<Value>, StageError> {
        if nullish(Some(message)) { return Ok(None); }
        Ok(Some(match property(message, "role", budget)? { Some(role) => copy(role, budget)?, None => Value::Undefined }))
    };
    let mut pointer = chat_index - 1;
    while pointer >= 0 {
        budget.work(1)?;
        let (Some(current), Some(target)) = (messages.get(pointer as usize), usize::try_from(chat_index).ok().and_then(|at| messages.get(at)))
        else { return Ok(HostReply::Failed { message: Text::from_str(MISSING_ROLE, budget)? }) };
        let (Some(current_role), Some(target_role)) = (role(current)?, role(target)?)
        else { return Ok(HostReply::Failed { message: Text::from_str(MISSING_ROLE, budget)? }) };
        if strict_equals(&current_role, &target_role) {
            let data = property(current, "data", budget)?.map(|data| copy(data, budget)).transpose()?;
            return Ok(HostReply::Value(data.unwrap_or(Value::Undefined)));
        }
        pointer -= 1;
    }
    // `chat.fmIndex === -1 ? selchar.firstMessage : selchar.alternateGreetings[chat.fmIndex]`
    let greeting = match property(conversation, "fmIndex", budget)? {
        Some(Value::Number(index)) if *index == -1.0 => property(character, "firstMessage", budget)?,
        index => {
            let Some(Value::Array(greetings)) = property(character, "alternateGreetings", budget)? else {
                return Ok(HostReply::Failed { message: Text::from_str("Cannot read properties of undefined", budget)? });
            };
            match index {
                Some(Value::Number(index)) if *index >= 0.0 && index.fract() == 0.0 => greetings.as_slice().get(*index as usize),
                _ => None,
            }
        }
    };
    Ok(HostReply::Value(greeting.map(|value| copy(value, budget)).transpose()?.unwrap_or(Value::Undefined)))
}

/// Whether an edit can move the messages after it.
pub(super) fn moves_messages(effect: &Effect) -> bool {
    matches!(effect, Effect::Message(MessageEdit::Cut { .. } | MessageEdit::Remove { .. } | MessageEdit::Insert { .. }
        | MessageEdit::Replace { .. }))
}
