//! Opens a pinned payload for an export writer. A body only the Sync server or
//! external storage holds is downloaded, and a cancel request stops that download.
//! A body that cannot be fetched fails with the code every export shares.

use super::NativeJobError;
use crate::asset_repository::PayloadCas;
use crate::local_backup::CancellationProbe;
use crate::server_sync::residency::HydrationSession;
use std::fs::File;
use std::io;

/// `None` when neither this device, the Sync server nor external storage keeps the
/// body. `local` maps errors from this device's own storage.
pub(super) fn open_available_payload(
    cas: &PayloadCas,
    hash: &str,
    cancellation: &dyn CancellationProbe,
    local: fn(io::Error) -> NativeJobError,
) -> Result<Option<File>, NativeJobError> {
    if let Some(file) = cas.open_object(hash).map_err(local)? {
        return Ok(Some(file));
    }
    let check = || {
        if cancellation.is_cancelled() {
            Err(crate::server_sync::SyncError::new("cancelled", 499))
        } else {
            Ok(())
        }
    };
    let mut session = HydrationSession::new(cas.repository_root(), cancellation.cancellation_flag())
        .map_err(|error| local(io::Error::other(error.code)))?;
    session.open(hash, &check).map_err(|error| {
        if cancellation.is_cancelled() {
            NativeJobError::new("cancelled", "payload download was cancelled")
        } else if error.code == "local-storage-full" {
            NativeJobError::new("local-storage-full", "downloaded payload could not be stored")
        } else {
            NativeJobError::new(
                "remote-asset-unavailable",
                format!("payload could not be downloaded: {}", error.code),
            )
        }
    })
}

pub(super) fn available_payload_size(
    cas: &PayloadCas,
    hash: &str,
    cancellation: &dyn CancellationProbe,
    local: fn(io::Error) -> NativeJobError,
) -> Result<Option<u64>, NativeJobError> {
    open_available_payload(cas, hash, cancellation, local)?
        .map(|file| file.metadata().map(|metadata| metadata.len()))
        .transpose()
        .map_err(local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_backup::NeverCancelled;
    use crate::persistent_store::PersistentStore;
    use crate::server_sync::residency::test_remote;

    fn local(error: io::Error) -> NativeJobError {
        NativeJobError::new("store-error", error.to_string())
    }

    #[test]
    fn a_body_kept_elsewhere_is_downloaded_or_reported_by_where_it_is_kept() {
        let directory = tempfile::tempdir().unwrap();
        let _store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let served = cas.prepare_bytes(b"synthetic served body").unwrap().content_hash;
        test_remote::keep_only_on_server(directory.path(), &served);
        let file = open_available_payload(&cas, &served, &NeverCancelled, local).unwrap().unwrap();
        assert_eq!(file.metadata().unwrap().len(), 21);
        assert!(cas.stat_object(&served).unwrap().is_some());

        // Custody is recorded, but the server cannot be reached.
        let unreachable = cas.prepare_bytes(b"synthetic unreachable body").unwrap();
        std::fs::remove_file(cas.object_path(&unreachable.content_hash).unwrap().unwrap()).unwrap();
        test_remote::hold(directory.path(), &[(&unreachable.content_hash, unreachable.byte_size)]);
        let error = open_available_payload(&cas, &unreachable.content_hash, &NeverCancelled, local).unwrap_err();
        assert_eq!(error.code, "remote-asset-unavailable");

        let missing = cas.prepare_bytes(b"synthetic missing body").unwrap().content_hash;
        std::fs::remove_file(cas.object_path(&missing).unwrap().unwrap()).unwrap();
        assert!(available_payload_size(&cas, &missing, &NeverCancelled, local).unwrap().is_none());
    }
}
