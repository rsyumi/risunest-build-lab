//! The stored `hypaV3Data` record (`src/ts/process/memory/hypav3.ts:1660-1718`): conversion of the captured value,
//! orphan cleanup and the serializable value a run stores.
use crate::hypa_v2::key::{unique, Key, KeyIndex};
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, OwnedVec, Text, Value};
use std::borrow::Cow;

fn decline(field: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(field)) }
fn text(value: &str, budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_str(value, budget)?) }

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

/// The working record. `root` keeps every other root property in place; `summaries`, and `metrics` after a
/// completed selection, are written back by [`Memory::encode`]. Each summary keeps its own properties in place.
pub(crate) struct Memory {
    root: Value,
    pub(crate) summaries: OwnedVec<Value>,
}

impl Memory {
    /// `toHypaV3Data` over the captured value. A shape the TS policy throws on, or coerces where this port does not,
    /// declines before any dependency call.
    pub(crate) fn decode(stored: Option<Value>, budget: &Budget) -> Result<Self, StageError> {
        let Some(mut root) = stored.filter(Value::truthy) else {
            let mut root = Value::Object(OwnedVec::new(budget)?);
            root.define(text("summaries", budget)?, Value::Null, budget)?;
            return Ok(Self { root, summaries: OwnedVec::new(budget)? });
        };
        if !matches!(root, Value::Object(_)) { return Err(decline("hypaV3Data")); }
        // The legacy property is dropped from the working copy.
        root.take(b"lastSelectedSummaries", budget)?;
        let mut entries = take_array(&mut root, b"summaries", budget)?.ok_or_else(|| decline("hypaV3Data.summaries"))?;
        let mut summaries = OwnedVec::new(budget)?;
        for entry in entries.as_mut_slice() {
            let mut value = std::mem::replace(entry, Value::Undefined);
            if !matches!(value, Value::Object(_)) { return Err(decline("hypaV3Data.summaries")); }
            if !matches!(value.get(b"text", budget)?, Some(Value::Text(_))) { return Err(decline("hypaV3Data.summaries.text")); }
            let mut memos = take_array(&mut value, b"chatMemos", budget)?.ok_or_else(|| decline("hypaV3Data.summaries.chatMemos"))?;
            // `new Set(memos.map(null -> undefined))`: a Set stores -0 as +0.
            for memo in memos.as_mut_slice() {
                budget.work(1)?;
                match memo {
                    Value::Null => *memo = Value::Undefined,
                    Value::Number(number) if *number == 0.0 => *number = 0.0,
                    _ => {}
                }
            }
            let memos = unique(memos.as_mut_slice(), budget)?;
            value.define(text("chatMemos", budget)?, Value::Array(memos), budget)?;
            summaries.push(value)?;
        }
        Ok(Self { root, summaries })
    }

    /// `cleanOrphanedSummary` (`:1686-1708`): a summary survives when every memo is a chat memo or an additional memo.
    pub(crate) fn clean(&mut self, chats: &KeyIndex, additional: &KeyIndex, budget: &Budget) -> Result<(), StageError> {
        let mut survivors = OwnedVec::new(budget)?;
        let mut summaries = std::mem::replace(&mut self.summaries, OwnedVec::new(budget)?);
        for summary in summaries.as_mut_slice() {
            let summary = std::mem::replace(summary, Value::Undefined);
            let mut present = true;
            for memo in memos(&summary, budget)? {
                let found = match Key::of(memo, budget)? {
                    Some(key) => chats.get(&key, budget)?.is_some() || additional.get(&key, budget)?.is_some(),
                    None => false,
                };
                if !found { present = false; break; }
            }
            if present { survivors.push(summary)?; }
        }
        self.summaries = survivors;
        Ok(())
    }

    /// The final summary's last memo, `None` without summaries; an empty memo set reads as `undefined`.
    pub(crate) fn last_memo(&self, budget: &Budget) -> Result<Option<&Value>, StageError> {
        let Some(summary) = self.summaries.as_slice().last() else { return Ok(None) };
        Ok(Some(memos(summary, budget)?.last().unwrap_or(&Value::Undefined)))
    }

    pub(crate) fn text(&self, index: usize, budget: &Budget) -> Result<&Text, StageError> {
        match self.summaries.as_slice()[index].get(b"text", budget)? {
            Some(Value::Text(text)) => Ok(text),
            _ => Err(StageError::Invalid(Cow::Borrowed("hypa-v3-summary-text"))),
        }
    }

    pub(crate) fn important(&self, index: usize, budget: &Budget) -> Result<bool, StageError> {
        Ok(self.summaries.as_slice()[index].get(b"isImportant", budget)?.is_some_and(Value::truthy))
    }

    /// Appends a new summary of the given memo set (`:1195-1201`).
    pub(crate) fn append(&mut self, summary: Text, chat_memos: OwnedVec<Value>, budget: &Budget) -> Result<(), StageError> {
        let mut value = Value::Object(OwnedVec::new(budget)?);
        value.define(text("text", budget)?, Value::Text(summary), budget)?;
        value.define(text("chatMemos", budget)?, Value::Array(chat_memos), budget)?;
        value.define(text("isImportant", budget)?, Value::Bool(false), budget)?;
        value.define(text("categoryId", budget)?, Value::Undefined, budget)?;
        value.define(text("tags", budget)?, Value::Array(OwnedVec::new(budget)?), budget)?;
        self.summaries.push(value)?;
        Ok(())
    }

    /// `toSerializableHypaV3Data` (`:1676-1684`) after an optional `data.metrics` assignment.
    pub(crate) fn encode(self, metrics: Option<Value>, budget: &Budget) -> Result<Value, StageError> {
        let Self { mut root, summaries } = self;
        root.define(text("summaries", budget)?, Value::Array(summaries), budget)?;
        if let Some(metrics) = metrics { root.define(text("metrics", budget)?, metrics, budget)?; }
        Ok(root)
    }
}

fn memos<'a>(summary: &'a Value, budget: &Budget) -> Result<&'a [Value], StageError> {
    match summary.get(b"chatMemos", budget)? {
        Some(Value::Array(values)) => Ok(values.as_slice()),
        _ => Err(StageError::Invalid(Cow::Borrowed("hypa-v3-summary-memos"))),
    }
}
