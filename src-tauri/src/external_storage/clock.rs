use super::contract::{ErrorKind, ProviderError, Result};

pub(crate) const MAX_DISCONTINUITY_MS: u64 = 5_000;
pub(crate) const MAX_SERVER_OFFSET_MS: u64 = 300_000;
pub(crate) const SAMPLE_LIFETIME_MS: u64 = 15 * 60_000;

pub(crate) fn failure(stage: &str, reason: &str, facts: impl std::fmt::Display) -> ProviderError {
    let facts = facts.to_string();
    let detail = format!("stage={stage} reason={reason}");
    ProviderError::new(ErrorKind::ClockSkew).caused(
        &if facts.is_empty() { detail } else { format!("{detail} {facts}") })
}

pub(crate) fn repair_failure(error: crate::persistent_store::StoreError) -> ProviderError {
    use crate::{native_log::CommandFailure, persistent_store::StoreError};
    let code = error.code();
    let reason = match &error {
        StoreError::Validation { message } if matches!(message.as_str(),
            "intent-quarantined" | "request-id-integrity" | "accepted-clock-correction-required"
            | "unpublished-proof-incomplete" | "clock-overflow" | "binding-authority-changed") => message.as_str(),
        _ => code.as_ref(),
    };
    failure("publish", "clock-repair-failed", format_args!("repair_error={reason}"))
}

pub(crate) fn movement(before: u64, after: u64, elapsed: u64) -> i128 {
    i128::from(after) - i128::from(before) - i128::from(elapsed)
}

pub(crate) fn validate_sample(date: u64, before: u64, after: u64, rtt: u64) -> Result<()> {
    let delta = movement(before, after, rtt);
    if delta.abs() > i128::from(MAX_DISCONTINUITY_MS) {
        return Err(failure("sample", "wall-clock-jump",
            format_args!("delta_ms={delta} limit_ms={MAX_DISCONTINUITY_MS} rtt_ms={rtt}")));
    }
    let local_mid = (i128::from(before) + i128::from(after)) / 2;
    let server_mid = i128::from(date) + 500;
    // Include both wall-clock readings when a correction occurred during the request.
    let uncertainty = (i128::from(rtt) + 1) / 2 + 500 + (delta.abs() + 1) / 2;
    let bound = (server_mid - local_mid).abs() + uncertainty;
    if bound > i128::from(MAX_SERVER_OFFSET_MS) {
        return Err(failure("sample", "server-offset",
            format_args!("offset_bound_ms={bound} limit_ms={MAX_SERVER_OFFSET_MS} delta_ms={delta} rtt_ms={rtt}")));
    }
    Ok(())
}

pub(crate) fn future_change(stage: &str, physical_ms: u64, upper_ms: u64) -> ProviderError {
    failure(stage, "future-change",
        format_args!("excess_ms={}", physical_ms.saturating_sub(upper_ms)))
}

pub(crate) fn after_refresh(mut error: ProviderError) -> ProviderError {
    if error.kind == ErrorKind::ClockSkew {
        let detail = format!("{} refresh_attempted=true",
            error.cause.0.as_deref().unwrap_or("stage=admission reason=sample-missing").trim_end());
        error = error.caused(&detail);
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_clock_details_keep_fixed_causes_without_empty_fields() {
        use crate::{native_log::CommandFailure, persistent_store::StoreError};
        let missing = failure("admission", "sample-missing", "");
        assert_eq!(missing.detail().as_deref(), Some("stage=admission reason=sample-missing"));
        assert_eq!(after_refresh(missing).detail().as_deref(),
            Some("stage=admission reason=sample-missing refresh_attempted=true"));
        for (message, expected) in [
            ("intent-quarantined", "intent-quarantined"),
            ("request-id-integrity", "request-id-integrity"),
            ("synthetic-private-value", "validation"),
        ] {
            let error = repair_failure(StoreError::Validation { message: message.into() });
            assert_eq!(serde_json::to_value(&error).unwrap()["detail"],
                format!("stage=publish reason=clock-repair-failed repair_error={expected}"));
        }
    }
}
