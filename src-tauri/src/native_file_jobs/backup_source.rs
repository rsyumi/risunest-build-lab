//! A bounded signature probe chooses a reader; the chosen job still verifies its complete input.
use super::*;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BackupSourceFormat {
    Portable,
    BlockRisuSave,
    LocalBackup,
}

pub(crate) fn detect(mut file: File) -> Result<BackupSourceFormat, NativeJobError> {
    let unsupported = || {
        NativeJobError::new(
            "unsupported-format",
            "The selected file is not a supported backup",
        )
    };
    let mut prefix = [0; 11];
    let count = file.read(&mut prefix).map_err(|_| unsupported())?;
    file.seek(SeekFrom::Start(0)).map_err(|_| unsupported())?;
    if prefix[..count].starts_with(b"RISUSAVE\0")
        || prefix[..count].starts_with(b"\0RISUSAVE\0")
        || prefix[..count].starts_with(b"\0\0RISU")
    {
        return Ok(BackupSourceFormat::BlockRisuSave);
    }
    if prefix[..count].starts_with(b"PK\x03\x04") {
        let mut entry = zip::read::read_zipfile_from_stream(&mut file)
            .map_err(|_| unsupported())?
            .ok_or_else(unsupported)?;
        if entry.name() != "format.json"
            || entry.size() > 16 * 1024
            || entry.compression() != zip::CompressionMethod::Stored
        {
            return Err(unsupported());
        }
        let value: serde_json::Value =
            serde_json::from_reader((&mut entry).take(16 * 1024 + 1)).map_err(|_| unsupported())?;
        if value.get("magic").and_then(Value::as_str) == Some("risunest-portable-backup") {
            return Ok(BackupSourceFormat::Portable);
        }
        return Err(unsupported());
    }
    if count < 4 {
        return Err(unsupported());
    }
    let size = u32::from_le_bytes(prefix[..4].try_into().unwrap()) as usize;
    if size == 0 || size > 1024 * 1024 {
        return Err(unsupported());
    }
    file.seek(SeekFrom::Start(4)).map_err(|_| unsupported())?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes).map_err(|_| unsupported())?;
    let name = std::str::from_utf8(&bytes).map_err(|_| unsupported())?;
    if name.contains('\0')
        || name.contains('\\')
        || name
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(unsupported());
    }
    // Foreign local backups start with a length-framed entry, commonly an asset. No extension
    // or one-byte compressed header can establish a full archive's validity.
    Ok(BackupSourceFormat::LocalBackup)
}

#[tauri::command(async)]
pub(crate) fn native_backup_source_format(
    app: AppHandle,
    state: State<'_, NativeFileJobState>,
    source: JobSource,
) -> Result<BackupSourceFormat, NativeJobError> {
    if matches!(source,JobSource::AndroidSeekable{..}|JobSource::IosScoped{..}) {
        portable_source_custody::ensure_platform_source(&app,&source)?;
        let detected=portable_source_custody::probe_format(&source)?;
        portable_source_custody::confirm_platform_probe(&app,&source,detected)?;
        Ok(detected)
    } else { detect(open_job_source(&state.root,&source)?.file) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn android_pocket_backups_probe_and_claim_the_kotlin_spool() {
        for bytes in [
            include_bytes!("legacy_backup/fixtures/pocket-risu-v1.10.0.bin").as_slice(),
            include_bytes!("legacy_backup/fixtures/pocket-risu-v1.12.0.bin").as_slice(),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let token = Uuid::new_v4().to_string();
            let spool = directory.path().join("sources").join(&token);
            fs::create_dir_all(&spool).unwrap();
            fs::write(spool.join("source.risudat"), bytes).unwrap();
            fs::write(spool.join("ownership.json"), serde_json::to_vec(&serde_json::json!({
                "format": ANDROID_SPOOL_FORMAT, "version": 1, "token": token, "createdAtMillis": 1,
            })).unwrap()).unwrap();
            fs::write(spool.join("source.json"), serde_json::to_vec(&serde_json::json!({
                "token": token, "state": "ready", "displayName": "pocket.bin", "bytes": bytes.len(),
                "totalBytes": bytes.len(), "importDestination": null,
            })).unwrap()).unwrap();
            let source = JobSource::AndroidSpool { token: token.clone() };
            assert_eq!(detect(open_job_source(directory.path(), &source).unwrap().file).unwrap(), BackupSourceFormat::LocalBackup);
            let jobs = directory.path().join("jobs");
            fs::create_dir_all(&jobs).unwrap();
            let owned = create_owned_directory(&jobs, &Uuid::new_v4().to_string()).unwrap();
            let claimed = claim_spool_source(directory.path(), &token, &owned).unwrap();
            assert_eq!(claimed.opened.total_bytes, bytes.len() as u64);
            assert!(open_job_source(directory.path(), &source).is_err());
        }
    }

    #[test]
    fn signatures_ignore_names_and_reject_truncated_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("misleading.risunest");
        for (bytes, expected) in [
            (&b"RISUSAVE\0x"[..], Some(BackupSourceFormat::BlockRisuSave)),
            (
                &b"\0RISUSAVE\0\x08x"[..],
                Some(BackupSourceFormat::BlockRisuSave),
            ),
            (
                &b"\x04\0\0\0test\0\0\0\0"[..],
                Some(BackupSourceFormat::LocalBackup),
            ),
            (&b"\x04\0\0\0t"[..], None),
            (&b"PK\x03\x04"[..], None),
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(detect(File::open(&path).unwrap()).ok(), expected);
        }
    }
}
