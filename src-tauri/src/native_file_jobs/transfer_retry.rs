use super::{error::cancelled, JobControl, NativeJobError};
use reqwest::header::HeaderMap;
use std::time::Duration;

pub(super) fn retry_delay(headers: &HeaderMap, retries: u32) -> Duration {
    Duration::from_secs(headers.get("retry-after").and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok()).unwrap_or(1 << retries).min(30))
}

pub(super) async fn wait_for_retry(job: &JobControl, delay: Duration) -> Result<(), NativeJobError> {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        if job.is_cancel_requested() { return Err(cancelled("official transfer retry was cancelled")); }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { return Ok(()); }
        tokio::time::sleep(remaining.min(Duration::from_millis(50))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_keeps_backoff_header_and_upper_bound() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_delay(&headers, 2), Duration::from_secs(4));
        headers.insert("retry-after", "7".parse().unwrap());
        assert_eq!(retry_delay(&headers, 0), Duration::from_secs(7));
        headers.insert("retry-after", "999".parse().unwrap());
        assert_eq!(retry_delay(&headers, 0), Duration::from_secs(30));
        headers.insert("retry-after", "invalid".parse().unwrap());
        assert_eq!(retry_delay(&headers, 1), Duration::from_secs(2));
    }

    #[test]
    fn a_cancelled_job_does_not_wait_for_the_retry_delay() {
        let job = super::super::JobRegistry::default()
            .create(super::super::JobKind::RestoreOfficialAccountSnapshot).unwrap();
        job.request_cancel().unwrap();
        let error = tauri::async_runtime::block_on(wait_for_retry(&job, Duration::from_secs(30))).unwrap_err();
        assert_eq!(error.code, "cancelled");
    }
}
