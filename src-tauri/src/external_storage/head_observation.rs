use super::{contract::{ErrorKind, ProviderError, Result, VersionToken}, publication::HeadObservation};
use serde::{Deserialize, Serialize};
fn corrupt(_: impl std::fmt::Display) -> ProviderError { ProviderError::new(ErrorKind::Corrupt) }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredHeadObservation {
    commit_id: String,
    authenticated_body_hash: String,
    version: Option<VersionToken>,
}
impl From<&HeadObservation> for StoredHeadObservation {
    fn from(value: &HeadObservation) -> Self {
        Self {
            commit_id: value.commit_id.clone(),
            authenticated_body_hash: value.authenticated_body_hash.clone(),
            version: value.version.clone(),
        }
    }
}

pub(crate) fn observation_json(value: &HeadObservation) -> Result<String> {
    if value.commit_id.is_empty()
        || !crate::trust_boundary::is_lower_hex_256(&value.authenticated_body_hash)
        || value
            .version
            .as_ref()
            .is_some_and(|version| version.0.is_empty())
    {
        return Err(corrupt("invalid authenticated head observation"));
    }
    serde_json::to_string(&StoredHeadObservation::from(value)).map_err(corrupt)
}
pub(crate) fn head_observation(value: &str) -> Result<HeadObservation> {
    let stored = parse_observation(value)?;
    Ok(HeadObservation { commit_id: stored.commit_id,
        authenticated_body_hash: stored.authenticated_body_hash, version: stored.version })
}

fn parse_observation(value: &str) -> Result<StoredHeadObservation> {
    if value.is_empty() || value.len() > 16 * 1024 {
        return Err(corrupt("invalid stored head observation"));
    }
    let parsed: StoredHeadObservation = serde_json::from_str(value).map_err(corrupt)?;
    if parsed.commit_id.is_empty()
        || !crate::trust_boundary::is_lower_hex_256(&parsed.authenticated_body_hash)
        || parsed
            .version
            .as_ref()
            .is_some_and(|version| version.0.is_empty())
        || serde_json::to_string(&parsed).map_err(corrupt)? != value
    {
        return Err(corrupt("invalid stored head observation"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_head_observations_require_exact_authenticated_shape() {
        let head = HeadObservation { commit_id: "commit".into(), authenticated_body_hash: "a".repeat(64), version: Some(VersionToken("version".into())) };
        let encoded = observation_json(&head).unwrap();
        assert_eq!(head_observation(&encoded).unwrap(), head);
        assert!(head_observation(&(encoded.clone()+" ")).is_err());
        assert!(head_observation(&encoded.replace(&"a".repeat(64), "bad")).is_err());
        assert!(head_observation(&encoded.replace("version\":\"version", "version\":\"" )).is_err());
    }
}
