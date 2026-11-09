//! Prompt messages as the TS pipeline builds them.

use crate::budget::{Budget, Error, OwnedVec};
use crate::text::Text;
use crate::value::{define_property as define, Value};
use serde::{Deserialize, Serialize};

type Properties = OwnedVec<(Text, Value)>;

fn define_text(properties: &mut Properties, key: &str, value: &Text, budget: &Budget) -> Result<(), Error> {
    define(properties, Text::from_str(key, budget)?, Value::Text(value.copy(budget)?), budget)
}

fn define_texts(properties: &mut Properties, key: &str, values: &[Text], budget: &Budget) -> Result<(), Error> {
    let mut array = OwnedVec::new(budget)?;
    for value in values { array.push(Value::Text(value.copy(budget)?))?; }
    define(properties, Text::from_str(key, budget)?, Value::Array(array), budget)
}

fn define_extra(properties: &mut Properties, extra: &[(Text, Value)], budget: &Budget) -> Result<(), Error> {
    for (key, value) in extra { define(properties, key.copy(budget)?, copy_value(value, 0, budget)?, budget)?; }
    Ok(())
}

fn copy_value(value: &Value, depth: usize, budget: &Budget) -> Result<Value, Error> {
    budget.work(1)?;
    if depth > budget.limits().max_depth { return Err(Error::Limit("json-depth")); }
    Ok(match value {
        Value::Undefined => Value::Undefined,
        Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(value) => Value::Number(*value),
        Value::Text(value) => Value::Text(value.copy(budget)?),
        Value::Array(values) => {
            let mut copy = OwnedVec::new(budget)?;
            for value in values.as_slice() { copy.push(copy_value(value, depth + 1, budget)?)?; }
            Value::Array(copy)
        }
        Value::Object(values) => {
            let mut copy = OwnedVec::new(budget)?;
            for (key, value) in values.as_slice() { copy.push((key.copy(budget)?, copy_value(value, depth + 1, budget)?))?; }
            Value::Object(copy)
        }
    })
}

// The own properties of an object operand in their stored order; `operation` names the refused shape.
fn properties(value: Value, operation: &'static str) -> Result<Vec<(Text, Value)>, Error> {
    match value { Value::Object(mut values) => Ok(std::mem::take(&mut values.data)), _ => Err(Error::Type { operation }) }
}

// An absent property and one holding `undefined` read the same, as every TS reader of these objects treats them.
fn optional_text(value: Value, operation: &'static str) -> Result<Option<Text>, Error> {
    match value { Value::Undefined => Ok(None), Value::Text(value) => Ok(Some(value)), _ => Err(Error::Type { operation }) }
}

fn optional_bool(value: Value, operation: &'static str) -> Result<Option<bool>, Error> {
    match value { Value::Undefined => Ok(None), Value::Bool(value) => Ok(Some(value)), _ => Err(Error::Type { operation }) }
}

fn optional_number(value: Value, operation: &'static str) -> Result<Option<f64>, Error> {
    match value { Value::Undefined => Ok(None), Value::Number(value) => Ok(Some(value)), _ => Err(Error::Type { operation }) }
}

fn optional_texts(value: Value, operation: &'static str, budget: &Budget) -> Result<Option<Vec<Text>>, Error> {
    let values = match value { Value::Undefined => return Ok(None), Value::Array(mut values) => std::mem::take(&mut values.data), _ => return Err(Error::Type { operation }) };
    let mut texts = Vec::new();
    for value in values {
        budget.work(1)?;
        match value { Value::Text(value) => texts.push(value), _ => return Err(Error::Type { operation }) }
    }
    Ok(Some(texts))
}

fn name_of(key: &Text, names: &[&'static str]) -> Option<&'static str> {
    names.iter().copied().find(|name| key.equals_ascii(name.as_bytes()))
}

/// `OpenAIChat.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { System, User, Assistant, Function }

/// The role a stored chat message carries (`Chat.message[].role`), unlike the prompt [`Role`]. A Lua or trigger edit
/// stores `user` for the name `user` and `char` for any other (`src/ts/process/scriptings.ts:511-518`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole { User, Char }

/// One prompt array entry, mirroring `OpenAIChat` (`src/ts/process/index.svelte.ts:134-144`).
/// An optional field is `None` where TS leaves the property absent.
#[derive(Debug)]
pub struct PromptMessage {
    pub role: Role,
    pub content: Text,
    pub memo: Option<Text>,
    pub name: Option<Text>,
    pub removable: Option<bool>,
    pub attr: Option<Vec<Text>>,
    pub multimodals: Option<Vec<Multimodal>>,
    pub thoughts: Option<Vec<Text>>,
    pub cache_point: Option<bool>,
    /// Properties that are not `OpenAIChat` fields, in the order TS held them.
    pub extra: Vec<(Text, Value)>,
}
impl PromptMessage {
    pub fn new(role: Role, content: Text) -> Self {
        Self {
            role, content, memo: None, name: None, removable: None, attr: None, multimodals: None, thoughts: None,
            cache_point: None, extra: Vec::new(),
        }
    }
    /// The `OpenAIChat` object TS holds, with keys in one canonical order: `role`, `content`, `name`, `memo`, `attr`,
    /// `multimodals` (left out when empty), `thoughts`, `removable`, `cachePoint`, then `extra`. As in any JS object, an
    /// array-index key in `extra` comes first, before `role`; the other `extra` keys follow in stored order. A `None`
    /// field is left out; no TS construction site sets these fields to `null`. This is the order of the main
    /// construction site (`src/ts/process/index.svelte.ts:2059-2068`), with `removable` and `cachePoint` appended as TS
    /// appends them (`:2238`, `:2488`) and `name` after `content` as the example messages place it
    /// (`src/ts/process/exampleMessages.ts:36-47`). A message built at another site can order its keys differently, so
    /// the request trigger's `displayData` (`src/ts/process/request/request.ts:265`) may differ in key order; the
    /// oracles record such cases in the tolerance ledger instead of storing an order here.
    pub fn to_value(&self, budget: &Budget) -> Result<Value, Error> {
        let mut properties = OwnedVec::new(budget)?;
        let role = match self.role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => "function" };
        define(&mut properties, Text::from_str("role", budget)?, Value::Text(Text::from_str(role, budget)?), budget)?;
        define_text(&mut properties, "content", &self.content, budget)?;
        if let Some(name) = &self.name { define_text(&mut properties, "name", name, budget)?; }
        if let Some(memo) = &self.memo { define_text(&mut properties, "memo", memo, budget)?; }
        if let Some(attr) = &self.attr { define_texts(&mut properties, "attr", attr, budget)?; }
        if let Some(multimodals) = self.multimodals.as_ref().filter(|values| !values.is_empty()) {
            let mut array = OwnedVec::new(budget)?;
            for multimodal in multimodals { array.push(multimodal.to_value(budget)?)?; }
            define(&mut properties, Text::from_str("multimodals", budget)?, Value::Array(array), budget)?;
        }
        if let Some(thoughts) = &self.thoughts { define_texts(&mut properties, "thoughts", thoughts, budget)?; }
        if let Some(removable) = self.removable {
            define(&mut properties, Text::from_str("removable", budget)?, Value::Bool(removable), budget)?;
        }
        if let Some(cache_point) = self.cache_point {
            define(&mut properties, Text::from_str("cachePoint", budget)?, Value::Bool(cache_point), budget)?;
        }
        // Like any JS object, an array-index key in `extra` sorts before the named fields.
        define_extra(&mut properties, &self.extra, budget)?;
        Ok(Value::Object(properties))
    }
    /// Reads an `OpenAIChat` object as a Lua edit trigger, the request trigger or a plugin replacer returns it. Every
    /// property that is not an `OpenAIChat` field goes to `extra` in input order.
    pub fn from_value(value: Value, budget: &Budget) -> Result<Self, Error> {
        const FIELDS: [&str; 9] = ["role", "content", "memo", "name", "removable", "attr", "multimodals", "thoughts", "cachePoint"];
        let (mut role, mut content) = (None, None);
        let mut message = Self::new(Role::User, Text::from_str("", budget)?);
        for (key, value) in properties(value, "prompt-message")? {
            budget.work(1)?;
            match name_of(&key, &FIELDS) {
                Some("role") => role = match value {
                    Value::Undefined => None,
                    Value::Text(text) if text.equals_ascii(b"system") => Some(Role::System),
                    Value::Text(text) if text.equals_ascii(b"user") => Some(Role::User),
                    Value::Text(text) if text.equals_ascii(b"assistant") => Some(Role::Assistant),
                    Value::Text(text) if text.equals_ascii(b"function") => Some(Role::Function),
                    _ => return Err(Error::Type { operation: "prompt-message-role" }),
                },
                Some("content") => content = optional_text(value, "prompt-message-content")?,
                Some("memo") => message.memo = optional_text(value, "prompt-message-memo")?,
                Some("name") => message.name = optional_text(value, "prompt-message-name")?,
                Some("removable") => message.removable = optional_bool(value, "prompt-message-removable")?,
                Some("attr") => message.attr = optional_texts(value, "prompt-message-attr", budget)?,
                Some("multimodals") => message.multimodals = match value {
                    Value::Undefined => None,
                    Value::Array(mut values) => {
                        let mut multimodals = Vec::new();
                        for value in std::mem::take(&mut values.data) { multimodals.push(Multimodal::from_value(value, budget)?); }
                        Some(multimodals)
                    }
                    _ => return Err(Error::Type { operation: "prompt-message-multimodals" }),
                },
                Some("thoughts") => message.thoughts = optional_texts(value, "prompt-message-thoughts", budget)?,
                Some("cachePoint") => message.cache_point = optional_bool(value, "prompt-message-cache-point")?,
                _ => message.extra.push((key, value)),
            }
        }
        message.role = role.ok_or(Error::Type { operation: "prompt-message-role" })?;
        message.content = content.ok_or(Error::Type { operation: "prompt-message-content" })?;
        Ok(message)
    }
}

/// `MultiModal.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MultimodalKind { Image, Video, Audio, Signature }

/// A stored asset the host resolves to bytes, so prompt arrays need not carry the payload.
#[derive(Debug, PartialEq, Eq)]
pub struct AssetRef(pub Text);

#[derive(Debug, PartialEq)]
pub enum MultimodalData {
    /// The `base64` field as TS holds it.
    Inline(Text),
    Asset(AssetRef),
}

/// `MultiModal` (`src/ts/process/index.svelte.ts:169-174`).
#[derive(Debug)]
pub struct Multimodal {
    pub kind: MultimodalKind,
    pub data: MultimodalData,
    pub width: Option<f64>,
    pub height: Option<f64>,
    /// Properties that are not `MultiModal` fields, in the order TS held them; only Lua and plugin results carry them.
    pub extra: Vec<(Text, Value)>,
}
impl Multimodal {
    /// The `MultiModal` object TS holds, with keys in the order its builders use
    /// (`src/ts/process/index.svelte.ts:1962-1967`, `src/ts/process/scriptings.ts:929-934`): `type`, `base64`, `width`,
    /// `height`, then `extra`, with a `None` field left out. An array-index key in `extra` comes first, before `type`, as
    /// in any JS object; the other `extra` keys follow in stored order. `MultimodalData::Asset` has no TS form, so
    /// the owner resolves it to `Inline` first.
    pub fn to_value(&self, budget: &Budget) -> Result<Value, Error> {
        let MultimodalData::Inline(base64) = &self.data else { return Err(Error::Invariant("multimodal-asset-unresolved")) };
        let mut properties = OwnedVec::new(budget)?;
        let kind = match self.kind {
            MultimodalKind::Image => "image", MultimodalKind::Video => "video",
            MultimodalKind::Audio => "audio", MultimodalKind::Signature => "signature",
        };
        define(&mut properties, Text::from_str("type", budget)?, Value::Text(Text::from_str(kind, budget)?), budget)?;
        define_text(&mut properties, "base64", base64, budget)?;
        if let Some(width) = self.width { define(&mut properties, Text::from_str("width", budget)?, Value::Number(width), budget)?; }
        if let Some(height) = self.height { define(&mut properties, Text::from_str("height", budget)?, Value::Number(height), budget)?; }
        define_extra(&mut properties, &self.extra, budget)?;
        Ok(Value::Object(properties))
    }
    /// Every property that is not a `MultiModal` field goes to `extra` in input order.
    pub fn from_value(value: Value, budget: &Budget) -> Result<Self, Error> {
        const FIELDS: [&str; 4] = ["type", "base64", "width", "height"];
        let (mut kind, mut base64) = (None, None);
        let (mut width, mut height, mut extra) = (None, None, Vec::new());
        for (key, value) in properties(value, "multimodal")? {
            budget.work(1)?;
            match name_of(&key, &FIELDS) {
                Some("type") => kind = match value {
                    Value::Undefined => None,
                    Value::Text(text) if text.equals_ascii(b"image") => Some(MultimodalKind::Image),
                    Value::Text(text) if text.equals_ascii(b"video") => Some(MultimodalKind::Video),
                    Value::Text(text) if text.equals_ascii(b"audio") => Some(MultimodalKind::Audio),
                    Value::Text(text) if text.equals_ascii(b"signature") => Some(MultimodalKind::Signature),
                    _ => return Err(Error::Type { operation: "multimodal-type" }),
                },
                Some("base64") => base64 = optional_text(value, "multimodal-base64")?,
                Some("width") => width = optional_number(value, "multimodal-width")?,
                Some("height") => height = optional_number(value, "multimodal-height")?,
                _ => extra.push((key, value)),
            }
        }
        Ok(Self {
            kind: kind.ok_or(Error::Type { operation: "multimodal-type" })?,
            data: MultimodalData::Inline(base64.ok_or(Error::Type { operation: "multimodal-base64" })?),
            width, height, extra,
        })
    }
}
