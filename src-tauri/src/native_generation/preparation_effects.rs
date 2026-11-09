use super::inputs::{PreparationError, PreparationSnapshot};
use risunest_ir::chat::MessageRole;
use risunest_ir::stage::{Effect, MessageEdit};
use risunest_ir::{Budget, Limits, Text, Value as Ir};
use risunest_records::edits::{at_index, relative_index};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{atomic::AtomicBool, Arc};

/// The author note a send writes with its chat variables, and the stored note it read (`None` when absent).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedNote {
    pub value: Value,
    pub original: Option<Value>,
}

/// The stored history rows the start trigger's message edits change: `rows` replace `delete_count` rows at `start` of
/// the `base_count` rows the preparation read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedMessages {
    pub base_count: usize,
    pub start: usize,
    pub delete_count: usize,
    pub rows: Vec<Value>,
}

impl PreparedMessages {
    /// The history's row count once the edits are stored.
    pub(crate) fn count(&self) -> usize { self.base_count - self.delete_count + self.rows.len() }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PreparedVariablePlan {
    /// Chat-variable writes in effect order; a later write of the same name keeps the last value.
    pub assignments: Vec<(String, Value)>,
    pub original: BTreeMap<String, Option<Value>>,
    pub note: Option<PreparedNote>,
    /// The message edits, which TS stores with the variables (`ScriptConversationAccess.commit`).
    pub messages: Option<PreparedMessages>,
    /// Canonical GUI selection, independent of the effective send preset override.
    pub selected_preset_id: Option<String>,
}

impl PreparedVariablePlan {
    pub(crate) fn is_empty(&self) -> bool { self.assignments.is_empty() && self.note.is_none() && self.messages.is_none() }
}

fn string(text: &Text) -> Option<String> { String::from_utf16(&text.units().collect::<Vec<_>>()).ok() }

// The JSON TS stores for a value, as `JSON.stringify` writes it (-0 is stored as 0). TS stores a non-finite number as null
// and removes the key for `undefined`; those values, and arrays or objects, decline here for now.
fn stored(value: &Ir) -> Option<Value> {
    match value {
        Ir::Null => Some(Value::Null),
        Ir::Bool(value) => Some(Value::Bool(*value)),
        Ir::Text(text) => string(text).map(Value::String),
        Ir::Number(number) if number.is_finite() => {
            let limits = Limits { max_owned_bytes: 1 << 12, max_json_bytes: 1 << 10, max_text_units: 1 << 10, max_values: 1 << 4,
                max_depth: 1, max_work: 1 << 12 };
            let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).ok()?;
            let text = risunest_ir::json::stringify(value, &budget).ok()??;
            serde_json::from_str(&string(&text)?).ok()
        }
        _ => None,
    }
}

// One trigger edit of the stored rows under the index rules of `risunest_records::edits::edit_messages`, which the prompt
// reads the same edits through. Message data is text; other edits and values decline for now.
fn edit_rows(rows: &mut Vec<Value>, edit: &MessageEdit) -> Result<(), PreparationError> {
    let declined = || PreparationError::Invalid("effect.message".into());
    let text = |data: &Ir| match data { Ir::Text(text) => string(text).map(Value::String), _ => None }.ok_or_else(declined);
    match edit {
        MessageEdit::SetData { index, data } => {
            let data = text(data)?;
            if let Some(at) = at_index(*index, rows.len()) {
                rows[at].as_object_mut().ok_or_else(declined)?.insert("data".into(), data);
            }
        }
        MessageEdit::Cut { start, end } => {
            let length = rows.len();
            let from = relative_index(*start, length);
            let to = end.map_or(length, |end| relative_index(end, length)).max(from);
            rows.truncate(to);
            rows.drain(..from);
        }
        MessageEdit::Append { role, data, chat_id } => {
            let role = match role { MessageRole::User => "user", MessageRole::Char => "char" };
            rows.push(json!({ "role": role, "data": text(data)?, "chatId": string(chat_id).ok_or_else(declined)? }));
        }
        _ => return Err(PreparationError::Invalid("effect".into())),
    }
    Ok(())
}

// The edited rows as one range of the stored ones: the rows between the unchanged prefix and suffix.
fn replacement(stored: &[Value], rows: &[Value]) -> Option<PreparedMessages> {
    let start = stored.iter().zip(rows).take_while(|(stored, row)| stored == row).count();
    let end = stored[start..].iter().rev().zip(rows[start..].iter().rev()).take_while(|(stored, row)| stored == row).count();
    if start == stored.len() && start == rows.len() { return None; }
    Some(PreparedMessages { base_count: stored.len(), start, delete_count: stored.len() - start - end,
        rows: rows[start..rows.len() - end].to_vec() })
}

// Retain the representable prefix if a later completed effect cannot be stored by this route.
pub(crate) fn preparation_variables(snapshot: &PreparationSnapshot, effects: &[Effect])
    -> (PreparedVariablePlan, Result<(), PreparationError>)
{
    let mut plan = PreparedVariablePlan::default();
    let mut edited: Option<Vec<Value>> = None;
    let convert = |plan: &mut PreparedVariablePlan, edited: &mut Option<Vec<Value>>| -> Result<(), PreparationError> {
        for effect in effects {
            match effect {
                Effect::ChatVariable { name, value } => {
                    let name = string(name).ok_or_else(|| PreparationError::Invalid("effect.variable.name".into()))?;
                    let value = stored(value).ok_or_else(|| PreparationError::Invalid("effect.variable.value".into()))?;
                    let state = match snapshot.history.conversation.get("scriptstate") {
                        None | Some(Value::Null) => None,
                        Some(Value::Object(state)) => Some(state),
                        _ => return Err(PreparationError::Invalid("scriptstate".into())),
                    };
                    let key = format!("${name}");
                    plan.original.entry(key.clone()).or_insert_with(|| state.and_then(|state| state.get(&key)).cloned());
                    plan.selected_preset_id = snapshot.selected_preset_id.clone();
                    plan.assignments.push((name, value));
                }
                Effect::ChatField { field, value } if field.equals_ascii(b"note") => {
                    // The author note is text.
                    let value = match value { Ir::Text(text) => string(text).map(Value::String), _ => None }
                        .ok_or_else(|| PreparationError::Invalid("effect.note".into()))?;
                    let original = match plan.note.take() {
                        Some(note) => note.original,
                        None => snapshot.history.conversation.get("note").cloned(),
                    };
                    plan.selected_preset_id = snapshot.selected_preset_id.clone();
                    plan.note = Some(PreparedNote { value, original });
                }
                Effect::Message(edit) => {
                    // An edit that declines leaves the rows as they were.
                    edit_rows(edited.get_or_insert_with(|| snapshot.history.messages.clone()), edit)?;
                    plan.selected_preset_id = snapshot.selected_preset_id.clone();
                }
                // Memory is converted separately, and is never settled for a failed preparation.
                Effect::ChatField { field, .. } if field.equals_ascii(b"lastMemory") => {}
                _ => return Err(PreparationError::Invalid("effect".into())),
            }
        }
        Ok(())
    };
    let mut result = convert(&mut plan, &mut edited);
    // A history the store cannot read back whole declines, as one too large to reserve does.
    if let Some(rows) = edited {
        if crate::persistent_store::generation::generation_history_fits(&rows) {
            plan.messages = replacement(&snapshot.history.messages, &rows);
        } else if result.is_ok() {
            result = Err(PreparationError::Invalid("effect.message".into()));
        }
    }
    (plan, result)
}

/// Admission's dry run of the same conversion: an effect this route cannot store declines as `effect`, so TS runs the
/// send instead.
pub(crate) fn preparation_plan(snapshot: &PreparationSnapshot, effects: &[Effect])
    -> (PreparedVariablePlan, Result<(), PreparationError>)
{
    let (plan, result) = preparation_variables(snapshot, effects);
    (plan, result.map_err(|_| PreparationError::Unsupported("effect".into())))
}
