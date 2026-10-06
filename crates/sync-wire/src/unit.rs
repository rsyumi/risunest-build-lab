use crate::{canonical, descriptor::RecordDescriptor, hash, payload_value, stamp::Stamp, validate_hash, Result, WireError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

pub const MAX_UNIT_KEY_BYTES: usize = 64 * 1024;
/// Largest decoded canonical JSON payload an inline value may carry. Larger
/// values travel as content objects so every page and push stays bounded.
pub const MAX_INLINE_UNIT_BYTES: usize = 262_144;
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UnitKey(String);
impl UnitKey {
    pub fn new(components: &[&str]) -> Result<Self> {
        let value = serde_json::to_string(components).map_err(|_| WireError("invalid-unit-key"))?;
        value.try_into()
    }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn components(&self) -> Vec<String> { serde_json::from_str(&self.0).unwrap() }
}
impl TryFrom<String> for UnitKey {
    type Error = WireError;
    fn try_from(value: String) -> Result<Self> {
        if value.len() > MAX_UNIT_KEY_BYTES { return Err(WireError("unit-key-too-large")); }
        let components: Vec<String> = serde_json::from_str(&value).map_err(|_| WireError("invalid-unit-key"))?;
        if components.is_empty() || components[0].is_empty()
            || serde_json::to_string(&components).map_err(|_| WireError("invalid-unit-key"))? != value {
            return Err(WireError("invalid-unit-key"));
        }
        let n = components.len();
        let kind = components[0].as_str();
        let valid = match kind {
            "root" | "toggle" | "variable" | "preset-protected" | "archive" | "asset" | "inlay" | "hypa" => n == 2,
            "character" | "preset" | "persona" | "messages" | "record" | "plugin" => n == 3,
            "conversation" | "plugin-local" => n == 4,
            // Record kinds and order scopes this build does not know stay opaque.
            "exists" => match components.get(1).map(String::as_str) {
                Some("conversation") => n == 4,
                Some("character" | "preset" | "persona" | "modules" | "loadouts" | "customModels") => n == 3,
                Some("plugins" | "") | None => false,
                Some(_) => n >= 3,
            },
            "order" => match components.get(1).map(String::as_str) {
                Some("conversations" | "plugin-storage") => n == 3,
                Some("characters" | "presets" | "personas" | "modules" | "plugins" | "loadouts" | "customModels") => n == 2,
                Some("") | None => false,
                Some(_) => true,
            },
            _ => true,
        };
        if !valid
            || (kind == "record" && !matches!(components[1].as_str(), "modules" | "plugins" | "loadouts" | "customModels"))
            // Every `toggle_*` global variable is a toggle unit and no other is.
            || (matches!(kind, "toggle" | "variable") && (kind == "toggle") != components[1].starts_with("toggle_"))
        {
            return Err(WireError("invalid-unit-key-shape"));
        }
        let structural_ids = match kind {
            "character" | "preset" | "persona" | "archive" => &components[1..2],
            "conversation" | "messages" => &components[1..3],
            "exists" => &components[2..],
            "record" if components[1] != "plugins" => &components[2..3],
            "order" if components[1] == "conversations" => &components[2..3],
            _ => &components[0..0],
        };
        if structural_ids.iter().any(String::is_empty) { return Err(WireError("invalid-unit-key-shape")); }
        Ok(Self(value))
    }
}
impl From<UnitKey> for String { fn from(value: UnitKey) -> Self { value.0 } }

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum UnitValue {
    Inline { bytes: String },
    Object { #[serde(rename = "descriptorHash")] descriptor_hash: String, descriptor: RecordDescriptor },
    Deleted,
}
impl<'de> Deserialize<'de> for UnitValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
        enum Input {
            Inline { bytes: String },
            Object { #[serde(rename = "descriptorHash")] descriptor_hash: String, descriptor: RecordDescriptor },
            Deleted {},
        }
        Ok(match Input::deserialize(d)? {
            Input::Inline { bytes } => Self::Inline { bytes },
            Input::Object { descriptor_hash, descriptor } => Self::Object { descriptor_hash, descriptor },
            Input::Deleted {} => Self::Deleted,
        })
    }
}
impl UnitValue {
    pub fn inline(bytes: &[u8]) -> Result<Self> {
        let canonical = payload_value::canonicalize(bytes)?;
        if canonical.len() > MAX_INLINE_UNIT_BYTES { return Err(WireError("inline-unit-too-large")); }
        Ok(Self::Inline { bytes: URL_SAFE_NO_PAD.encode(canonical) })
    }
    pub fn object(descriptor: RecordDescriptor) -> Result<Self> {
        Ok(Self::Object { descriptor_hash: descriptor.hash()?, descriptor })
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Inline { bytes } => {
                let decoded = URL_SAFE_NO_PAD.decode(bytes).map_err(|_| WireError("invalid-inline-value"))?;
                if decoded.len() > MAX_INLINE_UNIT_BYTES { return Err(WireError("inline-unit-too-large")); }
                if URL_SAFE_NO_PAD.encode(&decoded) != *bytes || payload_value::canonicalize(&decoded)? != decoded { return Err(WireError("noncanonical-inline-value")); }
            }
            Self::Object { descriptor_hash, descriptor } => {
                validate_hash(descriptor_hash)?; descriptor.validate()?;
                if descriptor.hash()? != *descriptor_hash { return Err(WireError("descriptor-hash-mismatch")); }
            }
            Self::Deleted => {}
        }
        Ok(())
    }
    pub fn identity(&self) -> Result<String> { self.validate()?; Ok(hash(&canonical::encode(self)?)) }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LwwDecision { KeepLocal, ApplyRemote, Identical }
pub fn compare_version(local_stamp: &Stamp, local_value: &UnitValue, remote_stamp: &Stamp, remote_value: &UnitValue) -> Result<LwwDecision> {
    local_stamp.validate()?; remote_stamp.validate()?;
    let local_identity = local_value.identity()?;
    let remote_identity = remote_value.identity()?;
    match remote_stamp.cmp(local_stamp) {
        Ordering::Less => Ok(LwwDecision::KeepLocal), Ordering::Greater => Ok(LwwDecision::ApplyRemote),
        Ordering::Equal if local_identity == remote_identity => Ok(LwwDecision::Identical),
        Ordering::Equal => Err(WireError("equal-stamp-integrity")),
    }
}
