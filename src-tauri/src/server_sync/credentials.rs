//! Device credentials never enter PDS, CAS, exports or sync projections.
use super::{client::ServerConfig, Result, SyncError};
use crate::device_secrets::{self as device, Purpose};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StoredConfig {
    pub endpoint: String,
    pub library_id: String,
    pub device_id: String,
    credential_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Secrets {
    token: String,
    directory: Option<risunest_sync_connect::Directory>,
}

#[track_caller]
fn unavailable() -> SyncError {
    SyncError::new("device-credential-unavailable", 409)
}
impl StoredConfig {
    pub fn persist(root: &Path, config: &ServerConfig) -> Result<Self> {
        config.validate()?;
        let stored = Self {
            endpoint: config.endpoint.clone(),
            library_id: config.library_id.clone(),
            device_id: config.device_id.clone(),
            credential_id: uuid::Uuid::new_v4().to_string(),
        };
        let bytes = serde_json::to_vec(&Secrets {
            token: config.token.clone(),
            directory: config.directory.clone(),
        })
        .map_err(|_| unavailable())?;
        device::write_new(root, Purpose::ServerSync, &stored.credential_id, &bytes).map_err(|_| unavailable())?;
        Ok(stored)
    }
    pub fn resolve(&self, root: &Path) -> Result<ServerConfig> {
        self.validate_id()?;
        let secrets: Secrets = serde_json::from_slice(&device::read(root, Purpose::ServerSync, &self.credential_id).map_err(|_| unavailable())?)
            .map_err(|_| unavailable())?;
        let config = ServerConfig {
            directory: secrets.directory,
            endpoint: self.endpoint.clone(),
            library_id: self.library_id.clone(),
            device_id: self.device_id.clone(),
            token: secrets.token,
        };
        config.validate()?;
        Ok(config)
    }
    pub fn remove(&self, root: &Path) -> Result<()> {
        self.validate_id()?;
        device::remove(root, Purpose::ServerSync, &self.credential_id).map_err(|_| unavailable())
    }
    fn validate_id(&self) -> Result<()> {
        if uuid::Uuid::parse_str(&self.credential_id)
            .is_ok_and(|id| id.to_string() == self.credential_id)
        {
            Ok(())
        } else {
            Err(unavailable())
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;

    #[test]
    fn synthetic_credentials_roundtrip_through_keychain_and_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let config = ServerConfig {
            directory: Some(
                risunest_sync_connect::generate_directory("https://registry.example".into())
                    .unwrap(),
            ),
            endpoint: "http://127.0.0.1:1".into(),
            library_id: "macos-synthetic-library".into(),
            device_id: "macos-synthetic-device".into(),
            token: "ab".repeat(32),
        };
        let stored = StoredConfig::persist(root.path(), &config).unwrap();
        // Only the newly generated credential ID is touched, including on assertion failure.
        struct Cleanup<'a>(&'a StoredConfig, &'a Path);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.remove(self.1);
            }
        }
        let _cleanup = Cleanup(&stored, root.path());
        let metadata = serde_json::to_string(&stored).unwrap();
        let directory = config.directory.as_ref().unwrap();
        assert!(!metadata.contains(&config.token));
        assert!(!metadata.contains(&directory.key));
        assert!(!metadata.contains(&directory.uuid));
        assert_eq!(stored.resolve(root.path()).unwrap().token, config.token);
        assert_eq!(
            stored.resolve(root.path()).unwrap().directory.unwrap().key,
            directory.key
        );
        let inventory = root.path().join("owned-secret-index");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        let marker = std::fs::read_dir(inventory).unwrap().next().unwrap().unwrap();
        let ownership = std::fs::read_to_string(marker.path()).unwrap();
        assert!(ownership.contains(&stored.credential_id));
        assert!(!ownership.contains(&config.token));
        assert!(!ownership.contains(&directory.key));
        stored.remove(root.path()).unwrap();
        assert!(stored.resolve(root.path()).is_err());
        crate::cleanup_secrets::remove_all(root.path()).unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn credential_is_os_protected_separate_and_corruption_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let config = ServerConfig {
            directory: Some(
                risunest_sync_connect::generate_directory("https://registry.example".into())
                    .unwrap(),
            ),
            endpoint: "http://127.0.0.1:1".into(),
            library_id: "fixture".into(),
            device_id: "device".into(),
            token: "ab".repeat(32),
        };
        let stored = StoredConfig::persist(root.path(), &config).unwrap();
        let text = serde_json::to_string(&stored).unwrap();
        assert!(!text.contains(&config.token));
        let directory = config.directory.as_ref().unwrap();
        assert!(!text.contains(&directory.key));
        assert!(!text.contains(&directory.uuid));
        assert_eq!(
            stored.resolve(root.path()).unwrap().directory.unwrap().key,
            directory.key
        );
        assert_eq!(stored.resolve(root.path()).unwrap().token, config.token);
        let path = root
            .path()
            .join("server-sync-credentials")
            .join(&stored.credential_id);
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(64).any(|b| b == config.token.as_bytes()));
        bytes[20] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(stored.resolve(root.path()).is_err());
        stored.remove(root.path()).unwrap();
        assert!(stored.resolve(root.path()).is_err());
    }
}
