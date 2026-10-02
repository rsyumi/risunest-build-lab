//! Local server-client storage only. Callers hold native server admission for
//! mutations and recheck pending work and source leases immediately before use.
use super::{Result, SyncError};
use crate::trust_boundary::{is_link_like, is_lower_hex_256};
use risunest_small_object_store as small_object_store;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};


#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheUsage {
    /// `cache_bytes` plus `ledger_bytes`.
    pub total_bytes: u64,
    /// What the per-connection caches occupy: body files and object databases.
    pub cache_bytes: u64,
    /// The part of `cache_bytes` a cleanup keeps.
    pub protected_bytes: u64,
    /// The part of `cache_bytes` a cleanup removes, counted as body bytes.
    pub reclaimable_bytes: u64,
    /// Files kept beside the caches, such as the asset residency ledger.
    pub ledger_bytes: u64,
    /// What the object databases and their write-ahead logs allocate on disk,
    /// as part of `cache_bytes`.
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

fn server_root(root: &Path) -> Result<Option<PathBuf>> {
    let path = root.join("server-sync");
    Ok(children(&path)?.map(|_| path))
}


fn require_unblocked(blocked: Option<&str>) -> Result<()> {
    if let Some(code) = blocked {
        return Err(SyncError::new(code, 409));
    }
    Ok(())
}


/// Only durable deletion tombstones authorize unattended removal. Unfinished
/// backups have no such authorization and must remain untouched.


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
        let cache = entry.path();
        let metadata = checked_metadata(&cache)?;
        if metadata.is_file() {
            // A file beside the per-connection caches, such as the asset
            // residency ledger, is kept and has no object database.
            usage.ledger_bytes += metadata.len();
            continue;
        }
        let staging = cache.join("staging");
        let transfers = if staging.is_dir() && cache.join("transfers.sqlite").is_file() {
            checked_metadata(&cache.join("transfers.sqlite"))?;
            rusqlite::Connection::open_with_flags(cache.join("transfers.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
        } else { None };
        visit_files(&cache, &mut |file, bytes| {
            if is_object_database(&cache, file) {
                // An allocation, counted once below. The bodies a cleanup
                // would delete from it are counted row by row.
                return Ok(());
            }
            let orphan_chunk = file.parent().and_then(|directory| {
                if directory.parent() != Some(staging.as_path()) { return None; }
                directory.file_name().and_then(|name| name.to_str())
            }).filter(|target| is_lower_hex_256(target)).is_some_and(|target| {
                transfers.as_ref().is_some_and(|db| db.query_row::<bool, _, _>(
                    "SELECT NOT EXISTS(SELECT 1 FROM chunks WHERE target=?1)", [target], |row| row.get(0),
                ).unwrap_or(false))
            });
            let reclaimable = blocked.is_none()
                && is_lower_hex_256(&name)
                && (orphan_chunk || cache_object_hash(&cache, file).is_some_and(|hash| {
                    active_cache != Some(name.as_str()) || !references.contains(&hash)
                }));
            if reclaimable && clean {
                fs::remove_file(file)?;
                return Ok(());
            }
            usage.cache_bytes += bytes;
            if reclaimable {
                usage.reclaimable_bytes += bytes;
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
        usage.cache_bytes += allocated;
        usage.database_bytes += allocated;
    }
    usage.protected_bytes = usage.cache_bytes.saturating_sub(usage.reclaimable_bytes);
    usage.total_bytes = usage.cache_bytes + usage.ledger_bytes;
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
                continue;
            }
            if clean {
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
