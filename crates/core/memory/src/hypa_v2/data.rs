//! The stored `hypaV2Data` record: released legacy conversion, current-format decoding, invalid-entry cleanup and the
//! serializable value a successful run stores (`src/ts/process/memory/hypav2.ts:146-332`).
use super::key::{unique, Key, KeyIndex};
use risunest_ir::chat::PromptMessage;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, OwnedVec, Text, Value};
use std::borrow::Cow;

pub(crate) struct Main { pub(crate) value: Value, pub(crate) id: f64 }

/// The working record. `root` keeps every other root property in place; its `mainChunks`, `chunks` and
/// `lastMainChunkID` values are written back by [`Memory::encode`].
pub(crate) struct Memory {
    root: Value,
    pub(crate) mains: OwnedVec<Main>,
    pub(crate) details: OwnedVec<Value>,
    pub(crate) last_id: f64,
}

fn decline(field: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(field)) }
fn text(value: &str, budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_str(value, budget)?) }

fn object(keys: &[&str], budget: &Budget) -> Result<Value, StageError> {
    let mut value = Value::Object(OwnedVec::new(budget)?);
    for key in keys { value.define(text(key, budget)?, Value::Null, budget)?; }
    Ok(value)
}

fn slot<'a>(value: &'a mut Value, key: &[u8], budget: &Budget) -> Result<Option<&'a mut Value>, StageError> {
    budget.work(1)?;
    let Value::Object(properties) = value else { return Ok(None) };
    for (name, value) in properties.as_mut_slice() {
        budget.work(name.len_units().min(key.len()).saturating_add(1))?;
        if name.equals_ascii(key) { return Ok(Some(value)); }
    }
    Ok(None)
}

fn take_array(value: &mut Value, key: &[u8], budget: &Budget) -> Result<Option<OwnedVec<Value>>, StageError> {
    match slot(value, key, budget)? {
        Some(slot @ Value::Array(_)) => match std::mem::replace(slot, Value::Null) { Value::Array(values) => Ok(Some(values)), _ => unreachable!() },
        _ => Ok(None),
    }
}

/// The memo identity TS stores for a prompt message: absent is `undefined`, distinct from `null` and `""`.
pub(crate) fn memo_value(message: &PromptMessage, budget: &Budget) -> Result<Value, StageError> {
    Ok(match &message.memo { Some(memo) => Value::Text(memo.copy(budget)?), None => { budget.work(1)?; Value::Undefined } })
}

/// `isOldHypaV2Data` (`:165-184`): both arrays exist and every entry has string `text` and `targetId`, so two empty
/// arrays match even beside current-format fields.
fn is_legacy(value: &Value, budget: &Budget) -> Result<bool, StageError> {
    if !matches!(value, Value::Object(_)) { return Ok(false); }
    for key in [b"chunks".as_slice(), b"mainChunks"] {
        let Some(Value::Array(entries)) = value.get(key, budget)? else { return Ok(false) };
        for entry in entries.as_slice() {
            if !matches!(entry, Value::Object(_)) { return Ok(false); }
            for field in [b"text".as_slice(), b"targetId"] {
                if !matches!(entry.get(field, budget)?, Some(Value::Text(_))) { return Ok(false); }
            }
        }
    }
    Ok(true)
}

fn legacy_text(entry: &mut Value, key: &[u8], budget: &Budget) -> Result<Text, StageError> {
    match entry.take(key, budget)? { Some(Value::Text(text)) => Ok(text), _ => Err(StageError::Invalid(Cow::Borrowed("hypa-v2-legacy"))) }
}

impl Memory {
    fn empty(keys: &[&str], budget: &Budget) -> Result<Self, StageError> {
        Ok(Self { root: object(keys, budget)?, mains: OwnedVec::new(budget)?, details: OwnedVec::new(budget)?, last_id: 0.0 })
    }

    /// Converts the captured record after the conversation is known. A shape the TS policy throws on, or handles
    /// in a way this port does not model, declines before any dependency call.
    pub(crate) fn decode(stored: Option<Value>, chats: &[PromptMessage], memos: &KeyIndex, budget: &Budget) -> Result<Self, StageError> {
        let Some(mut stored) = stored.filter(Value::truthy) else {
            return Self::empty(&["lastMainChunkID", "chunks", "mainChunks"], budget);
        };
        if is_legacy(&stored, budget)? { return Self::legacy(stored, chats, memos, budget); }
        if !matches!(stored, Value::Object(_)) { return Err(decline("hypaV2Data")); }
        let mains = take_array(&mut stored, b"mainChunks", budget)?.ok_or_else(|| decline("hypaV2Data"))?;
        let details = take_array(&mut stored, b"chunks", budget)?.ok_or_else(|| decline("hypaV2Data"))?;
        let mut memory = Self { root: stored, mains: OwnedVec::new(budget)?, details: OwnedVec::new(budget)?, last_id: 0.0 };
        let mut mains = mains;
        for entry in mains.as_mut_slice() {
            let mut value = std::mem::replace(entry, Value::Undefined);
            if !matches!(value, Value::Object(_)) { return Err(decline("hypaV2Data.mainChunks")); }
            let Some(Value::Number(id)) = value.get(b"id", budget)? else { return Err(decline("hypaV2Data.mainChunks")) };
            let id = *id;
            if !matches!(value.get(b"text", budget)?, Some(Value::Text(_))) { return Err(decline("hypaV2Data.mainChunks")); }
            // A missing or non-array `chatMemos` is the released corruption recovery (`:317-330`): an empty set,
            // in place when the property exists and appended otherwise.
            let memos = match take_array(&mut value, b"chatMemos", budget)? {
                Some(mut values) => unique(values.as_mut_slice(), budget)?,
                None => OwnedVec::new(budget)?,
            };
            value.define(text("chatMemos", budget)?, Value::Array(memos), budget)?;
            memory.mains.push(Main { value, id })?;
        }
        let mut details = details;
        for entry in details.as_mut_slice() {
            let value = std::mem::replace(entry, Value::Undefined);
            if !matches!(value, Value::Object(_)) || !matches!(value.get(b"text", budget)?, Some(Value::Text(_))) {
                return Err(decline("hypaV2Data.chunks"));
            }
            memory.details.push(value)?;
        }
        Ok(memory)
    }

    /// `convertOldToNewHypaV2Data` (`:187-271`). Only known fields are rebuilt.
    fn legacy(mut stored: Value, chats: &[PromptMessage], memos: &KeyIndex, budget: &Budget) -> Result<Self, StageError> {
        let mut memory = Self::empty(&["lastMainChunkID", "mainChunks", "chunks"], budget)?;
        let mut old_mains = take_array(&mut stored, b"mainChunks", budget)?.expect("legacy main chunks");
        let mut old_details = take_array(&mut stored, b"chunks", budget)?.expect("legacy chunks");
        let mut mains = OwnedVec::new(budget)?;
        // Old main chunks were unshifted, so the newest comes first.
        for entry in old_mains.as_mut_slice().iter_mut().rev() {
            mains.push((Some(legacy_text(entry, b"text", budget)?), legacy_text(entry, b"targetId", budget)?))?;
        }
        let mut details = OwnedVec::new(budget)?;
        for entry in old_details.as_mut_slice() {
            details.push((legacy_text(entry, b"text", budget)?, legacy_text(entry, b"targetId", budget)?))?;
        }
        let first = |target: &Text| -> Result<Option<usize>, StageError> { memos.get(&Key::text(target, budget)?, budget) };
        for index in 0..mains.as_slice().len() {
            budget.work(1)?;
            let target = &mains.as_slice()[index].1;
            // The predecessor is the immediately previous legacy entry, even if it was skipped.
            let previous = index.checked_sub(1).map(|previous| &mains.as_slice()[previous].1);
            let range = match previous {
                Some(previous) if previous.len_units() != 0 && target.len_units() != 0 => {
                    match (first(previous)?, first(target)?) {
                        (Some(start), Some(end)) => Some(start.min(end)..=start.max(end)),
                        _ => continue,
                    }
                }
                _ if target.len_units() != 0 => match first(target)? { Some(end) => Some(0..=end), None => continue },
                _ => None,
            };
            let mut chat_memos = OwnedVec::new(budget)?;
            if let Some(range) = range {
                for position in range { chat_memos.push(memo_value(&chats[position], budget)?)?; }
            }
            let chat_memos = unique(chat_memos.as_mut_slice(), budget)?;
            let id = memory.last_id;
            let main_text = mains.as_mut_slice()[index].0.take().expect("each legacy main is read once");
            let target = &mains.as_slice()[index].1;
            let mut value = Value::Object(OwnedVec::new(budget)?);
            value.define(text("id", budget)?, Value::Number(id), budget)?;
            value.define(text("text", budget)?, Value::Text(main_text), budget)?;
            value.define(text("chatMemos", budget)?, Value::Array(chat_memos), budget)?;
            value.define(text("lastChatMemo", budget)?, Value::Text(target.copy(budget)?), budget)?;
            memory.mains.push(Main { value, id })?;
            memory.last_id += 1.0;
            for (detail, detail_target) in details.as_slice() {
                budget.work(detail_target.len_units().min(target.len_units()).saturating_add(1))?;
                if detail_target.equals(target) { memory.details.push(detail_value(id, detail.copy(budget)?, budget)?)?; }
            }
        }
        Ok(memory)
    }

    /// `cleanInvalidChunks` (`:274-296`): a main survives when every stored memo occurs in the supplied chats, a
    /// detail when its main ID survives, and the last surviving main's ID becomes `lastMainChunkID`.
    pub(crate) fn clean(&mut self, memos: &KeyIndex, budget: &Budget) -> Result<(), StageError> {
        let mut survivors = OwnedVec::new(budget)?;
        let mut ids = KeyIndex::new();
        let mut mains = std::mem::replace(&mut self.mains, OwnedVec::new(budget)?);
        for main in mains.as_mut_slice() {
            let main = std::mem::replace(main, Main { value: Value::Undefined, id: 0.0 });
            let mut present = true;
            if let Some(Value::Array(values)) = main.value.get(b"chatMemos", budget)? {
                for value in values.as_slice() {
                    let found = match Key::of(value, budget)? { Some(key) => memos.get(&key, budget)?.is_some(), None => false };
                    if !found { present = false; break; }
                }
            }
            if present {
                ids.first(Key::of(&Value::Number(main.id), budget)?.expect("numeric key"), 0, budget)?;
                survivors.push(main)?;
            }
        }
        self.mains = survivors;
        let mut details = std::mem::replace(&mut self.details, OwnedVec::new(budget)?);
        for detail in details.as_mut_slice() {
            let detail = std::mem::replace(detail, Value::Undefined);
            let keep = match Key::of(detail.get(b"mainChunkID", budget)?.unwrap_or(&Value::Undefined), budget)? {
                Some(key) => ids.get(&key, budget)?.is_some(),
                None => false,
            };
            if keep { self.details.push(detail)?; }
        }
        self.last_id = self.mains.as_slice().last().map_or(0.0, |main| main.id);
        Ok(())
    }

    /// The final surviving main's `lastChatMemo`; an absent property reads as `undefined`.
    pub(crate) fn last_chat_memo(&self, budget: &Budget) -> Result<Option<Key>, StageError> {
        let Some(main) = self.mains.as_slice().last() else { return Ok(None) };
        Key::of(main.value.get(b"lastChatMemo", budget)?.unwrap_or(&Value::Undefined), budget)
    }

    pub(crate) fn main_text(&self, index: usize, budget: &Budget) -> Result<&Text, StageError> {
        match self.mains.as_slice()[index].value.get(b"text", budget)? {
            Some(Value::Text(text)) => Ok(text),
            _ => Err(StageError::Invalid(Cow::Borrowed("hypa-v2-main-text"))),
        }
    }

    pub(crate) fn detail_text(&self, index: usize, budget: &Budget) -> Result<&Text, StageError> {
        match self.details.as_slice()[index].get(b"text", budget)? {
            Some(Value::Text(text)) => Ok(text),
            _ => Err(StageError::Invalid(Cow::Borrowed("hypa-v2-detail-text"))),
        }
    }

    /// Appends a summarized main chunk and its details with the next ID (`:524-548`).
    pub(crate) fn append(&mut self, summary: Text, chat_memos: OwnedVec<Value>, last_memo: Value, details: OwnedVec<Option<Text>>,
        budget: &Budget) -> Result<(), StageError> {
        self.last_id += 1.0;
        let id = self.last_id;
        let mut value = Value::Object(OwnedVec::new(budget)?);
        value.define(text("id", budget)?, Value::Number(id), budget)?;
        value.define(text("text", budget)?, Value::Text(summary), budget)?;
        value.define(text("chatMemos", budget)?, Value::Array(chat_memos), budget)?;
        value.define(text("lastChatMemo", budget)?, last_memo, budget)?;
        self.mains.push(Main { value, id })?;
        let mut details = details;
        for detail in details.as_mut_slice() {
            self.details.push(detail_value(id, detail.take().expect("each detail is appended once"), budget)?)?;
        }
        Ok(())
    }

    /// `toSerializableHypaV2Data` (`:307-315`): memo sets are already ordered arrays.
    pub(crate) fn encode(self, budget: &Budget) -> Result<Value, StageError> {
        let Self { mut root, mut mains, details, last_id } = self;
        let mut values = OwnedVec::new(budget)?;
        for main in mains.as_mut_slice() { values.push(std::mem::replace(&mut main.value, Value::Undefined))?; }
        root.define(text("mainChunks", budget)?, Value::Array(values), budget)?;
        root.define(text("chunks", budget)?, Value::Array(details), budget)?;
        root.define(text("lastMainChunkID", budget)?, Value::Number(last_id), budget)?;
        Ok(root)
    }
}

fn detail_value(id: f64, detail: Text, budget: &Budget) -> Result<Value, StageError> {
    let mut value = Value::Object(OwnedVec::new(budget)?);
    value.define(text("mainChunkID", budget)?, Value::Number(id), budget)?;
    value.define(text("text", budget)?, Value::Text(detail), budget)?;
    Ok(value)
}
