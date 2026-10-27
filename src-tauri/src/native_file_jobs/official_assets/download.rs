//! One account read: the request, how its answer is classified, and the body
//! spooled into the job directory while its hash is computed.
use super::scheduler::Exchange;
use crate::native_file_jobs::official_snapshot::{
    authenticated_headers, await_network_operation, bounded_response_bytes, OfficialSnapshotCredential,
    NETWORK_IDLE_TIMEOUT,
};
use crate::native_file_jobs::{JobControl, NativeJobError};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING, RETRY_AFTER};
use risunest_network_log::reqwest::RequestBuilderExt;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// A downloaded body in the job directory. Dropping it removes the file, so a
/// body that is abandoned or already installed leaves nothing behind.
#[derive(Debug)]
pub(super) struct Spooled {
    pub(super) path: PathBuf,
    pub(super) sha256: String,
    pub(super) size: u64,
}

impl Drop for Spooled {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
pub(super) enum Read {
    Body(Spooled),
    Ended(Exchange),
    Cancelled,
}

pub(super) struct AccountReader {
    pub(super) client: reqwest::Client,
    pub(super) base_url: String,
    pub(super) credential: HeaderMap,
    pub(super) directory: PathBuf,
}

impl AccountReader {
    pub(super) fn new(
        base_url: &str,
        credential: &OfficialSnapshotCredential,
        directory: &Path,
    ) -> Result<Self, NativeJobError> {
        validate_base_url(base_url)?;
        // Other crates enable reqwest's decoders, which would strip a
        // Content-Encoding this client has to see and refuse.
        let client = crate::platform_tls::client_builder()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| NativeJobError::new("network-error", error.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            credential: authenticated_headers(credential)?,
            directory: directory.to_owned(),
        })
    }

    pub(super) async fn read(&self, key: &str, sequence: u64, job: &JobControl, received: &AtomicU64) -> Read {
        // Header values are bytes; fetch sends a key's code points as Latin-1 and refuses anything wider.
        let Some(key_header) = latin1_header(key) else {
            return Read::Ended(Exchange::Permanent);
        };
        let request = self
            .client
            .get(format!("{}/api/account/read/{}", self.base_url, hex::encode(key.as_bytes())))
            .headers(self.credential.clone())
            .header("x-risu-key", key_header)
            .header("x-risu-save-date", "0")
            .header(ACCEPT_ENCODING, "identity")
            .send_logged("account-asset-read");
        let response = match self.network(request, job).await {
            Ok(response) => response,
            Err(read) => return read,
        };
        let status = response.status().as_u16();
        match status {
            401 | 403 => Read::Ended(Exchange::Rejected),
            204 => Read::Ended(Exchange::Missing),
            303 => self.cache_answer(response, job).await,
            408 | 429 | 500..=u16::MAX => Read::Ended(Exchange::Retryable {
                status: Some(status),
                retry_after: response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            }),
            200..=299 => self.spool(response, sequence, job, received).await,
            _ => Read::Ended(Exchange::Permanent),
        }
    }

    async fn network<F, T>(&self, operation: F, job: &JobControl) -> Result<T, Read>
    where
        F: Future<Output = Result<T, reqwest::Error>>,
    {
        await_network_operation(operation, job, NETWORK_IDLE_TIMEOUT, "Official account asset read was cancelled")
            .await
            .map_err(|error| {
                if error.code == "cancelled" {
                    Read::Cancelled
                } else {
                    Read::Ended(Exchange::Retryable { status: None, retry_after: None })
                }
            })
    }

    /// A 303 says whether a cached copy matches; without a cache only a definite mismatch is an answer.
    async fn cache_answer(&self, response: reqwest::Response, job: &JobControl) -> Read {
        match bounded_response_bytes(response, job, NETWORK_IDLE_TIMEOUT).await {
            Ok(bytes) => {
                let matched = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|value| value.get("match").and_then(serde_json::Value::as_bool));
                Read::Ended(if matched == Some(false) { Exchange::Missing } else { Exchange::Permanent })
            }
            Err(error) if error.code == "cancelled" => Read::Cancelled,
            Err(error) if error.code == "network-error" => {
                Read::Ended(Exchange::Retryable { status: None, retry_after: None })
            }
            Err(_) => Read::Ended(Exchange::Permanent),
        }
    }

    async fn spool(&self, mut response: reqwest::Response, sequence: u64, job: &JobControl, received: &AtomicU64) -> Read {
        if response
            .headers()
            .get(CONTENT_ENCODING)
            .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
        {
            return Read::Ended(Exchange::Permanent);
        }
        let declared = response.content_length();
        if declared.is_some_and(|length| length > MAX_ASSET_BYTES) {
            return Read::Ended(Exchange::Permanent);
        }
        let mut spooled = Spooled {
            path: self.directory.join(format!("asset-{sequence}.payload")),
            sha256: String::new(),
            size: 0,
        };
        // Declared after the guard, so the file is closed before the guard removes it.
        let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(&spooled.path) else {
            return Read::Ended(Exchange::Local);
        };
        let mut hash = Sha256::new();
        loop {
            let chunk = match self.network(response.chunk(), job).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(read) => return read,
            };
            spooled.size += chunk.len() as u64;
            if spooled.size > MAX_ASSET_BYTES || declared.is_some_and(|length| spooled.size > length) {
                return Read::Ended(Exchange::Permanent);
            }
            hash.update(&chunk);
            if file.write_all(&chunk).is_err() {
                return Read::Ended(Exchange::Local);
            }
            received.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        drop(file);
        if declared.is_some_and(|length| length != spooled.size) {
            return Read::Ended(Exchange::Permanent);
        }
        spooled.sha256 = hex::encode(hash.finalize());
        Read::Body(spooled)
    }
}

pub(super) fn validate_base_url(base_url: &str) -> Result<(), NativeJobError> {
    let parsed = url::Url::parse(base_url).map_err(|error| NativeJobError::new("invalid-input", error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(NativeJobError::new("invalid-input", "Official account base URL must use HTTP or HTTPS"));
    }
    Ok(())
}

pub(super) fn latin1_header(key: &str) -> Option<HeaderValue> {
    let bytes = key.chars().map(|character| u8::try_from(u32::from(character)).ok()).collect::<Option<Vec<_>>>()?;
    HeaderValue::from_bytes(&bytes).ok()
}
