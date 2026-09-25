//! Local server-client storage only. Callers hold native server admission for
//! mutations and recheck pending work and source leases immediately before use.
use super::{backups, Result, SyncError};
use crate::trust_boundary::{is_link_like, is_lower_hex_256, sync_directory};
use risunest_small_object_store as small_object_store;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const PAGE_SIZE: usize = 100;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BackupCursor {
    pub created_at: u64,
    pub id: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedBackup {
    #[serde(flatten)]
    pub backup: backups::Backup,
    pub disk_bytes: u64,
    pub deletable: bool,
    pub blocked_reason: Option<String>,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackupInventory {
    pub items: Vec<ManagedBackup>,
    pub next: Option<BackupCursor>,
    pub complete_count: u64,
    pub complete_bytes: u64,
    pub incomplete_count: u64,
    pub incomplete_bytes: u64,
    pub disk_bytes: u64,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheUsage {
    pub total_bytes: u64,
    pub protected_bytes: u64,
    pub reclaimable_bytes: u64,
    /// What the object databases and their write-ahead logs allocate on disk.
    /// Deleting a body makes its pages reusable inside that allocation, so this
    /// is reported apart from the body bytes counted above it.
    pub database_bytes: u64,
    pub blocked_reason: Option<String>,
}

fn checked_metadata(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link_like(&metadata) || !(metadata.is_dir() || metadata.is_file()) {
        return Err(SyncError::new("invalid-management-path", 409));
    }
    Ok(metadata)
}
fn children(path: &Path) -> Result<Option<fs::ReadDir>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(metadata) if is_link_like(&metadata) || !metadata.is_dir() => {
            Err(SyncError::new("invalid-management-path", 409))
        }
        Ok(_) => Ok(Some(fs::read_dir(path)?)),
    }
}
fn visit_files(path: &Path, visitor: &mut impl FnMut(&Path, u64) -> Result<()>) -> Result<()> {
    let metadata = checked_metadata(path)?;
    if metadata.is_file() {
        return visitor(path, metadata.len());
    }
    for entry in fs::read_dir(path)? {
        visit_files(&entry?.path(), visitor)?;
    }
    Ok(())
}
fn tree_bytes(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    visit_files(path, &mut |_, bytes| {
        total = total
            .checked_add(bytes)
            .ok_or_else(|| SyncError::new("storage-size-overflow", 409))?;
        Ok(())
    })?;
    Ok(total)
}
fn server_root(root: &Path) -> Result<Option<PathBuf>> {
    let path = root.join("server-sync");
    Ok(children(&path)?.map(|_| path))
}

pub(crate) fn inventory(
    root: &Path,
    before: Option<&BackupCursor>,
    blocked: Option<&str>,
    pinned: &BTreeSet<String>,
) -> Result<BackupInventory> {
    let mut result = BackupInventory::default();
    let Some(server) = server_root(root)? else {
        return Ok(result);
    };
    let Some(entries) = children(&server.join("backups"))? else {
        return Ok(result);
    };
    let mut page = BTreeMap::new();
    let mut matching = 0usize;
    for entry in entries {
        let entry = entry?;
        let bytes = tree_bytes(&entry.path())?;
        result.disk_bytes += bytes;
        let id = entry.file_name().to_string_lossy().into_owned();
        match backups::inspect(root, &id) {
            Ok(backup) => {
                result.complete_count += 1;
                result.complete_bytes += backup.local_bytes + backup.remote_bytes;
                let key = (backup.created_at, id.clone());
                if before.is_some_and(|cursor| key >= (cursor.created_at, cursor.id.clone())) {
                    continue;
                }
                matching += 1;
                let reason = blocked.or_else(|| pinned.contains(&id).then_some("backup-in-use"));
                page.insert(
                    key,
                    ManagedBackup {
                        backup,
                        disk_bytes: bytes,
                        deletable: reason.is_none(),
                        blocked_reason: reason.map(str::to_owned),
                    },
                );
                if page.len() > PAGE_SIZE {
                    page.pop_first();
                }
            }
            Err(_) => {
                result.incomplete_count += 1;
                result.incomplete_bytes += bytes;
            }
        }
    }
    if matching > PAGE_SIZE {
        let ((created_at, id), _) = page.first_key_value().expect("nonempty bounded page");
        result.next = Some(BackupCursor {
            created_at: *created_at,
            id: id.clone(),
        });
    }
    result.items = page.into_values().rev().collect();
    Ok(result)
}

fn require_unblocked(blocked: Option<&str>) -> Result<()> {
    if let Some(code) = blocked {
        return Err(SyncError::new(code, 409));
    }
    Ok(())
}
fn remove_owned_tree(path: &Path) -> Result<()> {
    let metadata = checked_metadata(path)?;
    if metadata.is_file() {
        fs::remove_file(path)?;
    } else {
        for entry in fs::read_dir(path)? {
            remove_owned_tree(&entry?.path())?;
        }
        fs::remove_dir(path)?;
    }
    Ok(())
}
pub(crate) fn delete_backup(
    root: &Path,
    id: &str,
    blocked: Option<&str>,
    pinned: &BTreeSet<String>,
) -> Result<()> {
    require_unblocked(blocked)?;
    if pinned.contains(id) {
        return Err(SyncError::new("backup-in-use", 409));
    }
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| SyncError::new("invalid-backup-id", 400))?;
    if uuid.to_string() != id {
        return Err(SyncError::new("invalid-backup-id", 400));
    }
    let server = server_root(root)?.ok_or_else(|| SyncError::new("backup-not-found", 404))?;
    let base = server.join("backups");
    children(&base)?.ok_or_else(|| SyncError::new("backup-not-found", 404))?;
    // The tombstone is created only after validating a completed backup. A
    // restart may resume this exact requested deletion without parsing a
    // partly deleted receipt. Incomplete preservation directories never enter it.
    let deleting = base.join(format!(".deleting-{id}"));
    match fs::symlink_metadata(&deleting) {
        Ok(_) => {
            tree_bytes(&deleting)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            backups::inspect(root, id)?;
            let source = backups::directory(root, id)?;
            tree_bytes(&source)?;
            fs::rename(source, &deleting)?;
            sync_directory(&base)?;
        }
        Err(e) => return Err(e.into()),
    }
    remove_owned_tree(&deleting)?;
    sync_directory(&base)?;
    Ok(())
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeletionCleanup {
    pub removed: u64,
    pub failed: u64,
}

/// Only durable deletion tombstones authorize unattended removal. Unfinished
/// backups have no such authorization and must remain untouched.
pub(crate) fn cleanup_deleted_backups(
    root: &Path,
    blocked: Option<&str>,
    pinned: &BTreeSet<String>,
) -> Result<DeletionCleanup> {
    require_unblocked(blocked)?;
    let mut result = DeletionCleanup::default();
    let Some(server) = server_root(root)? else {
        return Ok(result);
    };
    let Some(entries) = children(&server.join("backups"))? else {
        return Ok(result);
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                result.failed += 1;
                continue;
            }
        };
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix(".deleting-"))
        else {
            continue;
        };
        if !uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
            continue;
        }
        if !checked_metadata(&entry.path()).is_ok_and(|metadata| metadata.is_dir()) {
            result.failed += 1;
            continue;
        }
        match delete_backup(root, id, blocked, pinned) {
            Ok(()) => result.removed += 1,
            Err(_) => result.failed += 1,
        }
    }
    Ok(result)
}

fn cache_object_hash(root: &Path, file: &Path) -> Option<String> {
    let parts = file
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    if parts.len() != 4 || parts[0] != "assets" || parts[1] != "objects" || parts[2].len() != 2 {
        return None;
    }
    let hash = format!("{}{}", parts[2], parts[3]);
    is_lower_hex_256(&hash).then_some(hash)
}
pub(crate) fn cache_usage(
    root: &Path,
    active_cache: Option<&str>,
    references: &BTreeSet<String>,
    blocked: Option<&str>,
    clean: bool,
) -> Result<CacheUsage> {
    if clean {
        require_unblocked(blocked)?;
    }
    let mut usage = CacheUsage {
        blocked_reason: blocked.map(str::to_owned),
        ..Default::default()
    };
    let Some(server) = server_root(root)? else {
        return Ok(usage);
    };
    for entry in fs::read_dir(&server)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "backups" {
            continue;
        }
        let cache = entry.path();
        visit_files(&cache, &mut |file, bytes| {
            if is_object_database(&cache, file) {
                // An allocation, counted once below, whose contents are
                // accounted row by row rather than as bytes on disk.
                return Ok(());
            }
            let reclaimable = blocked.is_none()
                && is_lower_hex_256(&name)
                && cache_object_hash(&cache, file).is_some_and(|hash| {
                    active_cache != Some(name.as_str()) || !references.contains(&hash)
                });
            if reclaimable && clean {
                fs::remove_file(file)?;
                return Ok(());
            }
            usage.total_bytes += bytes;
            if reclaimable {
                usage.reclaimable_bytes += bytes;
            } else {
                usage.protected_bytes += bytes;
            }
            Ok(())
        })?;
        sweep_object_database(
            &cache.join(OBJECT_DATABASE),
            blocked.is_none() && is_lower_hex_256(&name),
            &mut |hash| active_cache != Some(name.as_str()) || !references.contains(hash),
            clean,
            &mut usage,
        )?;
        // Measured after the sweep, so a cleanup reports what it left behind.
        let allocated = database_bytes(&cache)?;
        usage.total_bytes += allocated;
        usage.database_bytes += allocated;
    }
    Ok(usage)
}

const OBJECT_DATABASE: &str = "objects.sqlite";
const OBJECT_DATABASE_FILES: [&str; 3] =
    ["objects.sqlite", "objects.sqlite-wal", "objects.sqlite-shm"];

/// The derived cache's small-object database and the files SQLite keeps beside
/// it. Their bytes are an allocation, never a reclaimable cached object.
fn is_object_database(root: &Path, file: &Path) -> bool {
    file.parent() == Some(root)
        && file
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| OBJECT_DATABASE_FILES.contains(&name))
}

fn database_bytes(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    for name in OBJECT_DATABASE_FILES {
        match fs::symlink_metadata(root.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(metadata) if is_link_like(&metadata) || !metadata.is_file() => {
                return Err(SyncError::new("invalid-management-path", 409))
            }
            Ok(metadata) => total += metadata.len(),
        }
    }
    Ok(total)
}

/// Account for, and when cleaning remove, the small bodies this cache holds in
/// its object database. Returns whether anything was removed.
fn sweep_object_database(
    path: &Path,
    collectable: bool,
    reclaimable: &mut impl FnMut(&str) -> bool,
    clean: bool,
    usage: &mut CacheUsage,
) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let mut db = rusqlite::Connection::open(path)?;
    db.execute_batch("PRAGMA busy_timeout=5000;")?;
    let mut after = String::new();
    let mut removed = false;
    loop {
        let page = small_object_store::page(&db, &after, 1024)
            .map_err(|_| SyncError::new("cache-store-unavailable", 503))?;
        if page.is_empty() {
            break;
        }
        after = page[page.len() - 1].0.clone();
        let mut group = Vec::new();
        for (hash, bytes) in &page {
            if !collectable || !reclaimable(hash) {
                usage.protected_bytes += bytes;
            } else if clean {
                group.push(hash.as_str());
            } else {
                usage.reclaimable_bytes += bytes;
            }
        }
        if !group.is_empty() {
            let tx = db.transaction()?;
            small_object_store::delete_batch(&tx, &group)
                .map_err(|_| SyncError::new("cache-store-unavailable", 503))?;
            tx.commit()?;
            removed = true;
        }
    }
    if removed {
        // Return the freed pages to the filesystem without rewriting the whole
        // database, then move the write-ahead log's copy of them out too.
        db.execute_batch("PRAGMA incremental_vacuum; PRAGMA wal_checkpoint(TRUNCATE);")?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests;
