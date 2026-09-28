//! Directories nothing can own again: what a crashed publication was still
//! building, what a finished restore downloaded, and what a removed connection
//! kept. Runs at startup, before any job can claim a connection.
use super::{
    connection_store::ConnectionStore,
    contract::Result,
    job_store::JobStore,
    receive_artifacts::{managed_directory, remove_tree},
    runtime::{connection_directory, local_error},
    runtime_restore::discard_finished_staging,
};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

fn connection_directories(root: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(root.join("external-storage")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other?,
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.len() == 64 && name.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            found.push((name, entry.path()));
        }
    }
    Ok(found)
}

fn remove_managed(root: &Path, directory: &Path) -> io::Result<()> {
    if managed_directory(root, directory)? {
        remove_tree(directory)?;
    }
    Ok(())
}

pub(crate) fn remove_connection_directory(root: &Path, connection_id: &str) -> io::Result<()> {
    remove_managed(root, &connection_directory(root, connection_id))
}

pub(crate) fn remove_at_startup(root: &Path) {
    if !root.join("external-storage").is_dir() {
        return;
    }
    if let Err(error) = remove_unowned(root) {
        crate::nlog!("warn", "External storage leftovers were not removed: {error}");
    }
}

fn remove_unowned(root: &Path) -> Result<()> {
    let directories = connection_directories(root).map_err(local_error)?;
    let live: BTreeSet<String> = ConnectionStore::open(root)?
        .ids()?
        .iter()
        .map(|id| connection_directory(root, id))
        .filter_map(|path| path.file_name()?.to_str().map(str::to_owned))
        .collect();
    for (name, directory) in directories {
        let removed = if live.contains(&name) {
            remove_managed(root, &directory.join("package-cache").join("build"))
        } else {
            remove_managed(root, &directory)
        };
        if let Err(error) = removed {
            crate::nlog!("warn", "External storage leftover was not removed: {error}");
        }
    }
    if root.join("external-jobs.sqlite").is_file() {
        for job in JobStore::open(root)?.list()? {
            discard_finished_staging(root, &job);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_storage::job_store::{DurableJob, JobKind};
    use crate::external_storage::runtime::job_directory;

    fn write(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"synthetic").unwrap();
    }

    #[test]
    fn startup_removes_a_removed_connection_and_keeps_other_entries() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let removed = connection_directory(root, "removed-connection");
        write(&removed.join("package-cache").join("snapshot-cache.sqlite"));
        write(&removed.join("jobs").join("job").join("transfers.sqlite"));
        let captures = root.join("external-storage").join("captures").join("capture");
        write(&captures.join("capture.sqlite"));
        let other = root.join("external-storage").join("A".repeat(64));
        write(&other.join("kept"));
        remove_at_startup(root);
        assert!(!removed.exists());
        assert!(captures.join("capture.sqlite").is_file());
        assert!(other.join("kept").is_file());
    }

    fn store_connection(root: &Path, table: &str, id: &str) {
        drop(ConnectionStore::open(root).unwrap());
        rusqlite::Connection::open(root.join("external-connections.sqlite"))
            .unwrap()
            .execute(&format!("INSERT INTO {table}(id,value) VALUES(?1,'{{}}')"), [id])
            .unwrap();
    }

    fn restore(root: &Path, connection: &str, state: &str) -> PathBuf {
        let request = serde_json::from_value(serde_json::json!({
            "connectionId": connection, "kind": "restore",
            "snapshotId": "snapshot", "targetRevision": "1"
        }))
        .unwrap();
        let identity = crate::persistent_store::sync_selection::CaptureIdentity {
            store_id: "store".into(),
            library_epoch: "library".into(),
            generation: "generation".into(),
            selection_epoch: "selection".into(),
            revision: 1,
        };
        let mut job = DurableJob::new(request, false, 1, identity);
        assert_eq!(job.request.kind, JobKind::Restore);
        job.summary["state"] = serde_json::json!(state);
        JobStore::open(root).unwrap().put(&job).unwrap();
        let staging = job_directory(root, connection, &job.id).join("restore-snapshot");
        write(&staging.join("plaintext").join("record"));
        staging
    }

    #[test]
    fn startup_clears_live_connection_builds_and_finished_restores_only() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        store_connection(root, "connections", "live");
        store_connection(root, "pending_connections", "pending");
        let live = connection_directory(root, "live");
        write(&live.join("package-cache").join("build").join("stale-pack"));
        write(&live.join("package-cache").join("snapshot-cache.sqlite"));
        let pending = connection_directory(root, "pending");
        write(&pending.join("jobs").join("job").join("transfers.sqlite"));
        let failed = restore(root, "live", "failed");
        let cancelled = restore(root, "live", "cancelled");
        let paused = restore(root, "live", "waiting");
        remove_at_startup(root);
        assert!(!live.join("package-cache").join("build").exists());
        assert!(live.join("package-cache").join("snapshot-cache.sqlite").is_file());
        assert!(pending.join("jobs").join("job").join("transfers.sqlite").is_file());
        assert!(!failed.exists());
        assert!(!cancelled.exists());
        assert!(paused.join("plaintext").join("record").is_file());
    }

    #[test]
    fn no_external_storage_means_no_store_is_created() {
        let root = tempfile::tempdir().unwrap();
        remove_at_startup(root.path());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    fn link_directory(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn link_directory(target: &Path, link: &Path) {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "create junction: {output:?}");
    }

    #[test]
    fn a_removed_connection_directory_is_deleted_but_never_through_a_link() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let directory = connection_directory(root, "connection");
        write(&directory.join("package-cache").join("build").join("pack"));
        remove_connection_directory(root, "connection").unwrap();
        assert!(!directory.exists());
        remove_connection_directory(root, "connection").unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("kept"));
        link_directory(outside.path(), &directory);
        assert!(remove_connection_directory(root, "connection").is_err());
        assert!(outside.path().join("kept").is_file());
    }
}
