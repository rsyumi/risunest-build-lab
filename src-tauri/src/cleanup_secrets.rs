use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::Mutex,
};

static INDEX_LOCK: Mutex<()> = Mutex::new(());
const DIRECTORY: &str = "owned-secret-index";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Purpose {
    Provider,
    RepositoryKey,
    AccountCredential,
    ServerSync,
}

impl Purpose {
    fn name(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::RepositoryKey => "repository-key",
            Self::AccountCredential => "account-credential",
            Self::ServerSync => "server-sync",
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    purpose: Purpose,
    id: String,
}

pub(crate) fn account_id(root: &Path) -> String {
    use sha2::Digest;
    format!(
        "official-account-{}",
        hex::encode(sha2::Sha256::digest(root.as_os_str().as_encoded_bytes()))
    )
}

fn validate(root: &Path, entry: &Entry) -> Result<(), String> {
    let valid = match entry.purpose {
        Purpose::AccountCredential => entry.id == account_id(root),
        _ => uuid::Uuid::parse_str(&entry.id)
            .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == entry.id),
    };
    if valid {
        Ok(())
    } else {
        Err("secret-index-corrupt".into())
    }
}

fn ensure_directory(root: &Path) -> Result<std::path::PathBuf, String> {
    let path = root.join(DIRECTORY);
    fs::create_dir_all(root).map_err(|_| "secret-index-unavailable")?;
    match fs::create_dir(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err("secret-index-unavailable".into()),
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| "secret-index-unavailable")?;
    if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) {
        return Err("secret-index-corrupt".into());
    }
    #[cfg(unix)]
    fs::File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|_| "secret-index-unavailable")?;
    Ok(path)
}

fn read_entry(path: &Path) -> Result<Entry, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "secret-index-unavailable")?;
    if !metadata.is_file()
        || crate::trust_boundary::is_link_like(&metadata)
        || metadata.len() > 1024
    {
        return Err("secret-index-corrupt".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "secret-index-unavailable")?
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|_| "secret-index-unavailable")?;
    serde_json::from_slice(&bytes).map_err(|_| "secret-index-corrupt".into())
}

// Persist ownership before invoking the OS store, including failed creation attempts.
pub(crate) fn tracked_write<T>(
    root: &Path,
    purpose: Purpose,
    id: &str,
    write: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _lock = INDEX_LOCK.lock().map_err(|_| "secret-index-unavailable")?;
    let entry = Entry {
        purpose,
        id: id.to_owned(),
    };
    validate(root, &entry)?;
    let directory = ensure_directory(root)?;
    let path = directory.join(format!("{}--{}.json", purpose.name(), id));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            let bytes = serde_json::to_vec(&entry).map_err(|_| "secret-index-unavailable")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "secret-index-unavailable")?;
            #[cfg(unix)]
            fs::File::open(&directory)
                .and_then(|file| file.sync_all())
                .map_err(|_| "secret-index-unavailable")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_entry(&path)? != entry {
                return Err("secret-index-corrupt".into());
            }
        }
        Err(_) => return Err("secret-index-unavailable".into()),
    }
    write()
}

fn remove_with(
    root: &Path,
    mut remove: impl FnMut(Purpose, &str) -> Result<(), String>,
) -> Result<(), String> {
    let _lock = INDEX_LOCK.lock().map_err(|_| "secret-index-unavailable")?;
    let directory = root.join(DIRECTORY);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("secret-index-unavailable".into()),
    };
    if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) {
        return Err("secret-index-corrupt".into());
    }
    let mut entries = Vec::new();
    for file in fs::read_dir(&directory).map_err(|_| "secret-index-unavailable")? {
        let path = file.map_err(|_| "secret-index-unavailable")?.path();
        let entry = read_entry(&path)?;
        validate(root, &entry)?;
        if path.file_name().and_then(|name| name.to_str())
            != Some(format!("{}--{}.json", entry.purpose.name(), entry.id).as_str())
        {
            return Err("secret-index-corrupt".into());
        }
        entries.push((path, entry));
    }
    // Validate the complete inventory before deleting any credential.
    for (path, entry) in entries {
        remove(entry.purpose, &entry.id)?;
        fs::remove_file(path).map_err(|_| "secret-index-unavailable")?;
    }
    fs::remove_dir(directory).map_err(|_| "secret-index-unavailable".into())
}

pub(crate) fn remove_all(root: &Path) -> Result<(), String> {
    remove_with(root, |purpose, id| match purpose {
        Purpose::ServerSync => crate::server_sync::credentials::remove_owned(root, id),
        _ => crate::external_storage::secrets::remove_owned(root, purpose, id),
    })?;
    #[cfg(target_os = "android")]
    {
        crate::external_storage::secrets::remove_android_keys()?;
        crate::server_sync::credentials::remove_android_keys()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_creation_is_tracked_and_locked_store_remains_retryable() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(
            tracked_write(root.path(), Purpose::Provider, &id, || Err::<(), _>(
                "locked".into()
            ))
            .is_err()
        );
        assert!(remove_with(root.path(), |purpose, found| {
            assert_eq!(purpose, Purpose::Provider);
            assert_eq!(found, id);
            Err("locked".into())
        })
        .is_err());
        let mut removed = 0;
        remove_with(root.path(), |_, _| {
            removed += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(removed, 1);
        remove_with(root.path(), |_, _| panic!("already removed")).unwrap();
    }

    #[test]
    fn corrupt_inventory_blocks_all_deletion() {
        let root = tempfile::tempdir().unwrap();
        tracked_write(
            root.path(),
            Purpose::ServerSync,
            &uuid::Uuid::new_v4().to_string(),
            || Ok(()),
        )
        .unwrap();
        fs::write(root.path().join(DIRECTORY).join("corrupt.json"), b"{}").unwrap();
        assert_eq!(
            remove_with(root.path(), |_, _| panic!("must validate first")).unwrap_err(),
            "secret-index-corrupt"
        );
    }

    #[test]
    fn invalid_or_unwritable_inventory_never_creates_a_secret() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            tracked_write(root.path(), Purpose::Provider, "../outside", || {
                panic!("invalid identity must never reach the OS store");
                #[allow(unreachable_code)]
                Ok(())
            })
            .is_err()
        );
        fs::write(root.path().join(DIRECTORY), b"blocked").unwrap();
        assert!(tracked_write(
            root.path(),
            Purpose::ServerSync,
            &uuid::Uuid::new_v4().to_string(),
            || {
                panic!("inventory must be durable before secret creation");
                #[allow(unreachable_code)]
                Ok(())
            }
        )
        .is_err());
    }

    #[test]
    fn cleanup_removes_every_exact_owned_identity_without_reading_secrets() {
        let root = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        for purpose in [
            Purpose::Provider,
            Purpose::RepositoryKey,
            Purpose::ServerSync,
            Purpose::AccountCredential,
        ] {
            let id = if purpose == Purpose::AccountCredential {
                account_id(root.path())
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            tracked_write(root.path(), purpose, &id, || Ok(())).unwrap();
            expected.push((purpose, id));
        }
        remove_with(root.path(), |purpose, id| {
            let position = expected
                .iter()
                .position(|entry| entry.0 == purpose && entry.1 == id)
                .unwrap();
            expected.remove(position);
            Ok(())
        })
        .unwrap();
        assert!(expected.is_empty());
    }

    #[test]
    fn account_slots_are_root_scoped_and_index_contains_only_identity() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        assert_ne!(account_id(first.path()), account_id(second.path()));
        let id = account_id(first.path());
        tracked_write(first.path(), Purpose::AccountCredential, &id, || Ok(())).unwrap();
        tracked_write(first.path(), Purpose::AccountCredential, &id, || Ok(())).unwrap();
        let mut count = 0;
        remove_with(first.path(), |purpose, found| {
            assert_eq!(purpose, Purpose::AccountCredential);
            assert_eq!(found, id);
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
    }
}
