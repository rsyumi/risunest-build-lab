//! Logical section codec. Sections carry sorted logical keys, their values and
//! object references. The codec promises nothing about any local SQLite schema,
//! so a device can change its tables without changing what it publishes.
use super::{content_identity::hash, FormatError, Result};
use risunest_sync_wire::head::Sequence;
use serde::{Deserialize, Serialize};

pub const SECTION_CODEC: &str = "risunest.section-codec/v1";

/// Values larger than this are published as their own object instead of riding
/// inline in the entry.
pub const MAX_INLINE_VALUE_BYTES: usize = 4 * 1024;

pub const MAX_SECTION_ENTRY_BYTES: usize = 64 * 1024;
pub const MAX_SECTION_OBJECT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PLUGIN_VALUE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_VECTOR_DIMENSIONS: u32 = 16_384;

const HYPA_ID: &str = "hypa";
const LOCAL_PLUGINS_ID: &str = "local-plugins";
const LOCAL_SETTINGS_ID: &str = "local-settings";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum SectionKind {
    Hypa,
    LocalPlugins,
    LocalSettings,
}

impl SectionKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Hypa => HYPA_ID,
            Self::LocalPlugins => LOCAL_PLUGINS_ID,
            Self::LocalSettings => LOCAL_SETTINGS_ID,
        }
    }
    pub fn parse(id: &str) -> Result<Self> {
        match id {
            HYPA_ID => Ok(Self::Hypa),
            LOCAL_PLUGINS_ID => Ok(Self::LocalPlugins),
            LOCAL_SETTINGS_ID => Ok(Self::LocalSettings),
            _ => Err(FormatError("unknown-section")),
        }
    }
    /// Device-fixed data never reaches a synchronized state. Backup bundles
    /// accept it because restoring the same device is the point.
    pub fn is_synchronizable(self) -> bool {
        !matches!(self, Self::LocalSettings)
    }
    /// The seed that separates one section's content fingerprint from another's.
    pub fn fingerprint_domain(self) -> [u8; 32] {
        let mut bytes = b"risunest.section-fingerprint/v1\0".to_vec();
        bytes.extend_from_slice(self.id().as_bytes());
        hash(&bytes)
    }
}

/// Hypa keys are the normalized cache key. Plugin keys are an explicit
/// structure rather than a concatenation, so no owner, space or key can be
/// mistaken for another combination.
pub fn hypa_entry_key(cache_key: &str) -> Result<String> {
    if cache_key.len() != 64
        || !cache_key
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(FormatError("invalid-section-key"));
    }
    Ok(cache_key.into())
}

pub fn local_plugin_entry_key(owner: &str, space: &str, key: &str) -> Result<String> {
    if owner.is_empty() || space.is_empty() {
        return Err(FormatError("invalid-section-key"));
    }
    let encoded = serde_json::to_string(&[owner, space, key])
        .map_err(|_| FormatError("invalid-section-key"))?;
    if encoded.len() > 64 * 1024 {
        return Err(FormatError("invalid-section-key"));
    }
    Ok(encoded)
}

pub fn decode_local_plugin_entry_key(encoded: &str) -> Result<(String, String, String)> {
    let parts: Vec<String> =
        serde_json::from_str(encoded).map_err(|_| FormatError("invalid-section-key"))?;
    let [owner, space, key]: [String; 3] = parts
        .try_into()
        .map_err(|_| FormatError("invalid-section-key"))?;
    if local_plugin_entry_key(&owner, &space, &key)? != encoded {
        return Err(FormatError("invalid-section-key"));
    }
    Ok((owner, space, key))
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PluginSpace {
    String,
    Json,
}

/// A vector rides inline while it is small and becomes its own object past the
/// inline limit. The reference is resolved through the same pack and object
/// path every other large value uses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectReference {
    pub content_sha256: [u8; 32],
    #[serde(with = "crate::control_integer")]
    pub byte_length: u64,
}

impl ObjectReference {
    fn validate(&self) -> Result<()> {
        if self.byte_length == 0 || self.byte_length > MAX_SECTION_OBJECT_BYTES as u64 {
            Err(FormatError("invalid-section-value"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InlineOrObject {
    #[serde(rename = "inline")]
    Inline(String),
    #[serde(rename = "object")]
    Object(ObjectReference),
}

impl InlineOrObject {
    pub fn inline(bytes: &[u8]) -> Result<Self> {
        use base64::Engine;
        if bytes.len() > MAX_INLINE_VALUE_BYTES {
            return Err(FormatError("section-value-requires-object"));
        }
        Ok(Self::Inline(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
        ))
    }
    pub fn decode_inline(&self) -> Result<Vec<u8>> {
        use base64::Engine;
        let Self::Inline(encoded) = self else {
            return Err(FormatError("section-value-is-an-object"));
        };
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| FormatError("invalid-section-value"))?;
        if bytes.len() > MAX_INLINE_VALUE_BYTES {
            return Err(FormatError("invalid-section-value"));
        }
        Ok(bytes)
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::Inline(_) => self.decode_inline().map(|_| ()),
            Self::Object(reference) => reference.validate(),
        }
    }
}

/// A device value rides inline in its own JSON shape while its stored text is
/// small. Past the inline limit the entry names an object whose body is that
/// stored text, byte for byte.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ValueOrObject {
    #[serde(rename = "value")]
    Inline(serde_json::Value),
    #[serde(rename = "object")]
    Object(ObjectReference),
}

impl ValueOrObject {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Inline(_) => Ok(()),
            // Text up to the inline limit always rides inline, so a smaller
            // object is a shape no device writes.
            Self::Object(reference) if reference.byte_length <= MAX_INLINE_VALUE_BYTES as u64 => {
                Err(FormatError("invalid-section-value"))
            }
            Self::Object(reference) => reference.validate(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HypaValue {
    pub producer: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub preprocess_version: u32,
    pub dimensions: u32,
    pub vector: InlineOrObject,
    pub metadata: Option<serde_json::Value>,
}

/// Plugin values keep the storage API's own types. Arbitrary plugin JSON is
/// never rewritten by the normalization rules that apply to control metadata.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalPluginValue {
    pub space: PluginSpace,
    #[serde(flatten)]
    pub value: PluginValue,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PluginValue {
    #[serde(rename = "value")]
    Inline(serde_json::Value),
    #[serde(rename = "object")]
    Object(ObjectReference),
    #[serde(rename = "objects")]
    Objects(Vec<ObjectReference>),
    /// A JSON-space value in structure form: its text and the byte parts the
    /// text names, in order.
    #[serde(rename = "structured")]
    Structured(StructuredPluginValue),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredPluginValue {
    pub text: StructureText,
    pub parts: Vec<ObjectReference>,
}

/// Structure text rides inline while it is small, then as one object, then as
/// ordered parts, like string-space text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StructureText {
    #[serde(rename = "value")]
    Inline(String),
    #[serde(rename = "object")]
    Object(ObjectReference),
    #[serde(rename = "objects")]
    Objects(Vec<ObjectReference>),
}

impl StructureText {
    pub fn references(&self) -> &[ObjectReference] {
        match self {
            Self::Inline(_) => &[],
            Self::Object(reference) => std::slice::from_ref(reference),
            Self::Objects(references) => references,
        }
    }
}

impl StructuredPluginValue {
    fn validate(&self) -> Result<()> {
        // Inline text is never larger than the limit, and an object never smaller.
        let mut total = match &self.text {
            StructureText::Inline(text) if text.len() <= MAX_INLINE_VALUE_BYTES => text.len() as u64,
            StructureText::Object(reference) if reference.byte_length > MAX_INLINE_VALUE_BYTES as u64 => 0,
            StructureText::Objects(references) if references.len() >= 2 => 0,
            _ => return Err(FormatError("invalid-section-value")),
        };
        for reference in self.text.references().iter().chain(&self.parts) {
            reference.validate()?;
            total = total.checked_add(reference.byte_length).ok_or(FormatError("invalid-section-value"))?;
        }
        if total > MAX_PLUGIN_VALUE_BYTES { return Err(FormatError("invalid-section-value")); }
        Ok(())
    }
}

impl From<ValueOrObject> for PluginValue {
    fn from(value: ValueOrObject) -> Self {
        match value { ValueOrObject::Inline(value) => Self::Inline(value), ValueOrObject::Object(value) => Self::Object(value) }
    }
}

impl PluginValue {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Inline(_) => Ok(()),
            Self::Object(reference) => {
                if reference.byte_length <= MAX_INLINE_VALUE_BYTES as u64 { return Err(FormatError("invalid-section-value")); }
                reference.validate()
            }
            Self::Objects(references) => {
                if references.len() < 2 { return Err(FormatError("invalid-section-value")); }
                let mut total = 2u64;
                for reference in references {
                    reference.validate()?;
                    total = total.checked_add(reference.byte_length).ok_or(FormatError("invalid-section-value"))?;
                }
                if total > MAX_PLUGIN_VALUE_BYTES { return Err(FormatError("invalid-section-value")); }
                Ok(())
            }
            Self::Structured(value) => value.validate(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalSettingValue {
    #[serde(flatten)]
    pub value: ValueOrObject,
}

/// A removal carries the commit it first reached a remote in and when that
/// happened, so every device judges its age the same way. The marker means
/// nothing outside the remote lineage that issued the commit number.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SectionValue {
    #[serde(rename = "hypa")]
    Hypa(HypaValue),
    #[serde(rename = "localPlugin")]
    LocalPlugin(LocalPluginValue),
    #[serde(rename = "localSetting")]
    LocalSetting(LocalSettingValue),
    #[serde(rename = "tombstone")]
    Tombstone {
        first_published_generation: Sequence,
        #[serde(with = "crate::control_integer")]
        first_published_at_ms: u64,
    },
}

impl SectionValue {
    pub fn kind(&self) -> Option<SectionKind> {
        match self {
            Self::Hypa(_) => Some(SectionKind::Hypa),
            Self::LocalPlugin(_) => Some(SectionKind::LocalPlugins),
            Self::LocalSetting(_) => Some(SectionKind::LocalSettings),
            Self::Tombstone { .. } => None,
        }
    }
    pub fn tombstone(first_published_generation: Sequence, first_published_at_ms: u64) -> Self {
        Self::Tombstone {
            first_published_generation,
            first_published_at_ms,
        }
    }
    /// The object this value names instead of carrying its bytes inline.
    pub fn object_reference(&self) -> Option<&ObjectReference> {
        match self {
            Self::Hypa(HypaValue { vector: InlineOrObject::Object(reference), .. })
            | Self::LocalSetting(LocalSettingValue { value: ValueOrObject::Object(reference) }) => {
                Some(reference)
            }
            Self::LocalPlugin(LocalPluginValue { value: PluginValue::Object(reference), .. }) => Some(reference),
            _ => None,
        }
    }
    /// Every object this value names. A structured value lists its text before
    /// its parts.
    pub fn object_references(&self) -> impl Iterator<Item = &ObjectReference> + '_ {
        let (text, parts): (&[ObjectReference], &[ObjectReference]) = match self {
            Self::LocalPlugin(LocalPluginValue { value: PluginValue::Objects(references), .. }) => (references, &[]),
            Self::LocalPlugin(LocalPluginValue { value: PluginValue::Structured(value), .. }) => (value.text.references(), &value.parts),
            _ => (self.object_reference().map(std::slice::from_ref).unwrap_or_default(), &[]),
        };
        text.iter().chain(parts)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SectionEntryVersion {
    pub write_clock: Sequence,
    pub writer_id: String,
}

/// A backup bundle captures user values only, so `version` is absent there. A
/// synchronized section always carries one.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SectionEntry {
    pub codec: String,
    pub kind: SectionKind,
    pub key: String,
    pub value: SectionValue,
    pub version: Option<SectionEntryVersion>,
}

impl SectionEntry {
    pub fn new(
        kind: SectionKind,
        key: String,
        value: SectionValue,
        version: Option<SectionEntryVersion>,
    ) -> Result<Self> {
        let entry = Self {
            codec: SECTION_CODEC.into(),
            kind,
            key,
            value,
            version,
        };
        entry.validate()?;
        Ok(entry)
    }
    pub fn validate(&self) -> Result<()> {
        if self.codec != SECTION_CODEC {
            return Err(FormatError("unsupported-section-codec"));
        }
        match self.kind {
            SectionKind::Hypa => hypa_entry_key(&self.key)?,
            SectionKind::LocalPlugins => {
                decode_local_plugin_entry_key(&self.key)?;
                self.key.clone()
            }
            SectionKind::LocalSettings => {
                if self.key.is_empty() || self.key.len() > 64 * 1024 {
                    return Err(FormatError("invalid-section-key"));
                }
                self.key.clone()
            }
        };
        if self
            .value
            .kind()
            .is_some_and(|kind| kind != self.kind)
        {
            return Err(FormatError("section-value-kind-mismatch"));
        }
        match &self.value {
            SectionValue::Hypa(value) => {
                if value.producer.is_empty()
                    || value.model.is_empty()
                    || value.dimensions == 0
                    || value.dimensions > MAX_VECTOR_DIMENSIONS
                    || value
                        .endpoint
                        .as_ref()
                        .is_some_and(|endpoint| endpoint.is_empty())
                {
                    return Err(FormatError("invalid-section-value"));
                }
                value.vector.validate()?;
                let byte_length = match &value.vector {
                    InlineOrObject::Inline(_) => value.vector.decode_inline()?.len() as u64,
                    InlineOrObject::Object(reference) => reference.byte_length,
                };
                if byte_length != u64::from(value.dimensions) * 4 {
                    return Err(FormatError("invalid-section-vector-length"));
                }
            }
            SectionValue::LocalPlugin(value) => {
                let (_, space, _) = decode_local_plugin_entry_key(&self.key)?;
                let declared = match value.space {
                    PluginSpace::String => "string",
                    PluginSpace::Json => "json",
                };
                if space != declared {
                    return Err(FormatError("section-plugin-space-mismatch"));
                }
                value.value.validate()?;
                // Structure form exists only in the JSON space.
                if matches!(value.space, PluginSpace::String)
                    && match &value.value {
                        PluginValue::Inline(inline) => !inline.is_string(),
                        PluginValue::Structured(_) => true,
                        PluginValue::Object(_) | PluginValue::Objects(_) => false,
                    }
                {
                    return Err(FormatError("invalid-section-value"));
                }
            }
            // A removal is only ever published as part of a commit, so a
            // marker naming no commit or no time is not a removal this format
            // can carry.
            SectionValue::Tombstone {
                first_published_generation,
                first_published_at_ms,
            } => {
                if *first_published_generation == Sequence::from(0u64)
                    || *first_published_at_ms == 0
                    || *first_published_at_ms > i64::MAX as u64
                {
                    return Err(FormatError("invalid-section-value"));
                }
            }
            SectionValue::LocalSetting(value) => value.value.validate()?,
        }
        if self
            .version
            .as_ref()
            .is_some_and(|version| version.writer_id.is_empty() || version.writer_id.len() > 1024)
        {
            return Err(FormatError("invalid-section-version"));
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| FormatError("invalid-section-entry"))?;
        if bytes.len() > MAX_SECTION_ENTRY_BYTES {
            return Err(FormatError("section-entry-limit-exceeded"));
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_SECTION_ENTRY_BYTES {
            return Err(FormatError("section-entry-limit-exceeded"));
        }
        let entry: Self =
            serde_json::from_slice(bytes).map_err(|_| FormatError("invalid-section-entry"))?;
        entry.validate()?;
        if entry.encode()? != bytes {
            return Err(FormatError("non-canonical-section-entry"));
        }
        Ok(entry)
    }
    /// `decode`, except that a local plugin entry whose value has a form this
    /// build does not know comes back as [`UnknownPluginEntry`].
    pub fn decode_known(bytes: &[u8]) -> Result<std::result::Result<Self, UnknownPluginEntry>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            codec: String,
            kind: SectionKind,
            key: String,
            value: EnvelopeValue,
            version: Option<SectionEntryVersion>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        enum EnvelopeValue {
            #[serde(rename = "localPlugin")]
            LocalPlugin(serde_json::Map<String, serde_json::Value>),
        }
        let error = match Self::decode(bytes) {
            Ok(entry) => return Ok(Ok(entry)),
            Err(error) => error,
        };
        if bytes.is_empty() || bytes.len() > MAX_SECTION_ENTRY_BYTES { return Err(error); }
        let Ok(Envelope { codec, kind, key, value: EnvelopeValue::LocalPlugin(value), version }) = serde_json::from_slice(bytes) else {
            return Err(error);
        };
        let (_, space, _) = decode_local_plugin_entry_key(&key).map_err(|_| error)?;
        let unknown = value.len() == 2
            && value.get("space").and_then(serde_json::Value::as_str) == Some(space.as_str())
            && value.keys().all(|name| name == "space" || !["value", "object", "objects", "structured"].contains(&name.as_str()));
        if codec != SECTION_CODEC || kind != SectionKind::LocalPlugins || !unknown
            || version.as_ref().is_some_and(|version| version.writer_id.is_empty() || version.writer_id.len() > 1024)
        {
            return Err(error);
        }
        Ok(Err(UnknownPluginEntry { key, version }))
    }
}

/// A local plugin entry in a value form that a later build writes. Readers
/// skip it instead of failing the section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownPluginEntry {
    pub key: String,
    pub version: Option<SectionEntryVersion>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_objects_and_vectors_have_practical_limits() {
        assert!(InlineOrObject::Object(ObjectReference {
            content_sha256: [1;32], byte_length: MAX_SECTION_OBJECT_BYTES as u64 + 1,
        }).validate().is_err());
        let mut entry = hypa_entry(&"a".repeat(64));
        let SectionValue::Hypa(value) = &mut entry.value else { unreachable!() };
        value.dimensions = MAX_VECTOR_DIMENSIONS;
        value.vector = InlineOrObject::Object(ObjectReference {
            content_sha256: [1;32], byte_length: u64::from(MAX_VECTOR_DIMENSIONS)*4,
        });
        entry.validate().unwrap();
        let SectionValue::Hypa(value) = &mut entry.value else { unreachable!() };
        value.dimensions += 1;
        value.vector = InlineOrObject::Object(ObjectReference {
            content_sha256: [1;32], byte_length: u64::from(value.dimensions)*4,
        });
        assert!(entry.validate().is_err());
    }

    use std::collections::BTreeMap;

    fn version(clock: u64) -> Option<SectionEntryVersion> {
        Some(SectionEntryVersion {
            write_clock: Sequence::from(clock),
            writer_id: "writer-a".into(),
        })
    }

    fn hypa_entry(cache_key: &str) -> SectionEntry {
        SectionEntry::new(
            SectionKind::Hypa,
            hypa_entry_key(cache_key).unwrap(),
            SectionValue::Hypa(HypaValue {
                producer: "hypa-v2".into(),
                model: "text-embedding".into(),
                endpoint: None,
                preprocess_version: 1,
                dimensions: 4,
                vector: InlineOrObject::inline(&[0u8; 16]).unwrap(),
                metadata: None,
            }),
            version(7),
        )
        .unwrap()
    }

    #[test]
    fn section_ids_keys_counters_and_empty_sections_round_trip() {
        let cache_key = "a".repeat(64);
        let entries = [
            hypa_entry(&cache_key),
            SectionEntry::new(
                SectionKind::LocalPlugins,
                local_plugin_entry_key("provider-manager", "json", "settings").unwrap(),
                SectionValue::LocalPlugin(LocalPluginValue {
                    space: PluginSpace::Json,
                    value: PluginValue::Inline(serde_json::json!({ "b": 1, "a": [2, 3] })),
                }),
                version(9),
            )
            .unwrap(),
            SectionEntry::new(
                SectionKind::LocalPlugins,
                local_plugin_entry_key("yumi-translator", "string", "token").unwrap(),
                SectionValue::LocalPlugin(LocalPluginValue {
                    space: PluginSpace::String,
                    value: PluginValue::Inline(serde_json::Value::String("kept".into())),
                }),
                None,
            )
            .unwrap(),
            SectionEntry::new(
                SectionKind::Hypa,
                hypa_entry_key(&"b".repeat(64)).unwrap(),
                SectionValue::tombstone(Sequence::from(3u64), 1_760_000_000_000),
                version(10),
            )
            .unwrap(),
        ];
        for entry in &entries {
            let encoded = entry.encode().unwrap();
            assert_eq!(&SectionEntry::decode(&encoded).unwrap(), entry);
        }
        let text = "part-one가😀part-two";
        let parts = text.as_bytes().chunks(7).map(|bytes| ObjectReference {
            content_sha256: crate::content_identity::hash(bytes), byte_length: bytes.len() as u64,
        }).collect::<Vec<_>>();
        let multipart = SectionEntry::new(SectionKind::LocalPlugins,
            local_plugin_entry_key("owner", "string", "parts").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue { space: PluginSpace::String, value: PluginValue::Objects(parts.clone()) }), None).unwrap();
        assert_eq!(SectionEntry::decode(&multipart.encode().unwrap()).unwrap(), multipart);
        assert!(multipart.value.object_references().eq(&parts));
        for invalid_parts in [vec![], vec![parts[0].clone()], vec![ObjectReference { content_sha256: [1;32], byte_length: MAX_SECTION_OBJECT_BYTES as u64 + 1 }; 2]] {
            assert!(SectionEntry::new(SectionKind::LocalPlugins, multipart.key.clone(),
                SectionValue::LocalPlugin(LocalPluginValue { space: PluginSpace::String, value: PluginValue::Objects(invalid_parts) }), None).is_err());
        }
        let part = ObjectReference { content_sha256: [7; 32], byte_length: MAX_SECTION_OBJECT_BYTES as u64 };
        let text = ObjectReference { content_sha256: [8; 32], byte_length: MAX_INLINE_VALUE_BYTES as u64 + 1 };
        let structured = |space: PluginSpace, text: StructureText, parts: Vec<ObjectReference>| SectionEntry::new(SectionKind::LocalPlugins,
            local_plugin_entry_key("owner", if matches!(space, PluginSpace::Json) { "json" } else { "string" }, "structured").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue { space, value: PluginValue::Structured(StructuredPluginValue { text, parts }) }), None);
        let entry = structured(PluginSpace::Json, StructureText::Object(text.clone()), vec![part.clone(), part.clone()]).unwrap();
        let encoded = entry.encode().unwrap();
        assert_eq!(SectionEntry::decode(&encoded).unwrap(), entry);
        assert!(std::str::from_utf8(&encoded).unwrap().contains(r#""localPlugin":{"space":"json","structured":{"text":{"object":"#));
        assert!(entry.value.object_references().eq([&text, &part, &part]));
        structured(PluginSpace::Json, StructureText::Inline("#risunest-structured:1\n{}".into()), vec![]).unwrap();
        for (space, text, parts) in [
            (PluginSpace::String, StructureText::Inline("text".into()), vec![]),
            (PluginSpace::Json, StructureText::Inline("x".repeat(MAX_INLINE_VALUE_BYTES + 1)), vec![]),
            (PluginSpace::Json, StructureText::Object(ObjectReference { byte_length: 1, ..text.clone() }), vec![]),
            (PluginSpace::Json, StructureText::Objects(vec![text.clone()]), vec![]),
            (PluginSpace::Json, StructureText::Object(text.clone()), vec![ObjectReference { byte_length: 0, ..part.clone() }]),
            (PluginSpace::Json, StructureText::Object(text.clone()), vec![part.clone(); 64]),
        ] {
            assert!(structured(space, text, parts).is_err());
        }
        // An empty section is a real, publishable state. It differs from a
        // section that was never published, which carries no reference at all.
        let empty: BTreeMap<String, [u8; 32]> = BTreeMap::new();
        assert_ne!(
            crate::format::fingerprint(&SectionKind::Hypa.fingerprint_domain(), &empty),
            crate::format::fingerprint(&SectionKind::LocalPlugins.fingerprint_domain(), &empty)
        );
    }

    #[test]
    fn plugin_keys_are_structured_rather_than_concatenated() {
        let ambiguous = local_plugin_entry_key("a", "b", "c:d").unwrap();
        let other = local_plugin_entry_key("a", "b:c", "d").unwrap();
        assert_ne!(ambiguous, other);
        assert_eq!(
            decode_local_plugin_entry_key(&ambiguous).unwrap(),
            ("a".into(), "b".into(), "c:d".into())
        );
        assert!(decode_local_plugin_entry_key("[\"a\",\"b\"]").is_err());
        assert!(decode_local_plugin_entry_key("[\"a\", \"b\", \"c\"]").is_err());
        assert!(hypa_entry_key("A".repeat(64).as_str()).is_err());
        assert!(hypa_entry_key("abc").is_err());
    }

    #[test]
    fn arbitrary_plugin_json_keeps_its_own_shape_and_typed_spaces_are_enforced() {
        let value = serde_json::json!({ "zeta": 1, "alpha": { "nested": [1, 2] } });
        let entry = SectionEntry::new(
            SectionKind::LocalPlugins,
            local_plugin_entry_key("owner", "json", "key").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue {
                space: PluginSpace::Json,
                value: PluginValue::Inline(value.clone()),
            }),
            None,
        )
        .unwrap();
        let decoded = SectionEntry::decode(&entry.encode().unwrap()).unwrap();
        let SectionValue::LocalPlugin(restored) = decoded.value else {
            panic!("plugin value changed kind");
        };
        assert_eq!(restored.value, PluginValue::Inline(value.clone()));
        let PluginValue::Inline(restored) = restored.value else { unreachable!() };
        assert_eq!(
            serde_json::to_string(&restored).unwrap(),
            serde_json::to_string(&value).unwrap()
        );
        assert!(SectionEntry::new(
            SectionKind::LocalPlugins,
            local_plugin_entry_key("owner", "string", "key").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue {
                space: PluginSpace::String,
                value: PluginValue::Inline(serde_json::json!(5)),
            }),
            None,
        )
        .is_err());
    }

    #[test]
    fn a_value_over_the_inline_limit_becomes_an_object_reference() {
        assert!(InlineOrObject::inline(&vec![0u8; MAX_INLINE_VALUE_BYTES]).is_ok());
        assert_eq!(
            InlineOrObject::inline(&vec![0u8; MAX_INLINE_VALUE_BYTES + 1]),
            Err(FormatError("section-value-requires-object"))
        );
        let reference = InlineOrObject::Object(ObjectReference {
            content_sha256: [3; 32],
            byte_length: 1536 * 4,
        });
        assert!(reference.decode_inline().is_err());
        let entry = SectionEntry::new(
            SectionKind::Hypa,
            hypa_entry_key(&"c".repeat(64)).unwrap(),
            SectionValue::Hypa(HypaValue {
                producer: "hypa-v3-group".into(),
                model: "text-embedding".into(),
                endpoint: Some("https://synthetic.invalid/embed".into()),
                preprocess_version: 1,
                dimensions: 1536,
                vector: reference,
                metadata: Some(serde_json::json!({ "chunk": 2 })),
            }),
            version(11),
        )
        .unwrap();
        assert_eq!(
            SectionEntry::decode(&entry.encode().unwrap()).unwrap(),
            entry
        );
    }

    #[test]
    fn vector_lengths_plugin_spaces_and_removal_times_are_checked_before_apply() {
        let mut entry = hypa_entry(&"a".repeat(64));
        let SectionValue::Hypa(value) = &mut entry.value else { unreachable!() };
        value.dimensions += 1;
        assert_eq!(entry.validate(), Err(FormatError("invalid-section-vector-length")));
        let SectionValue::Hypa(value) = &mut entry.value else { unreachable!() };
        value.vector = InlineOrObject::Object(ObjectReference { content_sha256: [1; 32], byte_length: 16 });
        assert_eq!(entry.validate(), Err(FormatError("invalid-section-vector-length")));

        assert_eq!(SectionEntry::new(
            SectionKind::LocalPlugins,
            local_plugin_entry_key("owner", "json", "key").unwrap(),
            SectionValue::LocalPlugin(LocalPluginValue {
                space: PluginSpace::String, value: PluginValue::Inline(serde_json::Value::String("value".into())),
            }),
            version(1),
        ), Err(FormatError("section-plugin-space-mismatch")));
        assert_eq!(SectionEntry::new(
            SectionKind::Hypa, hypa_entry_key(&"a".repeat(64)).unwrap(),
            SectionValue::tombstone(super::Sequence::from(1u64), i64::MAX as u64 + 1), version(1),
        ), Err(FormatError("invalid-section-value")));
    }

    #[test]
    fn an_unknown_section_or_codec_is_reported_rather_than_narrowed() {
        assert_eq!(SectionKind::parse("device"), Err(FormatError("unknown-section")));
        assert_eq!(SectionKind::parse(""), Err(FormatError("unknown-section")));
        assert!(SectionKind::parse("hypa").unwrap().is_synchronizable());
        assert!(!SectionKind::parse("local-settings")
            .unwrap()
            .is_synchronizable());
        let mut entry = hypa_entry(&"d".repeat(64));
        entry.codec = "risunest.section-codec/v0".into();
        assert_eq!(entry.validate(), Err(FormatError("unsupported-section-codec")));
        let smuggled = serde_json::json!({
            "codec": SECTION_CODEC,
            "kind": "hypa",
            "key": "e".repeat(64),
            "value": { "tombstone": {
                "firstPublishedGeneration": "3",
                "firstPublishedAtMs": 1_760_000_000_000u64,
            } },
            "version": null,
            "extra": 1,
        });
        assert!(SectionEntry::decode(&serde_json::to_vec(&smuggled).unwrap()).is_err());
        assert!(SectionEntry::decode_known(&serde_json::to_vec(&smuggled).unwrap()).is_err());
        // A plugin value form from a later build is skipped, never guessed at;
        // a malformed known form is still an error.
        let key = local_plugin_entry_key("owner", "json", "later").unwrap();
        let plugin = |value: serde_json::Value| serde_json::to_vec(&serde_json::json!({
            "codec": SECTION_CODEC, "kind": "local-plugins", "key": key, "value": { "localPlugin": value }, "version": null,
        })).unwrap();
        assert_eq!(SectionEntry::decode_known(&plugin(serde_json::json!({ "space": "json", "later": [1] }))).unwrap(),
            Err(UnknownPluginEntry { key: key.clone(), version: None }));
        assert!(matches!(SectionEntry::decode_known(&plugin(serde_json::json!({ "space": "json", "value": 1 }))).unwrap(), Ok(_)));
        for invalid in [
            serde_json::json!({ "space": "json", "structured": 1 }),
            serde_json::json!({ "space": "string", "later": 1 }),
            serde_json::json!({ "space": "json", "later": 1, "other": 1 }),
            serde_json::json!({ "later": 1 }),
        ] {
            assert!(SectionEntry::decode_known(&plugin(invalid.clone())).is_err(), "{invalid}");
        }
    }

    /// A removal always names the commit it first reached a remote in. A
    /// marker naming none is not a shape this format accepts, in either
    /// direction.
    #[test]
    fn a_removal_carries_the_commit_it_was_first_published_in() {
        let entry = SectionEntry::new(
            SectionKind::Hypa,
            hypa_entry_key(&"1".repeat(64)).unwrap(),
            SectionValue::tombstone(Sequence::from(12u64), 1_760_000_000_000),
            version(10),
        )
        .unwrap();
        let encoded = entry.encode().unwrap();
        assert!(String::from_utf8_lossy(&encoded).contains(
            "\"value\":{\"tombstone\":{\"firstPublishedGeneration\":\"12\",\"firstPublishedAtMs\":\"1760000000000\"}}"
        ));
        assert_eq!(SectionEntry::decode(&encoded).unwrap(), entry);
        assert_eq!(
            SectionEntry::new(
                SectionKind::Hypa,
                hypa_entry_key(&"2".repeat(64)).unwrap(),
                SectionValue::tombstone(Sequence::from(0u64), 1_760_000_000_000),
                version(10),
            ),
            Err(FormatError("invalid-section-value"))
        );
        assert_eq!(
            SectionEntry::new(
                SectionKind::Hypa,
                hypa_entry_key(&"3".repeat(64)).unwrap(),
                SectionValue::tombstone(Sequence::from(12u64), 0),
                version(10),
            ),
            Err(FormatError("invalid-section-value"))
        );
        let unmarked = serde_json::json!({
            "codec": SECTION_CODEC,
            "kind": "hypa",
            "key": "4".repeat(64),
            "value": { "tombstone": {} },
            "version": null,
        });
        assert!(SectionEntry::decode(&serde_json::to_vec(&unmarked).unwrap()).is_err());
    }

    #[test]
    fn a_value_cannot_claim_a_different_section_than_its_entry() {
        assert_eq!(
            SectionEntry::new(
                SectionKind::Hypa,
                hypa_entry_key(&"f".repeat(64)).unwrap(),
                SectionValue::LocalPlugin(LocalPluginValue {
                    space: PluginSpace::Json,
                    value: PluginValue::Inline(serde_json::json!(1)),
                }),
                None,
            ),
            Err(FormatError("section-value-kind-mismatch"))
        );
    }

    fn device_entry(kind: SectionKind, space: Option<PluginSpace>, value: ValueOrObject) -> Result<SectionEntry> {
        match space {
            Some(space) => {
                let id = if space == PluginSpace::Json { "json" } else { "string" };
                SectionEntry::new(
                    kind,
                    local_plugin_entry_key("owner", id, "key").unwrap(),
                    SectionValue::LocalPlugin(LocalPluginValue { space, value: value.into() }),
                    version(3),
                )
            }
            None => SectionEntry::new(
                kind,
                "risuNestDeviceSettings".into(),
                SectionValue::LocalSetting(LocalSettingValue { value }),
                None,
            ),
        }
    }

    fn object(byte_length: u64) -> ValueOrObject {
        ValueOrObject::Object(ObjectReference { content_sha256: [4; 32], byte_length })
    }

    /// Inline device values keep the exact bytes they always had. An object
    /// takes the value's place under its own field and names the same kind of
    /// reference a vector uses.
    #[test]
    fn a_device_value_is_inline_json_or_one_object_reference() {
        let inline = device_entry(
            SectionKind::LocalPlugins,
            Some(PluginSpace::Json),
            ValueOrObject::Inline(serde_json::Value::Null),
        )
        .unwrap();
        let encoded = String::from_utf8(inline.encode().unwrap()).unwrap();
        assert!(encoded.contains("\"value\":{\"localPlugin\":{\"space\":\"json\",\"value\":null}}"));
        assert_eq!(SectionEntry::decode(encoded.as_bytes()).unwrap(), inline);
        let setting = device_entry(
            SectionKind::LocalSettings,
            None,
            ValueOrObject::Inline(serde_json::json!({ "b": 1, "a": 2 })),
        )
        .unwrap();
        assert!(String::from_utf8(setting.encode().unwrap())
            .unwrap()
            .contains("\"value\":{\"localSetting\":{\"value\":{\"b\":1,\"a\":2}}}"));

        let length = MAX_INLINE_VALUE_BYTES as u64 + 1;
        for (kind, space, prefix) in [
            (SectionKind::LocalPlugins, Some(PluginSpace::String), "{\"localPlugin\":{\"space\":\"string\",\"object\":"),
            (SectionKind::LocalPlugins, Some(PluginSpace::Json), "{\"localPlugin\":{\"space\":\"json\",\"object\":"),
            (SectionKind::LocalSettings, None, "{\"localSetting\":{\"object\":"),
        ] {
            let entry = device_entry(kind, space, object(length)).unwrap();
            let encoded = String::from_utf8(entry.encode().unwrap()).unwrap();
            assert!(encoded.contains(&format!(
                "\"value\":{prefix}{{\"contentSha256\":[{}],\"byteLength\":\"{length}\"}}}}}}",
                ["4"; 32].join(",")
            )), "{encoded}");
            assert_eq!(SectionEntry::decode(encoded.as_bytes()).unwrap(), entry);
            assert_eq!(
                entry.value.object_reference(),
                Some(&ObjectReference { content_sha256: [4; 32], byte_length: length })
            );
            assert_eq!(device_entry(kind, space, object(length - 1)), Err(FormatError("invalid-section-value")));
            assert_eq!(
                device_entry(kind, space, object(MAX_SECTION_OBJECT_BYTES as u64 + 1)),
                Err(FormatError("invalid-section-value"))
            );
            assert!(device_entry(kind, space, object(MAX_SECTION_OBJECT_BYTES as u64)).is_ok());
        }
        assert_eq!(inline.value.object_reference(), None);
        assert_eq!(hypa_entry(&"a".repeat(64)).value.object_reference(), None);
        assert_eq!(
            SectionValue::tombstone(Sequence::from(1u64), 1).object_reference(),
            None
        );
    }

    #[test]
    fn a_device_value_names_exactly_one_inline_value_or_object() {
        let digest = [4u8; 32];
        let reference = serde_json::json!({ "contentSha256": digest, "byteLength": "5000" });
        let plugin = |value: serde_json::Value| serde_json::json!({
            "codec": SECTION_CODEC,
            "kind": "local-plugins",
            "key": local_plugin_entry_key("owner", "json", "key").unwrap(),
            "value": { "localPlugin": value },
            "version": null,
        });
        let setting = |value: serde_json::Value| serde_json::json!({
            "codec": SECTION_CODEC,
            "kind": "local-settings",
            "key": "risuNestDeviceSettings",
            "value": { "localSetting": value },
            "version": null,
        });
        for valid in [
            plugin(serde_json::json!({ "space": "json", "value": null })),
            plugin(serde_json::json!({ "space": "json", "object": reference.clone() })),
            setting(serde_json::json!({ "object": reference.clone() })),
        ] {
            SectionEntry::decode(&serde_json::to_vec(&valid).unwrap()).unwrap();
            serde_json::from_value::<SectionEntry>(valid).unwrap();
        }
        for invalid in [
            plugin(serde_json::json!({ "space": "json", "value": 1, "object": reference.clone() })),
            plugin(serde_json::json!({ "space": "json", "object": reference.clone(), "value": 1 })),
            plugin(serde_json::json!({ "space": "json" })),
            plugin(serde_json::json!({ "space": "json", "value": 1, "extra": 1 })),
            setting(serde_json::json!({ "value": 1, "object": reference.clone() })),
            setting(serde_json::json!({})),
            setting(serde_json::json!({ "object": reference.clone(), "extra": 1 })),
            plugin(serde_json::json!({ "space": "string", "value": 1 })),
        ] {
            assert!(SectionEntry::decode(&serde_json::to_vec(&invalid).unwrap()).is_err(), "{invalid}");
            assert!(
                serde_json::from_value::<SectionEntry>(invalid.clone())
                    .map_err(|_| FormatError("invalid-section-entry"))
                    .and_then(|entry| entry.validate())
                    .is_err(),
                "{invalid}"
            );
        }
    }
}
