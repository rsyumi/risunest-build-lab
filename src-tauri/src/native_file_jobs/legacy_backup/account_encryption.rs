use super::*;
use aes::cipher::{BlockEncrypt, KeyInit};
use ghash::universal_hash::UniversalHash;
use risunest_network_log::reqwest::RequestBuilderExt;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::time::Duration;
use subtle::ConstantTimeEq;

const CHUNK: usize = 64 * 1024;
const KEY_RESPONSE_LIMIT: usize = 64 * 1024;
const KEY_TIMEOUT: Duration = Duration::from_secs(30);

fn cancelled(job: &JobControl) -> Result<(), NativeJobError> {
    if job.is_cancel_requested() {
        Err(NativeJobError::new("cancelled", "Backup restore cancelled"))
    } else { Ok(()) }
}

pub(super) fn prepare(
    entries: &[StagedLocalBackupEntry],
    database: &StagedLocalBackupEntry,
    job: &JobControl,
) -> Result<Option<tempfile::NamedTempFile>, NativeJobError> {
    prepare_with_key(entries, database, job, |timestamp| tauri::async_runtime::block_on(request_key(
        &format!("https://sv.risuai.xyz/cryptokey?key={timestamp:.0}"), job, KEY_TIMEOUT,
    )))
}

pub(in crate::native_file_jobs) fn prepare_with_key(
    entries: &[StagedLocalBackupEntry],
    database: &StagedLocalBackupEntry,
    job: &JobControl,
    request: impl FnOnce(f64) -> Result<String, NativeJobError>,
) -> Result<Option<tempfile::NamedTempFile>, NativeJobError> {
    let Some(entry) = entries.iter().find(|entry| entry.logical_name == ENCRYPTION_ENTRY) else { return Ok(None); };
    let invalid = || NativeJobError::new("backup-encryption-metadata-invalid", "Backup encryption metadata is invalid");
    if entry.byte_length > MAX_METADATA_BYTES as u64 { return Err(invalid()); }
    let metadata: Value = serde_json::from_reader(BufReader::new(open_staged(entry).map_err(local_backup_error)?))
        .map_err(|_| invalid())?;
    if metadata.get("type").and_then(Value::as_str) == Some("none") { return Ok(None); }
    if metadata.get("type").and_then(Value::as_str) != Some("account") {
        return Err(NativeJobError::new("backup-encryption-unsupported", "Backup encryption type is unsupported"));
    }
    let timestamp = metadata.get("time").and_then(Value::as_f64)
        .filter(|time| time.is_finite() && *time >= 0.0 && time.fract() == 0.0 && *time <= 9_007_199_254_740_991.0)
        .ok_or_else(invalid)?;
    cancelled(job)?;
    progress(job, super::super::JobStage::RequestingBackupKey, 0, None)?;
    let key = request(timestamp)?;
    let path = database.staged_path.as_deref().ok_or_else(invalid)?;
    let directory = path.parent().ok_or_else(invalid)?;
    decrypt(File::open(path).map_err(io_job_error)?, database.byte_length, &key, directory, job).map(Some)
}

async fn request_key(url: &str, job: &JobControl, timeout: Duration) -> Result<String, NativeJobError> {
    let request = async {
        let client = crate::platform_tls::client_builder().connect_timeout(timeout).timeout(timeout)
            .build().map_err(|_| NativeJobError::new("backup-key-request-failed", "Could not request the account backup key"))?;
        let mut response = client.get(url).header("Sec-Fetch-Site", "same-site").send_logged("http-plugin").await
            .map_err(|_| NativeJobError::new("backup-key-request-failed", "Could not request the account backup key"))?;
        if !response.status().is_success() {
            return Err(NativeJobError::new("backup-key-http-error", format!("Account backup key request failed (HTTP {})", response.status().as_u16())));
        }
        let invalid = || NativeJobError::new("backup-key-invalid", "The account server returned an invalid backup key");
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| NativeJobError::new("backup-key-request-failed", "Could not read the account backup key response"))? {
            if chunk.len() > KEY_RESPONSE_LIMIT.saturating_sub(bytes.len()) { return Err(invalid()); }
            bytes.extend_from_slice(&chunk);
        }
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        data.get("key").and_then(Value::as_str).filter(|key| !key.trim().is_empty())
            .map(str::to_owned).ok_or_else(invalid)
    };
    let cancellation = async {
        loop {
            cancelled(job)?;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        #[allow(unreachable_code)] Ok::<String, NativeJobError>(String::new())
    };
    await_key(request, cancellation, tokio::time::sleep(timeout)).await
}

async fn await_key(
    request: impl std::future::Future<Output = Result<String, NativeJobError>>,
    cancellation: impl std::future::Future<Output = Result<String, NativeJobError>>,
    deadline: impl std::future::Future<Output = ()>,
) -> Result<String, NativeJobError> {
    tokio::select! {
        biased;
        result = cancellation => result,
        _ = deadline => Err(NativeJobError::new("backup-key-request-failed", "Account backup key request timed out")),
        result = request => result,
    }
}

// The existing WebCrypto format has no AAD, a zero 96-bit nonce, and one tag.
// Authenticate the ciphertext before handing the private plaintext file to a reader.
fn decrypt(
    mut source: impl Read, total: u64, key: &str, directory: &std::path::Path, job: &JobControl,
) -> Result<tempfile::NamedTempFile, NativeJobError> {
    let invalid = || NativeJobError::new("backup-decryption-failed", "The backup database could not be decrypted");
    let length = total.checked_sub(16).filter(|length| *length <= (1u64 << 36) - 32).ok_or_else(invalid)?;
    progress(job, super::super::JobStage::DecryptingDatabase, 0, Some(length))?;
    let digest = Sha256::digest(key.as_bytes());
    let cipher = aes::Aes256::new_from_slice(digest.as_slice()).map_err(|_| invalid())?;
    let mut hash_key = aes::cipher::Block::<aes::Aes256>::default();
    cipher.encrypt_block(&mut hash_key);
    let mut auth = ghash::GHash::new(&hash_key);
    let mut mask = aes::cipher::Block::<aes::Aes256>::default();
    mask[15] = 1;
    cipher.encrypt_block(&mut mask);
    let mut plaintext = tempfile::NamedTempFile::new_in(directory).map_err(io_job_error)?;
    let mut buffer = [0u8; CHUNK];
    let mut stream = vec![aes::cipher::Block::<aes::Aes256>::default(); CHUNK / 16];
    let mut remaining = length;
    let mut counter = 2u32;
    while remaining != 0 {
        cancelled(job)?;
        let size = remaining.min(CHUNK as u64) as usize;
        source.read_exact(&mut buffer[..size]).map_err(|_| invalid())?;
        cancelled(job)?;
        auth.update_padded(&buffer[..size]);
        let blocks = size.div_ceil(16);
        for block in &mut stream[..blocks] {
            block.fill(0);
            block[12..].copy_from_slice(&counter.to_be_bytes());
            counter = counter.wrapping_add(1);
        }
        cipher.encrypt_blocks(&mut stream[..blocks]);
        for (bytes, block) in buffer[..size].chunks_mut(16).zip(&stream) {
            for (byte, mask) in bytes.iter_mut().zip(block) { *byte ^= mask; }
        }
        plaintext.write_all(&buffer[..size]).map_err(io_job_error)?;
        remaining -= size as u64;
        progress(job, super::super::JobStage::DecryptingDatabase, length - remaining, Some(length))?;
    }
    cancelled(job)?;
    let mut lengths = ghash::Block::default();
    lengths[8..].copy_from_slice(&(length * 8).to_be_bytes());
    auth.update(&[lengths]);
    let mut expected = auth.finalize();
    for (byte, mask) in expected.iter_mut().zip(mask) { *byte ^= mask; }
    let mut actual = [0u8; 16];
    source.read_exact(&mut actual).map_err(|_| invalid())?;
    let mut extra = [0u8; 1];
    if source.read(&mut extra).map_err(|_| invalid())? != 0 || !bool::from(expected.as_slice().ct_eq(&actual)) {
        return Err(invalid());
    }
    plaintext.as_file().sync_all().map_err(io_job_error)?;
    Ok(plaintext)
}

fn progress(job: &JobControl, stage: super::super::JobStage, completed: u64, total: Option<u64>) -> Result<(), NativeJobError> {
    let status = job.status();
    if status.state == super::super::JobState::Queued { return Ok(()); }
    job.set_detail(super::super::JobDetail::new(
        stage, super::super::StageUnit::Bytes, completed, total,
        status.detail.map(|detail| detail.counts).unwrap_or_default(),
    )).map_err(|message| {
        cancelled(job).err().unwrap_or_else(|| NativeJobError::new("store-error", message))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use crate::native_file_jobs::{JobKind, JobRegistry};

    #[test]
    fn key_wait_observes_injected_deadline_and_cancellation_without_a_job_deadline() {
        use std::future::{pending, ready};
        tauri::async_runtime::block_on(async {
            assert_eq!(await_key(pending(), pending(), ready(())).await.unwrap_err().code, "backup-key-request-failed");
            assert_eq!(await_key(pending(), ready(Err(NativeJobError::new("cancelled", "Synthetic cancellation"))), pending()).await.unwrap_err().code, "cancelled");
            assert_eq!(await_key(ready(Ok("synthetic-key".into())), pending(), pending()).await.unwrap(), "synthetic-key");
        });
    }

    #[test]
    fn key_http_boundary_checks_status_json_size_and_required_header() {
        for (status, body, expected) in [
            ("200 OK", r#"{"key":"synthetic-key"}"#.to_owned(), Ok("synthetic-key")),
            ("503 Service Unavailable", "unavailable".to_owned(), Err("backup-key-http-error")),
            ("200 OK", "invalid JSON".to_owned(), Err("backup-key-invalid")),
            ("200 OK", r#"{"key":" "}"#.to_owned(), Err("backup-key-invalid")),
            ("200 OK", format!(r#"{{"key":"{}"}}"#, "a".repeat(KEY_RESPONSE_LIMIT)), Err("backup-key-invalid")),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let header = String::from_utf8(request).unwrap().to_lowercase();
                assert!(header.contains("sec-fetch-site: same-site\r\n"));
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let job = JobRegistry::default().create(JobKind::RestoreLegacyLocalBackup).unwrap();
            let result = tauri::async_runtime::block_on(request_key(&format!("http://{address}/cryptokey?key=1"), &job, Duration::from_secs(5)));
            assert_eq!(result.as_deref().map_err(|error| error.code.as_str()), expected);
            server.join().unwrap();
        }
    }

    #[test]
    fn webcrypto_writer_round_trip_and_authentication_failures() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/account-encryption.json")).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let job = JobRegistry::default().create(JobKind::RestoreLegacyLocalBackup).unwrap();
        let key = fixture["key"].as_str().unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let bytes = base64::engine::general_purpose::STANDARD.decode(case["ciphertext"].as_str().unwrap()).unwrap();
            let plain = decrypt(bytes.as_slice(), bytes.len() as u64, key, directory.path(), &job).unwrap();
            let restored = std::fs::read(plain.path()).unwrap();
            assert_eq!(restored.len() as u64, case["size"].as_u64().unwrap());
            assert_eq!(hex::encode(Sha256::digest(&restored)), case["sha256"].as_str().unwrap());
            drop(plain);
            for (input, key) in [
                (bytes.clone(), "wrong synthetic key"),
                ({ let mut changed = bytes.clone(); changed[0] ^= 1; changed }, key),
                ({ let mut changed = bytes.clone(); let last = changed.len() - 1; changed[last] ^= 1; changed }, key),
                (bytes[..bytes.len() - 1].to_vec(), key),
            ] {
                assert_eq!(decrypt(input.as_slice(), input.len() as u64, key, directory.path(), &job).unwrap_err().code, "backup-decryption-failed");
                assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
            }
        }
    }

    #[test]
    fn decryption_reads_bounded_chunks_and_removes_cancelled_plaintext() {
        struct CheckedReader<'a> {
            bytes: &'a [u8],
            job: &'a JobControl,
            cancel_after_read: bool,
            maximum: usize,
        }
        impl Read for CheckedReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                assert!(buffer.len() <= CHUNK);
                self.maximum = self.maximum.max(buffer.len());
                let count = self.bytes.read(buffer)?;
                if self.cancel_after_read { self.job.request_cancel().unwrap(); }
                Ok(count)
            }
        }
        let fixture: Value = serde_json::from_str(include_str!("fixtures/account-encryption.json")).unwrap();
        let case = fixture["cases"].as_array().unwrap().last().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(case["ciphertext"].as_str().unwrap()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        for cancel_after_read in [false, true] {
            let job = JobRegistry::default().create(JobKind::RestoreLegacyLocalBackup).unwrap();
            job.start(JobPhase::ReadingSource).unwrap();
            let mut source = CheckedReader { bytes: &bytes, job: &job, cancel_after_read, maximum: 0 };
            let result = decrypt(&mut source, bytes.len() as u64, fixture["key"].as_str().unwrap(), directory.path(), &job);
            if cancel_after_read {
                assert_eq!(result.unwrap_err().code, "cancelled");
                assert!(!source.bytes.is_empty(), "cancellation must stop before reading the rest");
            } else {
                let file = result.unwrap();
                assert_eq!(file.as_file().metadata().unwrap().len(), case["size"].as_u64().unwrap());
            }
            assert_eq!(source.maximum, CHUNK);
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn invalid_encryption_metadata_never_reaches_the_key_service() {
        let directory = tempfile::tempdir().unwrap();
        let metadata_path = directory.path().join("metadata");
        let database = StagedLocalBackupEntry { logical_name: DATABASE_ENTRY.into(), byte_length: 32, sha256: String::new(), staged_path: None };
        let job = JobRegistry::default().create(JobKind::RestoreLegacyLocalBackup).unwrap();
        for (bytes, expected) in [
            ("invalid JSON", "backup-encryption-metadata-invalid"),
            (r#"{"type":"account","time":-1}"#, "backup-encryption-metadata-invalid"),
            (r#"{"type":"account","time":1.5}"#, "backup-encryption-metadata-invalid"),
            (r#"{"type":"account","time":"1"}"#, "backup-encryption-metadata-invalid"),
            (r#"{"type":"account","time":9007199254740992}"#, "backup-encryption-metadata-invalid"),
            (r#"{"type":"unknown","time":1}"#, "backup-encryption-unsupported"),
        ] {
            std::fs::write(&metadata_path, bytes).unwrap();
            let entries = [StagedLocalBackupEntry { logical_name: ENCRYPTION_ENTRY.into(), byte_length: bytes.len() as u64, sha256: String::new(), staged_path: Some(metadata_path.clone()) }];
            assert_eq!(prepare(&entries, &database, &job).unwrap_err().code, expected);
        }
        let entries = [StagedLocalBackupEntry { logical_name: ENCRYPTION_ENTRY.into(), byte_length: MAX_METADATA_BYTES as u64 + 1, sha256: String::new(), staged_path: None }];
        assert_eq!(prepare(&entries, &database, &job).unwrap_err().code, "backup-encryption-metadata-invalid");
    }
}
