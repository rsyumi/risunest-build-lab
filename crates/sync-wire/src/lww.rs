use crate::{
    canonical, hash,
    stamp::{validate_writer_id, DecimalU64, Stamp},
    unit::{UnitKey, UnitValue},
    validate_hash, validate_id, Result, WireError,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDeviceClaimRequest {
    pub writer_id: String,
    pub authorization_id: String,
    pub former_token: Option<String>,
}
impl NewDeviceClaimRequest {
    pub fn validate(&self) -> Result<()> {
        validate_writer_id(&self.writer_id)?;
        validate_id(&self.authorization_id)?;
        if self.former_token.as_ref().is_some_and(|token| {
            token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(WireError("invalid-former-token"));
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        Ok(hash(&canonical::encode(self)?))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDeviceClaimReceipt {
    pub authorization_id: String,
    pub writer_id: String,
    pub device_id: String,
    pub library_id: String,
    pub epoch: String,
    /// A supplied former token cannot authenticate this library after the claim.
    pub former_credential_inactive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDeviceClaimStatus {
    pub request_digest: String,
    pub receipt: NewDeviceClaimReceipt,
}

/// A registration's claim state, which a join reads before it reads the library.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDeviceClaimState {
    pub claim: Option<NewDeviceClaimStatus>,
    /// A writer or operation already belongs to this registration, so it cannot claim a writer.
    pub used: bool,
}

/// The writer a join that keeps its installation's writer binds to the registration before it
/// reads the library. A registration takes one writer, so another installation cannot join with it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriterBindingRequest {
    pub writer_id: String,
}
impl WriterBindingRequest {
    pub fn validate(&self) -> Result<()> {
        validate_writer_id(&self.writer_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitChange {
    pub key: UnitKey,
    pub stamp: Stamp,
    pub value: UnitValue,
}
impl UnitChange {
    pub fn validate(&self) -> Result<()> {
        self.stamp.validate()?;
        self.value.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushRequest {
    pub library_id: String,
    /// Authenticated publisher identity; unit stamps retain their original issuers.
    pub writer_id: String,
    pub operation_id: String,
    pub changes: Vec<UnitChange>,
}
impl PushRequest {
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.library_id)?;
        validate_id(&self.operation_id)?;
        validate_writer_id(&self.writer_id)?;
        if self.changes.is_empty() {
            return Err(WireError("empty-push"));
        }
        let mut keys = BTreeSet::new();
        for change in &self.changes {
            change.validate()?;
            if !keys.insert(change.key.as_str()) {
                return Err(WireError("duplicate-unit-key"));
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        Ok(hash(&canonical::encode(self)?))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushReceipt {
    pub operation_id: String,
    pub seq: DecimalU64,
    pub accepted_keys: Vec<UnitKey>,
    pub server_time_ms: DecimalU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum OperationReceipt {
    Accepted {
        #[serde(rename = "bodyDigest")]
        body_digest: String,
        receipt: PushReceipt,
    },
    Rejected {
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "bodyDigest")]
        body_digest: String,
        error: String,
        #[serde(rename = "serverTimeMs")]
        server_time_ms: DecimalU64,
    },
}
impl OperationReceipt {
    pub fn body_digest(&self) -> &str {
        match self {
            Self::Accepted { body_digest, .. } | Self::Rejected { body_digest, .. } => body_digest,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelOperationRequest {
    pub body_digest: String,
}
impl CancelOperationRequest {
    pub fn validate(&self) -> Result<()> {
        validate_hash(&self.body_digest)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalItem {
    pub seq: DecimalU64,
    pub key: UnitKey,
    pub stamp: Stamp,
    pub value: UnitValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangesPage {
    pub through_seq: DecimalU64,
    pub journal_floor: DecimalU64,
    pub items: Vec<JournalItem>,
    pub next_after: DecimalU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalFloor {
    pub error: String,
    pub journal_floor: DecimalU64,
    pub latest_seq: DecimalU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatePin {
    pub pin_id: String,
    pub start_seq: DecimalU64,
    pub expires_at_ms: DecimalU64,
    /// Units the pin holds, so a reader knows how many state pages it will list.
    pub unit_count: DecimalU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatePage {
    pub pin_id: String,
    pub start_seq: DecimalU64,
    pub items: Vec<UnitChange>,
    pub next_key: Option<UnitKey>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AckRequest {
    pub seq: DecimalU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimeSample {
    pub server_time_ms: DecimalU64,
    pub precision_ms: DecimalU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum SeqNotification {
    Seq { seq: DecimalU64 },
}
