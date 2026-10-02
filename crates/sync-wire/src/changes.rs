use crate::{validate_hash, validate_id, Result};
use serde::{Deserialize, Serialize};

pub const MAX_KEY_BYTES: usize = 64 * 1024;
pub const MAX_METADATA_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum RecordVersion {
    Absent,
    Live {
        #[serde(rename = "objectHash")]
        object_hash: String,
        #[serde(rename = "descriptorHash")]
        descriptor_hash: Option<String>,
    },
    Tombstone {
        #[serde(rename = "deletionId")]
        deletion_id: String,
    },
}
// Serde's internally tagged *unit* variant ignores surplus fields even when the
// enum denies unknown fields. Deserialize absent through an empty struct variant.
impl<'de> Deserialize<'de> for RecordVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
        enum Input {
            Absent {},
            Live {
                #[serde(rename = "objectHash")]
                object_hash: String,
                #[serde(rename = "descriptorHash")]
                descriptor_hash: Option<String>,
            },
            Tombstone {
                #[serde(rename = "deletionId")]
                deletion_id: String,
            },
        }
        Ok(match Input::deserialize(d)? {
            Input::Absent {} => Self::Absent,
            Input::Live {
                object_hash,
                descriptor_hash,
            } => Self::Live {
                object_hash,
                descriptor_hash,
            },
            Input::Tombstone { deletion_id } => Self::Tombstone { deletion_id },
        })
    }
}
impl RecordVersion {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Absent => Ok(()),
            Self::Tombstone { deletion_id } => validate_id(deletion_id),
            Self::Live {
                object_hash,
                descriptor_hash,
            } => {
                validate_hash(object_hash)?;
                if let Some(hash) = descriptor_hash {
                    validate_hash(hash)?;
                }
                Ok(())
            }
        }
    }
    pub fn object_hashes(&self) -> Vec<&str> {
        match self {
            Self::Live {
                object_hash,
                descriptor_hash,
            } => std::iter::once(object_hash.as_str())
                .chain(descriptor_hash.iter().map(String::as_str))
                .collect(),
            _ => Vec::new(),
        }
    }
}
