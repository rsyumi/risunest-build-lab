//! Lore entry fields, folders and dormant lorePlus fields.
//!
//! [`LoreEntry`] is the selection view of one stored `loreBook` object (`src/ts/storage/database.svelte.ts:1398`).
//! Selection reads `key`, `secondkey`, `insertorder`, `comment`, `content`, `mode`, `alwaysActive`, `selective`,
//! `useRegex` and `id`. A folder is an ordinary entry whose `mode` is `folder` and whose key is
//! `\u{f000}folder:<id>`; `folder` on a member entry is never read. The lorePlus-era fields (`extentions`,
//! `activationPercent`, `loreCache`, `bookVersion`), a `folder` that is not a string and any other property stay in
//! [`LoreEntry::extra`] in stored order. The fields selection reads are coerced to the types it uses, so an entry is
//! not a lossless copy of the stored object.
//!
//! A stored value selection cannot treat as TS does is declined only where TS reads it: a truthy non-string `key`
//! or `secondkey` when that key is searched, a missing or non-string `content`, an array or object `insertorder`
//! and a truthy non-string `comment` when the entry is evaluated, and an array or object `id` when it names a chat
//! variable. An entry TS skips never declines.

use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::num::{number_to_text, to_int32};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use std::borrow::Cow;

/// A stored `id`, compared with `===` and stringified as `'__internal_ka_' + id` does.
#[derive(Debug)]
pub enum LoreId {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Text(Text),
    /// An array or object: never `===` another entry's copy, and declined (`lore-id`) where TS stringifies it.
    Object,
}

impl LoreId {
    pub fn strict_equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => left == right,
            (Self::Text(left), Self::Text(right)) => left == right,
            _ => false,
        }
    }
    pub fn is_nullish(&self) -> bool { matches!(self, Self::Undefined | Self::Null) }
}

/// A stored field TS uses as a string once it is truthy.
#[derive(Debug)]
pub enum StoredText {
    Text(Text),
    /// A falsy non-string (`undefined`, `null`, `false`, `0`, `NaN`).
    Falsy,
    /// A truthy non-string, which selection declines where TS reads it.
    Other,
}

impl StoredText {
    fn from_value(value: &Value, budget: &Budget) -> Result<Self, StageError> {
        Ok(match value {
            Value::Text(text) => Self::Text(text.copy(budget)?),
            other if other.truthy() => Self::Other,
            _ => Self::Falsy,
        })
    }
    /// `!!field`.
    pub fn truthy(&self) -> bool {
        match self { Self::Text(text) => text.len_units() != 0, Self::Falsy => false, Self::Other => true }
    }
    /// The string a truthy field holds: `None` when it is falsy, declined with `field` when it is not a string.
    pub fn truthy_text(&self, field: &'static str) -> Result<Option<&Text>, StageError> {
        match self {
            Self::Text(text) if text.len_units() != 0 => Ok(Some(text)),
            Self::Other => Err(unsupported(field)),
            _ => Ok(None),
        }
    }
    pub fn copy(&self, budget: &Budget) -> Result<Self, StageError> {
        Ok(match self { Self::Text(text) => Self::Text(text.copy(budget)?), Self::Falsy => Self::Falsy, Self::Other => Self::Other })
    }
}

#[derive(Debug)]
pub struct LoreEntry {
    pub key: StoredText,
    pub secondkey: StoredText,
    /// `insertorder` as the sort's subtraction converts it; `None` for an array or object.
    pub insertorder: Option<f64>,
    pub comment: StoredText,
    /// `content` when it is a string.
    pub content: Option<Text>,
    /// `mode` when it is a string.
    pub mode: Option<Text>,
    pub always_active: bool,
    pub selective: bool,
    pub use_regex: bool,
    pub id: LoreId,
    /// `folder` when it is a string: the key of the folder entry this entry belongs to.
    pub folder: Option<Text>,
    /// Every other stored property, in stored order.
    pub extra: Vec<(Text, Value)>,
}

pub(crate) fn unsupported(field: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(field)) }

impl LoreEntry {
    /// Copies a stored entry; only a stored entry that is not an object is declined here.
    pub fn from_value(value: &Value, budget: &Budget) -> Result<Self, StageError> {
        let Some(fields) = value.object() else { return Err(unsupported("lore-entry")) };
        let mut entry = Self {
            key: StoredText::Falsy, secondkey: StoredText::Falsy, insertorder: Some(f64::NAN), comment: StoredText::Falsy,
            content: None, mode: None, always_active: false, selective: false, use_regex: false, id: LoreId::Undefined,
            folder: None, extra: Vec::new(),
        };
        for (name, field) in fields {
            budget.work(1)?;
            let Some(name_text) = name.as_unicode() else {
                entry.extra.push((name.copy(budget)?, copy_value(field, budget)?));
                continue;
            };
            match name_text {
                "key" => entry.key = StoredText::from_value(field, budget)?,
                "secondkey" => entry.secondkey = StoredText::from_value(field, budget)?,
                "insertorder" => entry.insertorder = match field {
                    Value::Array(_) | Value::Object(_) => None,
                    field => Some(to_number(field, "lore-insertorder", budget)?),
                },
                "comment" => entry.comment = StoredText::from_value(field, budget)?,
                "content" => entry.content = match field { Value::Text(text) => Some(text.copy(budget)?), _ => None },
                "mode" => entry.mode = match field { Value::Text(text) => Some(text.copy(budget)?), _ => None },
                "alwaysActive" => entry.always_active = field.truthy(),
                "selective" => entry.selective = field.truthy(),
                "useRegex" => entry.use_regex = field.truthy(),
                "id" => entry.id = match field {
                    Value::Undefined => LoreId::Undefined,
                    Value::Null => LoreId::Null,
                    Value::Bool(value) => LoreId::Bool(*value),
                    Value::Number(value) => LoreId::Number(*value),
                    Value::Text(text) => LoreId::Text(text.copy(budget)?),
                    Value::Array(_) | Value::Object(_) => LoreId::Object,
                },
                "folder" => match field {
                    Value::Text(text) => entry.folder = Some(text.copy(budget)?),
                    _ => entry.extra.push((name.copy(budget)?, copy_value(field, budget)?)),
                },
                _ => entry.extra.push((name.copy(budget)?, copy_value(field, budget)?)),
            }
        }
        Ok(entry)
    }

    /// `mode === 'child'`.
    pub fn is_child(&self) -> bool { self.mode.as_ref().is_some_and(|mode| mode.equals_ascii(b"child")) }
    /// `mode === 'folder'`.
    pub fn is_folder(&self) -> bool { self.mode.as_ref().is_some_and(|mode| mode.equals_ascii(b"folder")) }
    /// `!!key`.
    pub fn has_key(&self) -> bool { self.key.truthy() }
    /// `!!secondkey`.
    pub fn has_secondkey(&self) -> bool { self.secondkey.truthy() }
    /// `content`, which TS parses for decorators: declined when it is missing or not a string.
    pub fn content(&self) -> Result<&Text, StageError> { self.content.as_ref().ok_or_else(|| unsupported("lore-content")) }

    /// `id ?? pickHashRand(5555, content).toString()`, the suffix of the keep and dont activate chat variables.
    pub fn variable_suffix(&self, budget: &Budget) -> Result<Text, StageError> {
        Ok(match &self.id {
            LoreId::Undefined | LoreId::Null => number_to_text(pick_hash_rand(5555, self.content()?, budget)?, budget)?,
            LoreId::Bool(value) => Text::from_str(if *value { "true" } else { "false" }, budget)?,
            LoreId::Number(value) => number_to_text(*value, budget)?,
            LoreId::Text(text) => text.copy(budget)?,
            LoreId::Object => return Err(unsupported("lore-id")),
        })
    }
}

/// `ToNumber` for the primitives a stored field holds; a string goes through `ir::num::to_number`.
pub fn to_number(value: &Value, field: &'static str, budget: &Budget) -> Result<f64, StageError> {
    match value {
        Value::Undefined => Ok(f64::NAN),
        Value::Null => Ok(0.0),
        Value::Bool(value) => Ok(if *value { 1.0 } else { 0.0 }),
        Value::Number(value) => Ok(*value),
        Value::Text(text) => Ok(risunest_ir::num::to_number(text, budget)?),
        Value::Array(_) | Value::Object(_) => Err(unsupported(field)),
    }
}

/// A deep copy, as `structuredClone` makes of stored JSON.
pub fn copy_value(value: &Value, budget: &Budget) -> Result<Value, StageError> {
    budget.work(1)?;
    Ok(match value {
        Value::Undefined => Value::Undefined,
        Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(value) => Value::Number(*value),
        Value::Text(text) => Value::Text(text.copy(budget)?),
        Value::Array(values) => {
            let mut copy = OwnedVec::new(budget)?;
            for value in values.as_slice() { copy.push(copy_value(value, budget)?)?; }
            Value::Array(copy)
        }
        Value::Object(fields) => {
            let mut copy = OwnedVec::new(budget)?;
            for (name, value) in fields.as_slice() { copy.push((name.copy(budget)?, copy_value(value, budget)?))?; }
            Value::Object(copy)
        }
    })
}

/// `pickHashRand(cid, word)` (`src/ts/util.ts:1044`): a djb2-style hash of `word`, taken four times in a row, seeds
/// `sfc32`, which then skips `cid % 1000` values.
pub fn pick_hash_rand(cid: u32, word: &Text, budget: &Budget) -> Result<f64, StageError> {
    budget.work(word.len_units().saturating_mul(4).saturating_add((cid % 1000) as usize))?;
    // `hashAddress` is a JS number: `<<` works on its Int32 value while `+` adds the full double.
    let mut hash = 5515.0f64;
    let mut state = [0i32; 4];
    for seed in &mut state {
        for unit in word.units() {
            hash = to_int32(hash).wrapping_shl(5) as f64 + hash + unit as f64;
        }
        *seed = to_int32(hash);
    }
    for _ in 0..cid % 1000 { sfc32(&mut state); }
    Ok(sfc32(&mut state))
}

/// One `sfc32` draw (`src/ts/util.ts:453`).
fn sfc32(state: &mut [i32; 4]) -> f64 {
    let [mut a, mut b, mut c, mut d] = *state;
    let t = a.wrapping_add(b).wrapping_add(d);
    d = d.wrapping_add(1);
    a = b ^ ((b as u32) >> 9) as i32;
    b = c.wrapping_add(c.wrapping_shl(3));
    c = c.wrapping_shl(21) | ((c as u32) >> 11) as i32;
    c = c.wrapping_add(t);
    *state = [a, b, c, d];
    (t as u32) as f64 / 4294967296.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use risunest_ir::json::parse_json;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
            max_depth: 32, max_work: 1 << 28 }, Arc::new(AtomicBool::new(false))).unwrap()
    }

    #[test]
    fn dormant_lore_plus_fields_and_the_folder_are_kept_in_stored_order() {
        let budget = budget();
        let stored = r#"{"extentions":{"risu_case_sensitive":true},"key":"k","activationPercent":50,"content":"c",
            "folder":"folder:f","loreCache":{"key":"x","data":["y"]},"bookVersion":2,"mode":"normal"}"#;
        let entry = LoreEntry::from_value(&parse_json(stored, &budget).unwrap(), &budget).unwrap();
        let names: Vec<String> = entry.extra.iter().map(|(name, _)| String::from_utf16(&name.units().collect::<Vec<_>>()).unwrap()).collect();
        assert_eq!(names, ["extentions", "activationPercent", "loreCache", "bookVersion"]);
        assert!(matches!(entry.extra[0].1.get(b"risu_case_sensitive", &budget).unwrap(), Some(Value::Bool(true))));
        assert!(matches!(entry.extra[1].1, Value::Number(percent) if percent == 50.0));
        assert!(matches!(entry.extra[2].1.get(b"data", &budget).unwrap(), Some(Value::Array(data)) if data.as_slice().len() == 1));
        assert!(entry.folder.as_ref().is_some_and(|folder| folder.units().eq("\u{f000}folder:f".encode_utf16())));
        // A folder that is not a string is never read and stays with the other properties.
        let entry = LoreEntry::from_value(&parse_json(r#"{"folder":5,"key":"k"}"#, &budget).unwrap(), &budget).unwrap();
        assert!(entry.folder.is_none());
        assert!(matches!(entry.extra.as_slice(), [(name, Value::Number(folder))] if name.equals_ascii(b"folder") && *folder == 5.0));
    }
}
