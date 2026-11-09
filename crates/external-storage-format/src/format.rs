use super::{content_identity::hash, FormatError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "risunest.external-storage/v1";

/// The library and its referenced assets are the whole content of every
/// repository, so their fingerprints are separated by one fixed domain rather
/// than by a per-repository selection. What a device publishes beyond them is
/// decided per device and carried in the published state.
pub fn library_fingerprint_domain() -> [u8; 32] {
    hash(b"risunest.external-library-fingerprint/v1")
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    Cas,
    Sequential,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Descriptor {
    pub schema: String,
    pub repository_id: String,
    pub encrypted: bool,
    pub publication_strategy: Option<Strategy>,
    /// The library format level of a repository used for sync. Absent means
    /// level 1, and such a descriptor encodes exactly as before levels existed,
    /// while builds that predate levels cannot decode one that carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_level: Option<u64>,
}
impl Descriptor {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 * 1024 {
            return Err(FormatError("invalid-descriptor"));
        }
        let descriptor: Self =
            serde_json::from_slice(bytes).map_err(|_| FormatError("invalid-descriptor"))?;
        descriptor.validate()?;
        Ok(descriptor)
    }
    /// The format level a descriptor declares, read without the strict
    /// decode, so a later level that adds fields still reports its level.
    pub fn declared_format_level(bytes: &[u8]) -> Option<u64> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        value.get("formatLevel")?.as_u64()
    }
    /// This descriptor at `level`.
    pub fn with_format_level(mut self, level: u64) -> Result<Self> {
        self.format_level = Some(level);
        self.validate()?;
        Ok(self)
    }
    /// Whether both name the same repository, whatever their levels.
    pub fn same_repository(&self, other: &Self) -> bool {
        Self { format_level: None, ..self.clone() } == Self { format_level: None, ..other.clone() }
    }
    /// The level, 1 for a descriptor without one.
    pub fn level(&self) -> u64 {
        self.format_level.unwrap_or(1)
    }
    pub fn new(repository_id: String, strategy: Option<Strategy>) -> Result<Self> {
        let descriptor = Self {
            schema: SCHEMA.into(),
            repository_id,
            encrypted: true,
            publication_strategy: strategy,
            format_level: None,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA
            || !self.encrypted
            || self.repository_id.is_empty()
            || self.repository_id.len() > 128
            || self.format_level.is_some_and(|level| {
                level < 2 || level > (1 << 53) - 1 || self.publication_strategy.is_none()
            })
        {
            return Err(FormatError("invalid-descriptor"));
        }
        Ok(())
    }
}
/// Fingerprints intentionally exclude packing, locators, nonces and timestamps.
pub fn fingerprint(domain: &[u8; 32], entries: &BTreeMap<String, [u8; 32]>) -> [u8; 32] {
    let mut digest = FingerprintBuilder::new(domain);
    for (key, content) in entries {
        digest
            .push(key, content)
            .expect("BTreeMap keys are unique and sorted");
    }
    digest.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encryption_is_required_for_every_repository() {
        for strategy in [None, Some(Strategy::Cas), Some(Strategy::Sequential)] {
            let mut descriptor = Descriptor::new("synthetic".into(), strategy).unwrap();
            descriptor.encrypted = false;
            assert!(Descriptor::decode(&serde_json::to_vec(&descriptor).unwrap()).is_err());
            assert!(descriptor.validate().is_err());
        }
    }
    #[test]
    fn the_library_fingerprint_domain_is_stable_and_separates_other_domains() {
        assert_eq!(library_fingerprint_domain(), library_fingerprint_domain());
        assert_ne!(library_fingerprint_domain(), [0; 32]);
    }
    #[test]
    fn descriptor_identity_no_longer_depends_on_a_published_selection() {
        let sync = Descriptor::new("repository".into(), Some(Strategy::Cas)).unwrap();
        let backup = Descriptor::new("repository".into(), None).unwrap();
        assert_ne!(sync, backup);
        assert_eq!(
            Descriptor::decode(&serde_json::to_vec(&sync).unwrap()).unwrap(),
            sync
        );
        // A device section selection is no longer part of repository identity,
        // so a strategy-carrying repository accepts every device's choice.
        assert!(Descriptor::new("repository".into(), Some(Strategy::Sequential)).is_ok());
    }
    #[test]
    fn a_format_level_is_written_only_when_set_and_read_tolerantly() {
        let plain = Descriptor::new("repository".into(), Some(Strategy::Cas)).unwrap();
        let bytes = serde_json::to_vec(&plain).unwrap();
        assert_eq!(
            bytes,
            br#"{"schema":"risunest.external-storage/v1","repositoryId":"repository","encrypted":true,"publicationStrategy":"cas"}"#
        );
        assert_eq!(Descriptor::declared_format_level(&bytes), None);
        let leveled = plain.clone().with_format_level(2).unwrap();
        let bytes = serde_json::to_vec(&leveled).unwrap();
        assert_eq!(Descriptor::decode(&bytes).unwrap(), leveled);
        assert!(leveled.same_repository(&plain) && leveled != plain);
        assert_eq!((leveled.level(), plain.level()), (2, 1));
        let later = br#"{"schema":"risunest.external-storage/v1","repositoryId":"repository","encrypted":true,"publicationStrategy":"cas","formatLevel":3,"next":true}"#;
        assert_eq!(Descriptor::declared_format_level(later), Some(3));
        assert!(Descriptor::decode(later).is_err());
        assert!(plain.clone().with_format_level(1).is_err());
        assert!(Descriptor::new("repository".into(), None).unwrap().with_format_level(2).is_err());
    }
}

pub struct FingerprintBuilder {
    digest: sha2::Sha256,
    previous: Option<String>,
}
impl FingerprintBuilder {
    pub fn new(domain: &[u8; 32]) -> Self {
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update(b"risunest.external-fingerprint/v1\0");
        digest.update(domain);
        Self {
            digest,
            previous: None,
        }
    }
    pub fn push(&mut self, key: &str, content: &[u8; 32]) -> super::Result<()> {
        use sha2::Digest;
        if self
            .previous
            .as_deref()
            .is_some_and(|previous| previous >= key)
        {
            return Err(super::FormatError("fingerprint-order"));
        }
        self.digest.update((key.len() as u64).to_le_bytes());
        self.digest.update(key.as_bytes());
        self.digest.update(content);
        self.previous = Some(key.into());
        Ok(())
    }
    pub fn finish(self) -> [u8; 32] {
        use sha2::Digest;
        self.digest.finalize().into()
    }
}
