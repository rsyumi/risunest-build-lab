pub(crate) mod backups;
pub(crate) mod cache;
pub(crate) mod client;
pub(crate) mod commands;
pub(crate) mod credentials;
pub(crate) mod events;
pub(crate) mod management;
pub(crate) mod media;
pub(crate) mod planner;
pub(crate) mod remote;
pub(crate) mod residency;
#[cfg(test)]
mod tests;
pub(crate) mod transfer;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncError {
    pub code: String,
    pub status: u16,
    /// False for a rejection the same request would receive again. The scheduler
    /// stops automatic retries on it instead of backing off forever.
    pub retryable: bool,
}
impl SyncError {
    pub fn new(code: impl Into<String>, status: u16) -> Self {
        let code = code.into();
        let retryable = retryable(&code, status);
        Self {
            code,
            status,
            retryable,
        }
    }
}
/// Transport failures, client-side waits, server load and ordering outcomes are
/// worth another attempt. Every validation reply and local invariant failure is
/// not, including a reply this client rejected as invalid.
fn retryable(code: &str, status: u16) -> bool {
    match code {
        "cancelled"
        | "stale-head"
        | "operation-already-pending"
        | "local-revision-changed"
        | "remote-catch-up-required"
        | "device-operation-active"
        | "staging-expired"
        | "upload-expired"
        | "pin-expired"
        | "operation-history-expired" => true,
        _ => match status {
            408 | 429 => true,
            // An unparsed body is what a gateway returns; a parsed 502 code is
            // this client refusing the reply, which repeating cannot change.
            502 => code == "server-response-error",
            500..=599 => true,
            _ => false,
        },
    }
}
impl From<risunest_sync_wire::WireError> for SyncError {
    fn from(value: risunest_sync_wire::WireError) -> Self {
        Self::new(value.0, 400)
    }
}
impl From<std::io::Error> for SyncError {
    fn from(_: std::io::Error) -> Self {
        Self::new("local-storage", 503)
    }
}
impl From<rusqlite::Error> for SyncError {
    fn from(_: rusqlite::Error) -> Self {
        Self::new("local-metadata", 503)
    }
}
impl From<crate::persistent_store::StoreError> for SyncError {
    fn from(value: crate::persistent_store::StoreError) -> Self {
        match value {
            crate::persistent_store::StoreError::RevisionConflict { .. } => {
                Self::new("local-revision-changed", 409)
            }
            _ => Self::new("local-validation", 409),
        }
    }
}
pub(crate) type Result<T> = std::result::Result<T, SyncError>;

impl From<risunest_sync_connect::ConnectError> for SyncError {
    fn from(value: risunest_sync_connect::ConnectError) -> Self {
        Self::new(value.0, 400)
    }
}

#[cfg(test)]
mod classification_tests {
    use super::SyncError;

    #[test]
    fn transport_waits_and_server_load_stay_retryable() {
        for (code, status) in [
            ("server-unreachable", 503),
            ("server-timeout", 503),
            ("incomplete-response", 503),
            ("directory-unreachable", 503),
            ("sync-retry-budget-exhausted", 503),
            ("cancelled", 409),
            ("device-busy", 429),
            ("server-busy", 429),
            ("server-updating", 503),
            ("storage-unavailable", 503),
            ("request-timeout", 408),
            ("server-response-error", 502),
            ("staging-expired", 410),
            ("upload-expired", 410),
            ("pin-expired", 410),
            ("operation-history-expired", 410),
            ("stale-head", 412),
            ("operation-already-pending", 409),
            ("local-revision-changed", 409),
            ("remote-catch-up-required", 409),
            ("device-operation-active", 409),
        ] {
            assert!(
                SyncError::new(code, status).retryable,
                "{code} ({status}) must stay retryable"
            );
        }
    }

    #[test]
    fn validation_replies_and_local_invariants_stop_automatic_retries() {
        for (code, status) in [
            ("invalid-control-schema", 400),
            ("unordered-keys", 400),
            ("too-many-objects", 400),
            ("unauthorized", 401),
            ("forbidden", 403),
            ("object-not-found", 404),
            ("request-too-large", 413),
            ("unsupported-media-type", 415),
            ("precondition-required", 428),
            ("page-intent-conflict", 409),
            ("page-gap", 409),
            ("staging-sealed", 409),
            ("staging-not-sealed", 409),
            ("descriptor-object-mismatch", 409),
            ("missing-dependency", 409),
            ("missing-related-record", 409),
            ("reference-kind-mismatch", 409),
            ("changes-digest-mismatch", 409),
            ("operation-intent-conflict", 409),
            ("staged-intent-mismatch", 409),
            ("invalid-operation-status", 502),
            ("receipt-identity-mismatch", 409),
            ("server-descriptor-semantics-mismatch", 409),
            ("cached-object-missing", 409),
            ("missing-local-payload", 409),
            ("invalid-remote-record", 502),
            ("invalid-missing-response", 502),
            ("invalid-directory-envelope", 502),
            ("unexpected-content-encoding", 502),
            ("response-too-large", 502),
            ("epoch-reconciliation-required", 409),
            ("new-device-registration-required", 409),
            ("device-credential-unavailable", 409),
        ] {
            assert!(
                !SyncError::new(code, status).retryable,
                "{code} ({status}) must block automatic retries"
            );
        }
    }
}
