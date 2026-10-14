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

    fn secret(bytes: &[u8]) -> SecretBytes {
        SecretBytes(zeroize::Zeroizing::new(bytes.to_vec()))
    }

    struct RemoveSyntheticSecret { root: PathBuf, purpose: Purpose, id: String }
    impl Drop for RemoveSyntheticSecret {
        fn drop(&mut self) { let _ = device::remove(&self.root, self.purpose, &self.id); }
    }

    /// The one test that reaches this platform's credential store. Each
    /// purpose stores, replaces and removes its own secret, an empty provider
    /// password survives, and no purpose reads what another one stored.
    #[test]
    fn platform_store_keeps_each_purpose_in_its_own_namespace() {
        let root = tempfile::tempdir().unwrap();
        let provider = provider_vault(root.path());
        let keys = repository_key_vault(root.path());
        let slot = account_credential_slot(root.path());
        let _account = RemoveSyntheticSecret { root: root.path().to_owned(), purpose: Purpose::AccountCredential, id: slot.name.clone() };
        let empty = secret(b"");
        let replacement = secret(b"rotated-provider-token");
        let token = secret(br#"{"id":"synthetic","token":"synthetic-account-token"}"#);
        let rotated = secret(br#"{"id":"synthetic","token":"rotated-account-token"}"#);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let reference = provider.store(&empty).await.unwrap();
            let _provider = RemoveSyntheticSecret { root: root.path().to_owned(), purpose: Purpose::Provider,
                id: reference.0.strip_prefix("provider-v1:").unwrap().to_owned() };
            assert!(provider.read(&reference).await.unwrap().0.is_empty());
            provider.replace(&reference, &replacement).await.unwrap();
            assert_eq!(provider.read(&reference).await.unwrap().0.as_slice(), replacement.0.as_slice());
            #[cfg(windows)]
            {
            let sealed = std::fs::read(root.path().join("external-storage-secrets")
                .join(reference.0.strip_prefix("provider-v1:").unwrap())).unwrap();
            assert!(!sealed.windows(replacement.0.len()).any(|part| part == replacement.0.as_slice()));
            }
            provider.replace(&reference, &empty).await.unwrap();
            assert!(provider.read(&reference).await.unwrap().0.is_empty());

            let key = keys.store(&secret(&[7; 32])).await.unwrap();
            let _key = RemoveSyntheticSecret { root: root.path().to_owned(), purpose: Purpose::RepositoryKey,
                id: key.0.strip_prefix("repository-key-v1:").unwrap().to_owned() };
            assert_eq!(keys.read(&key).await.unwrap().0.as_slice(), &[7; 32]);
            assert!(keys.read(&reference).await.is_err());
            assert!(provider.read(&key).await.is_err());

            assert!(slot.read().is_none());
            slot.write(&token).unwrap();
            assert_eq!(slot.read().unwrap().0.as_slice(), token.0.as_slice());
            slot.write(&rotated).unwrap();
            assert_eq!(slot.read().unwrap().0.as_slice(), rotated.0.as_slice());
            #[cfg(windows)]
            {
            let sealed = std::fs::read(root.path().join("account-credentials").join(&slot.name)).unwrap();
            assert!(!sealed.windows(rotated.0.len()).any(|part| part == rotated.0.as_slice()));
            }
            assert!(provider.read(&SecretRef(format!("provider-v1:{}", slot.name))).await.is_err());

            provider.remove(&reference).await.unwrap();
            assert!(provider.read(&reference).await.is_err());
            keys.remove(&key).await.unwrap();
            assert!(keys.read(&key).await.is_err());
            slot.remove().unwrap();
            assert!(slot.read().is_none());
            slot.remove().unwrap();
        });
    }

    /// What the vault refuses before it asks the platform store, so these
    /// outcomes hold whether or not a store is reachable.
    #[test]
    fn oversized_empty_and_malformed_inputs_are_refused_before_the_store() {
        let root = tempfile::tempdir().unwrap();
        let provider = provider_vault(root.path());
        let keys = repository_key_vault(root.path());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let too_large = secret(&vec![0; MAX_PLAINTEXT_BYTES + 1]);
            assert_eq!(provider.store(&too_large).await.err().unwrap().kind, ErrorKind::Corrupt);
            assert_eq!(keys.store(&secret(b"")).await.err().unwrap().kind, ErrorKind::Corrupt);
            assert!(provider
                .read(&SecretRef("provider-v1:../outside".into()))
                .await
                .is_err());
        });
    }
}
