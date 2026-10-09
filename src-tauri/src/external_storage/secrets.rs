//! External-storage interfaces for the shared device secret adapter.
use super::{
    auth::{SecretBytes, SecretVault},
    contract::{ErrorKind, ProviderError, ProviderFuture, Result, SecretRef},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::device_secrets::{self as device, Purpose};
#[cfg(test)]
use crate::device_secrets::MAX_PLAINTEXT_BYTES;

fn map_error(error: device::Error) -> ProviderError {
    ProviderError::new(match error {
        device::Error::Missing => ErrorKind::ReauthRequired,
        device::Error::Unavailable => ErrorKind::DeviceVaultUnavailable,
        device::Error::Invalid => ErrorKind::Corrupt,
    })
}
fn unavailable() -> ProviderError { map_error(device::Error::Missing) }

pub(crate) struct NativeSecretVault {
    root: PathBuf,
    purpose: Purpose,
}

/// Provider credentials and sealed resumable-upload state share this vault.
/// The returned references contain only an opaque random identifier.
pub(crate) fn provider_vault(root: &Path) -> Arc<dyn SecretVault> {
    Arc::new(NativeSecretVault {
        root: root.to_owned(),
        purpose: Purpose::Provider,
    })
}

/// Repository root keys use an independent OS namespace from account tokens.
/// Independent recovery envelopes are created by the portable format core and
/// do not depend on this local vault.
pub(crate) fn repository_key_vault(root: &Path) -> Arc<dyn SecretVault> {
    Arc::new(NativeSecretVault {
        root: root.to_owned(),
        purpose: Purpose::RepositoryKey,
    })
}

/// One secret addressed by a root-scoped name instead of an issued reference.
pub(crate) struct NamedSecretSlot {
    root: PathBuf,
    purpose: Purpose,
    name: String,
}

/// The account token. An installation holds at most one, and a new device
/// obtains its own by signing in, so it never travels with stored data.
pub(crate) fn account_credential_slot(root: &Path) -> NamedSecretSlot {
    NamedSecretSlot {
        root: root.to_owned(),
        purpose: Purpose::AccountCredential,
        name: crate::cleanup_secrets::account_id(root),
    }
}

impl NamedSecretSlot {
    pub(crate) fn read(&self) -> Option<SecretBytes> {
        device::read(&self.root, self.purpose, &self.name).ok().map(|bytes| SecretBytes(zeroize::Zeroizing::new(bytes)))
    }
    pub(crate) fn write(&self, bytes: &SecretBytes) -> Result<()> {
        device::upsert(&self.root, self.purpose, &self.name, &bytes.0).map_err(map_error)
    }
    pub(crate) fn remove(&self) -> Result<()> {
        device::remove(&self.root, self.purpose, &self.name).map_err(map_error)
    }
}

impl NativeSecretVault {
    fn parse_reference(&self, reference: &SecretRef) -> Result<String> {
        let id = reference
            .0
            .strip_prefix(self.purpose.reference_prefix())
            .ok_or_else(unavailable)?;
        let parsed = uuid::Uuid::parse_str(id).map_err(|_| unavailable())?;
        if parsed.get_version_num() != 4 || parsed.to_string() != id {
            return Err(unavailable());
        }
        Ok(id.to_owned())
    }

    fn reference(&self, id: &str) -> SecretRef {
        SecretRef(format!("{}{id}", self.purpose.reference_prefix()))
    }
}

impl SecretVault for NativeSecretVault {
    fn read<'a>(&'a self, reference: &'a SecretRef) -> ProviderFuture<'a, SecretBytes> {
        Box::pin(async move {
            let id = self.parse_reference(reference)?;
            device::read(&self.root, self.purpose, &id).map(|bytes| SecretBytes(zeroize::Zeroizing::new(bytes))).map_err(map_error)
        })
    }
    fn store<'a>(&'a self, bytes: &'a SecretBytes) -> ProviderFuture<'a, SecretRef> {
        Box::pin(async move {
            let id = uuid::Uuid::new_v4().to_string();
            device::write_new(&self.root, self.purpose, &id, &bytes.0).map_err(map_error)?;
            Ok(self.reference(&id))
        })
    }
    fn replace<'a>(&'a self, reference: &'a SecretRef, bytes: &'a SecretBytes) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            let id = self.parse_reference(reference)?;
            device::replace(&self.root, self.purpose, &id, &bytes.0).map_err(map_error)
        })
    }
    fn remove<'a>(&'a self, reference: &'a SecretRef) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            let id = self.parse_reference(reference)?;
            device::remove(&self.root, self.purpose, &id).map_err(map_error)
        })
    }
}

#[cfg(all(test, any(windows, target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn empty_provider_password_roundtrips_and_can_replace_an_existing_password() {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let vault = provider_vault(root.path());
            let empty = SecretBytes(zeroize::Zeroizing::new(Vec::new()));
            let reference = vault.store(&empty).await.unwrap();
            let _cleanup = RemoveSyntheticSecret { root: root.path().to_owned(), purpose: Purpose::Provider,
                id: reference.0.strip_prefix("provider-v1:").unwrap().to_owned() };
            assert!(vault.read(&reference).await.unwrap().0.is_empty());
            vault.replace(&reference, &SecretBytes(zeroize::Zeroizing::new(b"synthetic".to_vec()))).await.unwrap();
            vault.replace(&reference, &empty).await.unwrap();
            assert!(vault.read(&reference).await.unwrap().0.is_empty());
            assert!(repository_key_vault(root.path()).store(&empty).await.is_err());
            vault.remove(&reference).await.unwrap();
        });
    }

    struct RemoveSyntheticSecret { root: PathBuf, purpose: Purpose, id: String }
    impl Drop for RemoveSyntheticSecret {
        fn drop(&mut self) { let _ = device::remove(&self.root, self.purpose, &self.id); }
    }

    #[test]
    fn purpose_bound_vault_roundtrips_replaces_and_removes() {
        let root = tempfile::tempdir().unwrap();
        let provider = provider_vault(root.path());
        let keys = repository_key_vault(root.path());
        let original = SecretBytes(zeroize::Zeroizing::new(
            b"synthetic-provider-token".to_vec(),
        ));
        let replacement = SecretBytes(zeroize::Zeroizing::new(b"rotated-provider-token".to_vec()));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let reference = provider.store(&original).await.unwrap();
            let _cleanup = RemoveSyntheticSecret { root: root.path().to_owned(), purpose: Purpose::Provider, id: reference.0.strip_prefix("provider-v1:").unwrap().to_owned() };
            assert_eq!(
                provider.read(&reference).await.unwrap().0.as_slice(),
                original.0.as_slice()
            );
            assert!(keys.read(&reference).await.is_err());
            provider.replace(&reference, &replacement).await.unwrap();
            assert_eq!(
                provider.read(&reference).await.unwrap().0.as_slice(),
                replacement.0.as_slice()
            );

            #[cfg(windows)]
            {
            let path = root
                .path()
                .join("external-storage-secrets")
                .join(reference.0.strip_prefix("provider-v1:").unwrap());
            let sealed = std::fs::read(path).unwrap();
            assert!(!sealed
                .windows(replacement.0.len())
                .any(|part| part == replacement.0.as_slice()));
            }

            provider.remove(&reference).await.unwrap();
            assert!(provider.read(&reference).await.is_err());
        });
    }

    #[test]
    fn the_account_slot_keeps_one_named_secret_in_its_own_namespace() {
        let root = tempfile::tempdir().unwrap();
        let slot = account_credential_slot(root.path());
        let _cleanup = RemoveSyntheticSecret { root: root.path().to_owned(), purpose: Purpose::AccountCredential, id: slot.name.clone() };
        let token = SecretBytes(zeroize::Zeroizing::new(
            br#"{"id":"synthetic","token":"synthetic-account-token"}"#.to_vec(),
        ));
        let rotated = SecretBytes(zeroize::Zeroizing::new(
            br#"{"id":"synthetic","token":"rotated-account-token"}"#.to_vec(),
        ));

        assert!(slot.read().is_none());
        slot.write(&token).unwrap();
        assert_eq!(slot.read().unwrap().0.as_slice(), token.0.as_slice());
        slot.write(&rotated).unwrap();
        assert_eq!(slot.read().unwrap().0.as_slice(), rotated.0.as_slice());

        #[cfg(windows)]
        {
        let sealed =
            std::fs::read(root.path().join("account-credentials").join(&slot.name)).unwrap();
        assert!(!sealed
            .windows(rotated.0.len())
            .any(|part| part == rotated.0.as_slice()));
        }

        // The provider vault namespace cannot reach the account slot.
        let provider = provider_vault(root.path());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            assert!(provider
                .read(&SecretRef("provider-v1:official-account".into()))
                .await
                .is_err());
        });

        slot.remove().unwrap();
        assert!(slot.read().is_none());
        slot.remove().unwrap();
    }

    #[test]
    fn corrupt_cross_purpose_and_oversized_values_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let provider = provider_vault(root.path());
        let keys = repository_key_vault(root.path());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let key = SecretBytes(zeroize::Zeroizing::new(vec![7; 32]));
            let reference = keys.store(&key).await.unwrap();
            let _cleanup = RemoveSyntheticSecret { root: root.path().to_owned(), purpose: Purpose::RepositoryKey, id: reference.0.strip_prefix("repository-key-v1:").unwrap().to_owned() };
            assert!(provider.read(&reference).await.is_err());
            assert!(provider
                .read(&SecretRef("provider-v1:../outside".into()))
                .await
                .is_err());
            let too_large = SecretBytes(zeroize::Zeroizing::new(vec![0; MAX_PLAINTEXT_BYTES + 1]));
            assert!(provider.store(&too_large).await.is_err());
            keys.remove(&reference).await.unwrap();
        });
    }
}
