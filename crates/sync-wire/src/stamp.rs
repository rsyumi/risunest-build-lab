use crate::{Result, WireError};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DecimalU64(pub u64);
impl TryFrom<String> for DecimalU64 {
    type Error = WireError;
    fn try_from(value: String) -> Result<Self> {
        if value.is_empty() || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(WireError("invalid-decimal-u64"));
        }
        value.parse().map(Self).map_err(|_| WireError("invalid-decimal-u64"))
    }
}
impl From<DecimalU64> for String {
    fn from(value: DecimalU64) -> Self { value.0.to_string() }
}
impl From<u64> for DecimalU64 {
    fn from(value: u64) -> Self { Self(value) }
}
mod logical_decimal {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &u32, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<u32, D::Error> {
        let value = DecimalU64::deserialize(deserializer)?;
        value.0.try_into().map_err(|_| serde::de::Error::custom("invalid-logical-counter"))
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stamp {
    pub physical_ms: DecimalU64,
    #[serde(with = "logical_decimal")]
    pub logical: u32,
    #[serde(deserialize_with = "decode_writer_id")]
    pub writer_id: String,
}
fn decode_writer_id<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = String::deserialize(d)?;
    validate_writer_id(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}
impl Stamp {
    pub fn validate(&self) -> Result<()> { validate_writer_id(&self.writer_id) }
}
impl Ord for Stamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.physical_ms.cmp(&other.physical_ms).then(self.logical.cmp(&other.logical))
            .then(self.writer_id.as_bytes().cmp(other.writer_id.as_bytes()))
    }
}
impl PartialOrd for Stamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
pub fn validate_writer_id(value: &str) -> Result<()> {
    if value.len() != 36 || value.as_bytes()[14] != b'4' || !matches!(value.as_bytes()[19], b'8'..=b'9' | b'a'..=b'b')
        || !value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_digit() || (b'a'..=b'f').contains(&b) }
        }) { return Err(WireError("invalid-writer-id")); }
    Ok(())
}

pub fn issue_stamp(wall_ms: u64, writer_id: &str, last: Option<&Stamp>, observed: Option<&Stamp>) -> Result<Stamp> {
    validate_writer_id(writer_id)?;
    let mut physical = wall_ms;
    for stamp in [last, observed].into_iter().flatten() {
        stamp.validate()?;
        physical = physical.max(stamp.physical_ms.0);
    }
    let logical = [last, observed].into_iter().flatten()
        .filter(|stamp| stamp.physical_ms.0 == physical).map(|stamp| stamp.logical).max()
        .map(|logical| logical.checked_add(1).ok_or(WireError("logical-counter-overflow")))
        .transpose()?.unwrap_or(0);
    Ok(Stamp { physical_ms: physical.into(), logical, writer_id: writer_id.into() })
}

pub const MAX_CLOCK_SKEW_MS: u64 = 300_000;
pub const CLOCK_SAMPLE_LIFETIME_MS: u64 = 900_000;

/// Times are captured around one successful, fresh target response.
#[derive(Clone, Debug)]
pub struct ClockSample {
    pub target_id: String,
    pub request_wall_ms: u64,
    pub response_wall_ms: u64,
    pub request_monotonic_ms: u64,
    pub response_monotonic_ms: u64,
    pub target_ms: u64,
    pub precision_ms: u64,
    pub successful: bool,
    pub cached_or_aged: bool,
}
#[derive(Clone, Debug)]
pub struct AdmittedClock {
    target_id: String,
    sampled_monotonic_ms: u64,
    target_upper_ms: u64,
}
impl ClockSample {
    pub fn admit(&self) -> Result<AdmittedClock> {
        if self.target_id.is_empty() || !self.successful || self.cached_or_aged { return Err(WireError("clock-sample-invalid")); }
        let elapsed = self.response_monotonic_ms.checked_sub(self.request_monotonic_ms).ok_or(WireError("clock-discontinuity"))?;
        let wall_elapsed = self.response_wall_ms.checked_sub(self.request_wall_ms).ok_or(WireError("clock-discontinuity"))?;
        if elapsed.abs_diff(wall_elapsed) > 1000 { return Err(WireError("clock-discontinuity")); }
        let midpoint = self.request_wall_ms.checked_add(wall_elapsed / 2).ok_or(WireError("clock-overflow"))?;
        let uncertainty = elapsed / 2 + elapsed % 2;
        let uncertainty = uncertainty.checked_add(self.precision_ms).ok_or(WireError("clock-overflow"))?;
        if midpoint.abs_diff(self.target_ms).checked_add(uncertainty).ok_or(WireError("clock-overflow"))? > MAX_CLOCK_SKEW_MS {
            return Err(WireError("clock-skew"));
        }
        Ok(AdmittedClock { target_id: self.target_id.clone(), sampled_monotonic_ms: self.response_monotonic_ms,
            target_upper_ms: self.target_ms.checked_add(uncertainty).ok_or(WireError("clock-overflow"))? })
    }
    pub fn http_date_target_ms(date_ms: u64) -> Result<u64> {
        date_ms.checked_add(500).ok_or(WireError("clock-overflow"))
    }
}
impl AdmittedClock {
    pub fn incoming_upper_ms(&self, target_id: &str, now_monotonic_ms: u64) -> Result<u64> {
        let age = now_monotonic_ms.checked_sub(self.sampled_monotonic_ms).ok_or(WireError("clock-sample-expired"))?;
        if target_id != self.target_id || age >= CLOCK_SAMPLE_LIFETIME_MS { return Err(WireError("clock-sample-expired")); }
        self.target_upper_ms.checked_add(age).and_then(|v| v.checked_add(MAX_CLOCK_SKEW_MS)).ok_or(WireError("clock-overflow"))
    }
    pub fn admit_incoming(&self, target_id: &str, now_monotonic_ms: u64, stamp: &Stamp) -> Result<()> {
        stamp.validate()?;
        if stamp.physical_ms.0 > self.incoming_upper_ms(target_id, now_monotonic_ms)? { return Err(WireError("incoming-clock-skew")); }
        Ok(())
    }
}
