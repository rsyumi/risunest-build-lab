use super::snapshot_archive::Archive;
use super::{
    active_generation, current_revision, CheckpointMode, ReadTarget, SnapshotCreated, SnapshotInfo,
    StoreError, StoreResult, DATABASE_FILE, GENERATION_TABLES,
};
use crate::asset_repository::migration_gc::AssetRootSet;
use crate::asset_repository::PayloadCas;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

const MIN_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;
const UNSCANNABLE_BLOCKER: &str = "record-unscannable";
const CAS_PHYSICAL_PREFIX: &[u8] = b"assets/objects/";
const COLD_STORAGE_HEADER: &str = "\u{ef01}COLDSTORAGE\u{ef01}";

#[cfg(test)]
thread_local! {
    pub(super) static ASSET_ROOT_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Default)]
pub(crate) struct ActiveReaderRegistry {
    count: AtomicUsize,
    deferred_asset_inventories: AtomicUsize,
    detached_asset_roots: Mutex<HashMap<String, AssetRootSet>>,
}

impl ActiveReaderRegistry {
    /// Short local captures pin their referenced objects incrementally. During
    /// that interval GC/eviction must defer instead of rescanning the full asset
    /// library on every small edit. Register under the repository mutation lock.
    pub(crate) fn defer_asset_inventory(self: &Arc<Self>) -> DeferredAssetInventory {
        self.deferred_asset_inventories
            .fetch_add(1, Ordering::SeqCst);
        DeferredAssetInventory(Arc::clone(self))
    }
    fn register(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    fn release(&self) {
        let previous = self.count.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0, "active reader registry underflow");
    }

    pub(crate) fn active_count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn publish_detached_asset_roots(&self, lease: &str, roots: AssetRootSet) -> StoreResult<()> {
        self.detached_asset_roots
            .lock()
            .map_err(|error| StoreError::Store {
                message: format!("active reader roots mutex poisoned: {error}"),
            })?
            .insert(lease.to_owned(), roots);
        Ok(())
    }

    fn remove_detached_asset_roots(&self, lease: &str) {
        if let Ok(mut roots) = self.detached_asset_roots.lock() {
            roots.remove(lease);
        }
    }

    /// A local capture is between writing its bodies and registering them.
    pub(crate) fn asset_inventory_deferred(&self) -> bool {
        self.deferred_asset_inventories.load(Ordering::SeqCst) > 0
    }

    pub(crate) fn detached_asset_roots(&self) -> StoreResult<Vec<AssetRootSet>> {
        if self.deferred_asset_inventories.load(Ordering::SeqCst) > 0 {
            return Err(StoreError::Validation {
                message: "Asset inventory is being pinned by a local capture".into(),
            });
        }
        Ok(self
            .detached_asset_roots
            .lock()
            .map_err(|error| StoreError::Store {
                message: format!("active reader roots mutex poisoned: {error}"),
            })?
            .values()
            .cloned()
            .collect())
    }
}

pub(crate) struct DeferredAssetInventory(Arc<ActiveReaderRegistry>);
impl Drop for DeferredAssetInventory {
    fn drop(&mut self) {
        let previous = self
            .0
            .deferred_asset_inventories
            .fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0);
    }
}

pub(crate) struct RevisionReadLease {
    pub(crate) connection: Connection,
    pub(crate) target: ReadTarget,
    lease: String,
    repository_root: PathBuf,
    active_readers: Arc<ActiveReaderRegistry>,
    transaction_open: bool,
    #[cfg(test)]
    pub(crate) acquired_at: Instant,
}

impl RevisionReadLease {
    pub(crate) fn active_readers(&self) -> Arc<ActiveReaderRegistry> {
        Arc::clone(&self.active_readers)
    }

    pub(crate) fn publish_detached_asset_roots(&self) -> StoreResult<()> {
        let roots = collect_asset_roots(&self.connection)?;
        self.active_readers
            .publish_detached_asset_roots(&self.lease, roots)
    }

    #[cfg(feature = "native-official-publication")]
    pub(crate) fn asset_roots(&self) -> StoreResult<AssetRootSet> {
        collect_asset_roots_for_generation(&self.connection, &self.target.generation)
    }
}

impl Drop for RevisionReadLease {
    fn drop(&mut self) {
        if self.transaction_open {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
        self.active_readers.remove_detached_asset_roots(&self.lease);
        self.active_readers.release();
    }
}

fn prepare_restore_candidate(persistent_dir: &Path, target: &Path) -> StoreResult<PathBuf> {
    let candidate = persistent_dir.join(format!(
        "{DATABASE_FILE}.restore-candidate-{}",
        Uuid::new_v4()
    ));
    fs::copy(target, &candidate)
        .map_err(|error| path_error("copy restore candidate", &candidate, error))?;

    let result = (|| -> StoreResult<()> {
        let mut connection = Connection::open(&candidate)?;
        super::schema::initialize(&mut connection)?;
        let transaction = connection.transaction()?;
        super::message_pages::accept_copied_database(&transaction)?;
        transaction.commit()?;
        let generation = active_generation(&connection)?;
        let reader = open_generation_reader(&candidate, &generation)?;
        super::portable_validation::validate_records(&reader, &crate::local_backup::NeverCancelled)?;
        drop(reader);
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(validation("migrated snapshot integrity check failed"));
        }
        checkpoint(&connection, CheckpointMode::Truncate)?;
        drop(connection);
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&candidate)?
            .sync_all()?;
        Ok(())
    })();

    if result.is_err() {
        if let Err(error) = remove_database_files(&candidate) {
            crate::nlog!(
                "warn",
                "persistent restore candidate cleanup skipped: {error}"
            );
        }
    }
    result?;
    Ok(candidate)
}

pub(super) fn sweep_temporary_generations(
    connection: &mut Connection,
    retained_stage: Option<&str>,
) -> StoreResult<()> {
    let transaction = connection.transaction()?;
    if let Some(retained_stage) = retained_stage {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM root WHERE generation=?1)",
            [retained_stage],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::Validation {
                message: "Native portable restore stage is missing".into(),
            });
        }
    }
    let legacy_generations = {
        let mut statement = transaction.prepare("SELECT generation FROM snapshot_leases")?;
        let generations = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        generations
    };
    transaction.execute("DELETE FROM snapshot_leases", [])?;
    let active = active_generation(&transaction)?;
    let mut statement = transaction.prepare(
        "SELECT generation FROM root
         WHERE generation LIKE 'staging-%'
            OR generation LIKE 'snapshot-%'",
    )?;
    let mut stale = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    stale.extend(legacy_generations);
    stale.sort();
    stale.dedup();
    for generation in stale {
        let binding_stage: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM lww_binding_sources s JOIN lww_binding_stages b
             ON b.staging_id=s.staging_id AND b.receive_id=s.request_id AND b.inspection_id=s.inspection_id
             WHERE s.staging_id=?1)",
            [&generation], |row| row.get(0),
        )?;
        let snapshot_stage: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM snapshot_restore_stages WHERE stage_id=?1 AND state='staged')",[&generation],|row|row.get(0))?;
        if generation != active && retained_stage != Some(generation.as_str()) && !binding_stage && !snapshot_stage {
            delete_generation(&transaction, &generation)?;
        }
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn acquire_revision(
    database_path: &Path,
    revision: i64,
    active_readers: Arc<ActiveReaderRegistry>,
) -> StoreResult<(String, RevisionReadLease)> {
    let repository_root = repository_root_from_database_path(database_path)?;
    let connection = open_revision_reader(database_path)?;
    #[cfg(test)]
    let acquired_at = Instant::now();
    let target = (|| -> StoreResult<ReadTarget> {
        let actual = current_revision(&connection)?;
        if actual != revision {
            return Err(StoreError::RevisionConflict {
                expected: revision,
                actual,
            });
        }
        let generation = active_generation(&connection)?;
        let root_exists = connection
            .query_row(
                "SELECT 1 FROM root WHERE generation = ?1",
                [&generation],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !root_exists {
            return Err(StoreError::RevisionConflict {
                expected: revision,
                actual,
            });
        }
        Ok(ReadTarget {
            revision,
            generation,
        })
    })();
    let target = match target {
        Ok(target) => target,
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(error);
        }
    };
    let lease = format!("snapshot-{revision}-{}", Uuid::new_v4());
    active_readers.register();
    Ok((
        lease.clone(),
        RevisionReadLease {
            connection,
            target,
            lease,
            repository_root,
            active_readers,
            transaction_open: true,
            #[cfg(test)]
            acquired_at,
        },
    ))
}

pub(super) fn open_revision_reader(database_path: &Path) -> StoreResult<Connection> {
    open_reader(database_path, &|_| Ok(()))
}

/// A reader that presents one generation under the raw table names. The views are temp objects,
/// so they have to exist before the connection becomes read-only.
pub(super) fn open_generation_reader(
    database_path: &Path,
    generation: &str,
) -> StoreResult<Connection> {
    open_reader(database_path, &|connection| {
        super::portable::install_generation_views(connection, generation)
    })
}

fn open_reader(
    database_path: &Path,
    prepare: &dyn Fn(&Connection) -> StoreResult<()>,
) -> StoreResult<Connection> {
    let preferred = revision_reader_open_flags_for_target(cfg!(target_os = "android"));
    if cfg!(target_os = "android") {
        return configure_revision_reader(database_path, preferred, prepare);
    }
    match configure_revision_reader(database_path, preferred, prepare) {
        Ok(connection) => Ok(connection),
        Err(_) => configure_revision_reader(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            prepare,
        ),
    }
}

pub(super) fn revision_reader_open_flags_for_target(is_android: bool) -> OpenFlags {
    let access = if is_android {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    access | OpenFlags::SQLITE_OPEN_NO_MUTEX
}

fn configure_revision_reader(
    database_path: &Path,
    flags: OpenFlags,
    prepare: &dyn Fn(&Connection) -> StoreResult<()>,
) -> StoreResult<Connection> {
    let connection = Connection::open_with_flags(database_path, flags)?;
    connection.busy_timeout(Duration::ZERO)?;
    connection.execute_batch("PRAGMA cache_size = -2048;")?;
    prepare(&connection)?;
    connection.execute_batch(
        "
        PRAGMA query_only = ON;
        BEGIN DEFERRED;
        ",
    )?;
    Ok(connection)
}

pub(super) fn close_revision(mut lease: RevisionReadLease) -> StoreResult<()> {
    let result = lease
        .connection
        .execute_batch("ROLLBACK")
        .map_err(StoreError::from);
    if result.is_ok() {
        lease.transaction_open = false;
    }
    result
}

pub(super) fn checkpoint(connection: &Connection, mode: CheckpointMode) -> StoreResult<()> {
    let (mode, reject_busy) = match mode {
        CheckpointMode::Passive => ("PASSIVE", false),
        CheckpointMode::Truncate => ("TRUNCATE", true),
    };
    let (busy, _, _): (i64, i64, i64) =
        connection.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if reject_busy && busy != 0 {
        return Err(StoreError::Store {
            message: "truncate checkpoint could not complete because the database is busy"
                .to_owned(),
        });
    }
    Ok(())
}

pub(super) fn create(
    store: &mut super::PersistentStore,
    reason: &str,
) -> StoreResult<SnapshotCreated> {
    let mut archive = Archive::open(&store.snapshots_dir)?;
    create_in_archive(store, &mut archive, reason)
}

fn create_in_archive(
    store: &mut super::PersistentStore,
    archive: &mut Archive,
    reason: &str,
) -> StoreResult<SnapshotCreated> {
    let started = Instant::now();
    let (scratch, current_bytes) = capture_scratch(store, archive)?;
    archive_scratch(archive, scratch, current_bytes, reason, started)
}

pub(super) fn capture_scratch(
    store: &mut super::PersistentStore,
    archive: &Archive,
) -> StoreResult<(super::snapshot_archive::Scratch, u64)> {
    let scratch = archive.scratch()?;
    let revision = store.revision()?;
    let lease = store.lww_acquire_library_backup_capture(revision)?;
    let result = (|| {
        let units = store.lww_backup_unit_values(&lease.lease)?;
        let (connection, target) = store.read_view(Some(&lease.lease))?;
        let current_bytes = logical_database_bytes(connection)?;
        let mut output = Connection::open(&scratch.path)?;
        {
            let backup = rusqlite::backup::Backup::new(connection, &mut output)?;
            loop {
                match backup.step(128)? {
                    rusqlite::backup::StepResult::Done => break,
                    rusqlite::backup::StepResult::More => (),
                    _ => return Err(validation("snapshot source is busy")),
                }
            }
        }
        let tx = output.transaction()?;
        tx.execute("DELETE FROM snapshot_original_meta", [])?;
        tx.execute("DELETE FROM snapshot_original_units", [])?;
        tx.execute("INSERT INTO snapshot_original_meta VALUES(1,?1,?2)", rusqlite::params![target.revision,target.generation])?;
        for (key,value) in units {
            tx.execute("INSERT INTO snapshot_original_units VALUES(?1,?2)", rusqlite::params![key.as_str(),serde_json::to_string(&value)?])?;
        }
        tx.commit()?;
        Ok(current_bytes)
    })();
    let release = store.release_revision(&lease.lease);
    let current_bytes = result?;
    release?;
    Ok((scratch, current_bytes))
}

pub(super) fn archive_scratch(
    archive: &mut Archive,
    scratch: super::snapshot_archive::Scratch,
    current_bytes: u64,
    reason: &str,
    started: Instant,
) -> StoreResult<SnapshotCreated> {
    let captured = Connection::open(&scratch.path)?;
    let revision = current_revision(&captured)?;
    let roots = collect_asset_roots(&captured)?;
    drop(captured);
    let metadata = archive.insert(&scratch.path, revision, reason, roots)?;
    archive.rotate(byte_budget(current_bytes), &metadata.id)?;
    Ok(SnapshotCreated {
        id: metadata.id,
        revision,
        bytes: metadata.bytes,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

pub(super) fn byte_budget(current_bytes: u64) -> u64 {
    current_bytes.saturating_mul(4).max(MIN_SNAPSHOT_BYTES)
}

pub(super) fn list(snapshots_dir: &Path) -> StoreResult<Vec<SnapshotInfo>> {
    Archive::open(snapshots_dir)?.list()
}

pub(super) fn snapshot_path_is_link_or_reparse(path: &Path) -> StoreResult<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Ok(metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0)
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

fn delete_generation(transaction: &rusqlite::Transaction<'_>, generation: &str) -> StoreResult<()> {
    for (table, _) in GENERATION_TABLES.iter().rev() {
        transaction.execute(
            &format!("DELETE FROM {table} WHERE generation = ?1"),
            [generation],
        )?;
    }
    Ok(())
}

fn logical_database_bytes(connection: &Connection) -> StoreResult<u64> {
    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    Ok((page_count as u64).saturating_mul(page_size as u64))
}

pub(super) fn collect_asset_roots(
    connection: &Connection,
) -> StoreResult<AssetRootSet> {
    #[cfg(test)]
    ASSET_ROOT_SCANS.with(|count| count.set(count.get() + 1));
    collect_asset_roots_scoped(connection, None, None)
}

pub(super) fn collect_plugin_asset_roots(connection: &Connection) -> StoreResult<AssetRootSet> {
    #[cfg(test)] PLUGIN_ROOT_SCANS.with(|count| count.set(count.get()+1));
    let mut roots = AssetRootSet::default();
    scan_json_column(connection, "SELECT value FROM plugin_storage", [], &mut roots)?;
    Ok(roots)
}

#[cfg(test)]
thread_local! { pub(super) static PLUGIN_ROOT_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(super) fn collect_device_plugin_asset_roots(store: &super::PersistentStore) -> StoreResult<(super::device_store::plugin_gc::PluginGcFence, AssetRootSet)> {
    use base64::Engine;
    use risunest_sync_wire::unit::UnitValue;
    let roots = std::cell::RefCell::new(AssetRootSet::default());
    let mut units = std::collections::BTreeMap::new();
    let fence = store.device_store()?.visit_plugin_gc_values(|_, _, _, raw| {
        let value = serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_owned()));
        observe_json_value(&value, None, &mut roots.borrow_mut());
        Ok(())
    }, |key, value| {
        match value {
            UnitValue::Inline { bytes, .. } => {
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(bytes)
                    .map_err(|error| validation(error.to_string()))?;
                let value = serde_json::from_slice(&bytes)?;
                observe_json_value(&value, None, &mut roots.borrow_mut());
            }
            UnitValue::Object { .. } => { units.insert(key.clone(), value.clone()); }
            UnitValue::Deleted => {}
        }
        Ok(())
    })?;
    let mut roots = roots.into_inner();
    if !units.is_empty() {
        let inventory = super::external_capture::original_unit_dependency_inventory(&units, &|hash| {
            let length: Option<i64> = store.connection.query_row("SELECT length(body) FROM message_page_objects WHERE hash=?1", [hash], |row| row.get(0)).optional()?;
            if length.is_some_and(|length| length < 0 || length as u64 > risunest_sync_wire::MAX_METADATA_BYTES as u64) { return Err(validation("plugin control exceeds its byte limit")); }
            Ok(store.connection.query_row("SELECT body FROM message_page_objects WHERE hash=?1", [hash], |row| row.get(0)).optional()?)
        }, &|hash| {
            let size: Option<i64> = store.connection.query_row("SELECT byte_size FROM asset_objects WHERE object_hash=?1", [hash], |row| row.get(0)).optional()?;
            size.map(|size| u64::try_from(size).map_err(|_| validation("plugin payload size is invalid"))).transpose()
        }, &crate::local_backup::NeverCancelled, false, &mut |_, _, _| Ok(()));
        match inventory {
            Ok(inventory) => roots.object_hashes.extend(inventory.payloads.into_keys()),
            Err(_) => { roots.retain_all_objects = true; roots.blockers.insert("plugin-local-unscannable".into()); }
        }
    }
    Ok((fence, roots))
}

pub(super) fn merge_asset_roots(target: &mut AssetRootSet, source: AssetRootSet) {
    target.manifest_hashes.extend(source.manifest_hashes);
    target.object_hashes.extend(source.object_hashes);
    target.legacy_asset_keys.extend(source.legacy_asset_keys);
    target.inlay_ids.extend(source.inlay_ids);
    target.cold_keys.extend(source.cold_keys);
    target.blockers.extend(source.blockers);
    target.retain_all_objects |= source.retain_all_objects;
}

pub(super) fn collect_asset_roots_with_plugin_cache(connection: &Connection, plugins: &AssetRootSet) -> StoreResult<AssetRootSet> {
    collect_asset_roots_scoped(connection, None, Some(plugins))
}

pub(super) fn collect_preserved_source_roots(repository:&Path)->AssetRootSet {
    let mut roots=AssetRootSet::default();
    let result=(||->StoreResult<()> {
        let directory=repository.join("source-preservation");
        let metadata=match fs::symlink_metadata(&directory) {Ok(value)=>value,Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(error)=>return Err(error.into())};
        if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) {return Err(validation("invalid source preservation directory"));}
        for entry in fs::read_dir(directory)? {
            let path=entry?.path();
            let metadata=fs::symlink_metadata(&path)?;
            if !metadata.is_dir() || crate::trust_boundary::is_link_like(&metadata) {return Err(validation("invalid source preservation entry"));}
            let path=path.join("index.sqlite");
            let metadata=fs::symlink_metadata(&path)?;
            if !metadata.is_file() || crate::trust_boundary::is_link_like(&metadata) {return Err(validation("invalid source preservation index"));}
            let index=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
            index.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")?;
            let mut statement=index.prepare("SELECT object_hash,storage_kind FROM source_files")?;
            let mut rows=statement.query([])?;
            while let Some(row)=rows.next()? {
                let hash:String=row.get(0)?;
                if hash.len()!=64 || !hash.bytes().all(|value|value.is_ascii_hexdigit()&&!value.is_ascii_uppercase()) {return Err(validation("invalid preserved object hash"));}
                match row.get::<_,String>(1)?.as_str() {"cas"=>{roots.object_hashes.insert(hash);},"owned"=>{},_=>return Err(validation("invalid preserved storage kind"))}
            }
        }
        Ok(())
    })();
    if result.is_err() {roots.retain_all_objects=true;roots.blockers.insert("source-preservation-unscannable".into());}
    roots
}

#[cfg(feature = "native-official-publication")]
fn collect_asset_roots_for_generation(
    connection: &Connection,
    generation: &str,
) -> StoreResult<AssetRootSet> {
    collect_asset_roots_scoped(connection, Some(generation), None)
}

// One scanner serves both the global GC-root collection and the per-generation
// publication pinning so the two table lists can never drift apart. The global
// scope additionally covers the logical-sync manifests, which are meaningless
// for a single generation.
fn collect_asset_roots_scoped(
    connection: &Connection,
    generation: Option<&str>,
    plugins: Option<&AssetRootSet>,
) -> StoreResult<AssetRootSet> {
    let mut roots = AssetRootSet::default();
    if generation.is_none() {
        let query = ["root", "bot_presets", "characters", "conversations", "messages",
            "plugin_storage", "asset_aliases", "asset_owner_heads"]
            .map(|table| format!("SELECT generation FROM {table}"))
            .join(" UNION ");
        let mut statement = connection.prepare(&query)?;
        let generations = statement.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for generation in generations {
            let scoped = collect_asset_roots_scoped(connection, Some(&generation), plugins)?;
            roots.manifest_hashes.extend(scoped.manifest_hashes);
            roots.object_hashes.extend(scoped.object_hashes);
            roots.legacy_asset_keys.extend(scoped.legacy_asset_keys);
            roots.inlay_ids.extend(scoped.inlay_ids);
            roots.cold_keys.extend(scoped.cold_keys);
            roots.blockers.extend(scoped.blockers);
            roots.retain_all_objects |= scoped.retain_all_objects;
        }
        if table_exists(connection, "server_sync_objects")? {
            scan_optional_hash_column(connection, "SELECT hash FROM server_sync_objects", [],
                HashTarget::Object, &mut roots)?;
        }
        merge_asset_roots(&mut roots, collect_lww_asset_roots(connection)?);
        return Ok(roots);
    }
    let scope_params: Vec<&dyn rusqlite::ToSql> = generation
        .as_ref()
        .map(|generation| vec![generation as &dyn rusqlite::ToSql])
        .unwrap_or_default();
    let scope_params = scope_params.as_slice();
    let scoped = generation.is_some();

    scan_optional_hash_column(
        connection,
        if scoped {
            "SELECT manifest_hash FROM asset_owner_heads
         WHERE generation = ?1 AND present = 1"
        } else {
            "SELECT manifest_hash FROM asset_owner_heads WHERE present = 1"
        },
        scope_params,
        HashTarget::Manifest,
        &mut roots,
    )?;
    scan_asset_alias_roots(
        connection,
        if scoped {
            "SELECT logical_key, object_hash FROM asset_aliases WHERE generation = ?1"
        } else {
            "SELECT logical_key, object_hash FROM asset_aliases"
        },
        scope_params,
        &mut roots,
    )?;
    // An archived character keeps no scannable detail, so its payload object and
    // the asset hashes it recorded are the only roots that hold those bytes.
    scan_archived_object_roots(
        connection,
        if scoped {
            "SELECT archived_object FROM characters
         WHERE generation = ?1 AND archived_object IS NOT NULL"
        } else {
            "SELECT archived_object FROM characters WHERE archived_object IS NOT NULL"
        },
        scope_params,
        &mut roots,
    )?;
    if !scoped {
        if table_exists(connection, "server_sync_objects")? {
            scan_optional_hash_column(
                connection,
                "SELECT hash FROM server_sync_objects",
                [],
                HashTarget::Object,
                &mut roots,
            )?;
        }
    }

    for (table, column) in [
        ("root", "value"),
        ("bot_presets", "value"),
        ("characters", "detail"),
        ("conversations", "detail"),
        ("messages", "value"),
        ("plugin_storage", "value"),
    ] {
        if table == "plugin_storage" && plugins.is_some() { continue; }
        let query = if scoped {
            format!("SELECT {column} FROM {table} WHERE generation = ?1")
        } else {
            format!("SELECT {column} FROM {table}")
        };
        scan_json_column(connection, &query, scope_params, &mut roots)?;
    }
    if let Some(plugins) = plugins {
        roots.manifest_hashes.extend(plugins.manifest_hashes.iter().cloned());
        roots.object_hashes.extend(plugins.object_hashes.iter().cloned());
        roots.legacy_asset_keys.extend(plugins.legacy_asset_keys.iter().cloned());
        roots.inlay_ids.extend(plugins.inlay_ids.iter().cloned());
        roots.cold_keys.extend(plugins.cold_keys.iter().cloned());
        roots.blockers.extend(plugins.blockers.iter().cloned());
        roots.retain_all_objects |= plugins.retain_all_objects;
    }
    for table in ["bot_presets", "characters"] {
        let query = if scoped {
            format!("SELECT image FROM {table} WHERE generation = ?1 AND image IS NOT NULL")
        } else {
            format!("SELECT image FROM {table} WHERE image IS NOT NULL")
        };
        scan_text_column(connection, &query, scope_params, &mut roots)?;
    }
    let mut aliases = connection.prepare(
        "SELECT logical_key, kind FROM asset_aliases
         WHERE generation = ?1 AND object_hash IS NOT NULL",
    )?;
    let mut rows = aliases.query(scope_params)?;
    while let Some(row) = rows.next()? {
        match (scanned_text(row, 0)?, scanned_text(row, 1)?) {
            (ScannedText::Text(key), ScannedText::Text(kind)) if kind == "asset" => {
                roots.legacy_asset_keys.remove(&key);
            }
            (ScannedText::Text(key), ScannedText::Text(kind)) if kind == "inlay" => {
                roots.inlay_ids.remove(&key);
            }
            _ => retain_unscannable_record(&mut roots),
        }
    }
    // Nothing stores a cold payload any more, so a record that still references
    // one hides an unknowable set of attachments. Keep every object instead.
    if !roots.cold_keys.is_empty() {
        roots.blockers.insert("cold-payload-unscanned".to_owned());
        roots.retain_all_objects = true;
    }
    Ok(roots)
}

fn collect_lww_asset_roots(connection: &Connection) -> StoreResult<AssetRootSet> {
    use base64::Engine;
    use risunest_sync_wire::unit::{UnitKey, UnitValue};
    let mut roots = AssetRootSet::default();
    for table in ["lww_units", "lww_receive_rows", "snapshot_original_units"] {
        let predicate = if table == "lww_receive_rows" { " WHERE status IN ('held','deferred')" } else { "" };
        let mut statement = connection.prepare(&format!("SELECT key,value FROM {table}{predicate}"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let result = (|| -> StoreResult<()> {
                let key:UnitKey=row.get::<_,String>(0)?.try_into().map_err(|error:risunest_sync_wire::WireError|validation(error.to_string()))?;
                let value:UnitValue=serde_json::from_str(&row.get::<_,String>(1)?)?;
                value.validate().map_err(|error|validation(error.to_string()))?;
                match &value {
                    UnitValue::Inline {bytes} => {
                        let decoded=base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(bytes).map_err(|error|validation(error.to_string()))?;
                        observe_json_value(&serde_json::from_slice(&decoded)?,None,&mut roots);
                    }
                    UnitValue::Object {..} => {
                        let inventory=super::external_capture::original_unit_dependency_inventory(&[(key,value)].into_iter().collect(),&|hash| {
                            let length:Option<i64>=connection.query_row("SELECT length(body) FROM message_page_objects WHERE hash=?1",[hash],|row|row.get(0)).optional()?;
                            if length.is_some_and(|length|length<0 || length as u64>risunest_sync_wire::MAX_METADATA_BYTES as u64) {return Err(validation("GC control exceeds its byte limit"));}
                            Ok(connection.query_row("SELECT body FROM message_page_objects WHERE hash=?1",[hash],|row|row.get(0)).optional()?)
                        },&|hash| {
                            let size:Option<i64>=connection.query_row("SELECT byte_size FROM asset_objects WHERE object_hash=?1",[hash],|row|row.get(0)).optional()?;
                            size.map(|size|u64::try_from(size).map_err(|_|validation("GC payload size is invalid"))).transpose()
                        },&crate::local_backup::NeverCancelled,false,&mut |_,_,_|Ok(()))?;
                        roots.object_hashes.extend(inventory.payloads.into_keys());
                    }
                    UnitValue::Deleted => {}
                }
                Ok(())
            })();
            if result.is_err() {roots.retain_all_objects=true;roots.blockers.insert("lww-source-unscannable".into());}
        }
    }
    let mut statement=connection.prepare("SELECT logical_key,kind FROM asset_aliases WHERE object_hash IS NOT NULL")?;
    let mut rows=statement.query([])?;
    while let Some(row)=rows.next()? {
        let key:String=row.get(0)?;
        match row.get::<_,String>(1)?.as_str() {"asset"=>{roots.legacy_asset_keys.remove(&key);},"inlay"=>{roots.inlay_ids.remove(&key);},_=>{}}
    }
    let mut statement=connection.prepare("SELECT hash FROM snapshot_restore_payloads p WHERE NOT EXISTS(SELECT 1 FROM snapshot_restore_body_jobs j WHERE j.stage_id=p.stage_id AND j.complete=1)")?;
    let hashes=statement.query_map([],|row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    roots.object_hashes.extend(hashes);
    Ok(roots)
}

/// Every CAS object one character's records reach, so archiving can record them
/// and keep them out of the sweep while the character has no scannable detail.
pub(super) fn collect_character_asset_hashes(connection:&Connection,cas:&PayloadCas,generation:&str,character_id:&str)->StoreResult<Vec<String>> {collect_character_asset_hashes_inner(connection,cas,generation,character_id,false)}
pub(super) fn collect_shared_character_asset_hashes(connection:&Connection,cas:&PayloadCas,generation:&str,character_id:&str)->StoreResult<Vec<String>> {collect_character_asset_hashes_inner(connection,cas,generation,character_id,true)}
fn collect_character_asset_hashes_inner(
    connection: &Connection,
    cas: &PayloadCas,
    generation: &str,
    character_id: &str,
    shared: bool,
) -> StoreResult<Vec<String>> {
    let mut roots = AssetRootSet::default();
    let scope: [&dyn rusqlite::ToSql; 2] = [&generation, &character_id];
    for query in [
        "SELECT detail FROM characters WHERE generation = ?1 AND character_id = ?2",
        "SELECT detail FROM conversations WHERE generation = ?1 AND character_id = ?2",
        "SELECT value FROM messages WHERE generation = ?1 AND character_id = ?2",
    ] {
        if shared {let mut statement=connection.prepare(query)?;let mut rows=statement.query(scope.as_slice())?;while let Some(row)=rows.next()?{let raw:String=row.get(0)?;let value=serde_json::from_str(&raw)?;let value=if query.contains("FROM characters"){super::lww::shared_archive_character(value)}else if query.contains("FROM conversations"){super::lww::shared_archive_conversation(value)}else{value};observe_json_value(&value,None,&mut roots);}}else{scan_json_column(connection, query, scope.as_slice(), &mut roots)?;}

    }
    scan_text_column(
        connection,
        "SELECT image FROM characters
         WHERE generation = ?1 AND character_id = ?2 AND image IS NOT NULL",
        scope.as_slice(),
        &mut roots,
    )?;

    let mut hashes: BTreeSet<String> = std::mem::take(&mut roots.object_hashes);
    let manifest_hash: Option<String> = connection
        .query_row(
            "SELECT manifest_hash FROM asset_owner_heads
             WHERE generation = ?1 AND owner_kind = 'character-additional-assets'
               AND owner_locator = ?2 AND present = 1",
            rusqlite::params![generation, character_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(manifest_hash) = manifest_hash {
        if let Some(canonical) = cas.read_object(&manifest_hash)? {
            if let Ok(entries) =
                crate::asset_repository::owner_manifest_codec::decode_owner_manifest(&canonical)
            {
                for entry in entries {
                    if let Some(payload) = entry.payload_hash {
                        hashes.insert(hex::encode(payload));
                    }
                }
            }
        }
        hashes.insert(manifest_hash);
    }

    let mut candidates: BTreeSet<String> = roots
        .legacy_asset_keys
        .iter()
        .chain(&roots.inlay_ids)
        .cloned()
        .collect();
    collect_alias_key_candidates(connection, generation, character_id, &mut candidates,shared)?;
    for candidate in candidates {
        let mut statement = connection.prepare_cached(
            "SELECT object_hash FROM asset_aliases
             WHERE generation = ?1 AND logical_key = ?2 AND object_hash IS NOT NULL",
        )?;
        let found = statement
            .query_map(rusqlite::params![generation, candidate], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        hashes.extend(found);
    }
    Ok(hashes.into_iter().collect())
}

/// String leaves of the character detail are the only place an alias logical key
/// can appear without a recognizable prefix, so they are looked up directly.
pub(super) fn collect_alias_key_candidates(
    connection: &Connection,
    generation: &str,
    character_id: &str,
    candidates: &mut BTreeSet<String>,
    shared: bool,
) -> StoreResult<()> {
    const MAX_CANDIDATE_BYTES: usize = 512;
    let detail: Option<String> = connection
        .query_row(
            "SELECT detail FROM characters WHERE generation = ?1 AND character_id = ?2",
            rusqlite::params![generation, character_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(detail) = detail else {
        return Ok(());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&detail) else {
        return Ok(());
    };
    let value=if shared{super::lww::shared_archive_character(value)}else{value};
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::String(value) => {
                if !value.is_empty()
                    && value.len() <= MAX_CANDIDATE_BYTES
                    && !value.contains(char::is_whitespace)
                {
                    candidates.insert(value);
                }
            }
            serde_json::Value::Array(values) => pending.extend(values),
            serde_json::Value::Object(values) => {
                pending.extend(values.into_iter().map(|(_, value)| value))
            }
            _ => {}
        }
    }
    Ok(())
}

fn repository_root_from_database_path(database_path: &Path) -> StoreResult<PathBuf> {
    database_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| validation("persistent database has no repository root"))
}

fn table_exists(connection: &Connection, table: &str) -> StoreResult<bool> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(true),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(StoreError::from)
}

// A snapshot exists to preserve the rows it captures, so a value whose storage
// class or encoding is damaged widens retention instead of failing the capture.
enum ScannedText {
    Text(String),
    Null,
    Damaged,
}

fn scanned_text(row: &rusqlite::Row<'_>, index: usize) -> StoreResult<ScannedText> {
    Ok(match row.get_ref(index)? {
        rusqlite::types::ValueRef::Null => ScannedText::Null,
        rusqlite::types::ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(value) => ScannedText::Text(value.to_owned()),
            Err(_) => ScannedText::Damaged,
        },
        _ => ScannedText::Damaged,
    })
}

fn retain_unscannable_record(roots: &mut AssetRootSet) {
    roots.blockers.insert(UNSCANNABLE_BLOCKER.to_owned());
    roots.retain_all_objects = true;
}

enum HashTarget {
    Manifest,
    Object,
}

fn scan_optional_hash_column<P: rusqlite::Params>(
    connection: &Connection,
    query: &str,
    params: P,
    target: HashTarget,
    roots: &mut AssetRootSet,
) -> StoreResult<()> {
    let mut statement = connection.prepare(query)?;
    let mut rows = statement.query(params)?;
    while let Some(row) = rows.next()? {
        match scanned_text(row, 0)? {
            ScannedText::Text(value) => {
                match target {
                    HashTarget::Manifest => roots.manifest_hashes.insert(value),
                    HashTarget::Object => roots.object_hashes.insert(value),
                };
            }
            ScannedText::Null => {}
            ScannedText::Damaged => retain_unscannable_record(roots),
        }
    }
    Ok(())
}

fn scan_asset_alias_roots<P: rusqlite::Params>(
    connection: &Connection,
    query: &str,
    params: P,
    roots: &mut AssetRootSet,
) -> StoreResult<()> {
    let mut statement = connection.prepare(query)?;
    let mut rows = statement.query(params)?;
    while let Some(row) = rows.next()? {
        match scanned_text(row, 1)? {
            ScannedText::Text(object_hash) => {
                roots.object_hashes.insert(object_hash);
            }
            ScannedText::Null => match scanned_text(row, 0)? {
                ScannedText::Text(logical_key) => {
                    roots.legacy_asset_keys.insert(logical_key);
                }
                _ => retain_unscannable_record(roots),
            },
            ScannedText::Damaged => retain_unscannable_record(roots),
        }
    }
    Ok(())
}

fn scan_archived_object_roots<P: rusqlite::Params>(
    connection: &Connection,
    query: &str,
    params: P,
    roots: &mut AssetRootSet,
) -> StoreResult<()> {
    let mut statement = connection.prepare(query)?;
    let mut rows = statement.query(params)?;
    while let Some(row) = rows.next()? {
        let ScannedText::Text(encoded) = scanned_text(row, 0)? else {
            retain_unscannable_record(roots);
            continue;
        };
        match serde_json::from_str::<super::archive::ArchivedObject>(&encoded) {
            Ok(archived) => {
                roots.object_hashes.insert(archived.object_hash);
                roots.object_hashes.extend(archived.asset_hashes);
            }
            Err(_) => retain_unscannable_record(roots),
        }
    }
    Ok(())
}

fn scan_json_column<P: rusqlite::Params>(
    connection: &Connection,
    query: &str,
    params: P,
    roots: &mut AssetRootSet,
) -> StoreResult<()> {
    let mut statement = connection.prepare(query)?;
    let mut rows = statement.query(params)?;
    while let Some(row) = rows.next()? {
        // The snapshot contains this exact record. If its references cannot
        // be decoded, retain objects instead of discarding the raw backup.
        match scanned_text(row, 0)? {
            ScannedText::Text(encoded) => match serde_json::from_str(&encoded) {
                Ok(value) => observe_json_value(&value, None, roots),
                Err(_) => retain_unscannable_record(roots),
            },
            _ => retain_unscannable_record(roots),
        }
    }
    Ok(())
}

fn scan_text_column<P: rusqlite::Params>(
    connection: &Connection,
    query: &str,
    params: P,
    roots: &mut AssetRootSet,
) -> StoreResult<()> {
    let mut statement = connection.prepare(query)?;
    let mut rows = statement.query(params)?;
    while let Some(row) = rows.next()? {
        match scanned_text(row, 0)? {
            ScannedText::Text(value) => observe_text(&value, roots),
            _ => retain_unscannable_record(roots),
        }
    }
    Ok(())
}

pub(super) fn observe_json_value(
    value: &serde_json::Value,
    parent_key: Option<&str>,
    roots: &mut AssetRootSet,
) {
    match value {
        serde_json::Value::String(value) => {
            observe_text(value, roots);
            if crate::trust_boundary::is_lower_hex_256(value) {
                roots.object_hashes.insert(value.clone());
            }
            if parent_key == Some("coldstorage") && !value.is_empty() {
                roots.cold_keys.insert(value.clone());
            } else if parent_key == Some("coldStoragedChats") {
                roots.cold_keys.insert(value.clone());
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                observe_json_value(value, parent_key, roots);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                observe_json_value(value, Some(key), roots);
            }
        }
        _ => {}
    }
}

fn observe_text(value: &str, roots: &mut AssetRootSet) {
    if value.starts_with("assets/") {
        roots.legacy_asset_keys.insert(value.to_owned());
    }
    if let Some(cold_key) = value.strip_prefix(COLD_STORAGE_HEADER) {
        if !cold_key.is_empty() {
            roots.cold_keys.insert(cold_key.to_owned());
        }
    }
    observe_native_cas_paths(value, roots);
    for prefix in ["{{inlay::", "{{inlayed::", "{{inlayeddata::"] {
        let mut remainder = value;
        while let Some(start) = remainder.find(prefix) {
            remainder = &remainder[start + prefix.len()..];
            let Some(end) = remainder.find("}}") else {
                break;
            };
            roots.inlay_ids.insert(remainder[..end].to_owned());
            remainder = &remainder[end + 2..];
        }
    }
}

fn observe_native_cas_paths(value: &str, roots: &mut AssetRootSet) {
    let bytes = value.as_bytes();
    let physical_len = CAS_PHYSICAL_PREFIX.len() + 65;
    if bytes.len() >= physical_len {
        for start in 0..=bytes.len() - physical_len {
            let candidate = &bytes[start..start + physical_len];
            if let Some(hash) = cas_hash_from_physical_key(candidate) {
                if bytes
                    .get(start + physical_len)
                    .is_none_or(|next| !next.is_ascii_hexdigit() && *next != b'/')
                {
                    roots.object_hashes.insert(hash);
                }
            }
        }
    }

    for (start, _) in value.char_indices() {
        let remainder = &value[start..];
        if !starts_with_url_scheme(remainder) {
            continue;
        }
        let end = remainder.find(is_url_delimiter).unwrap_or(remainder.len());
        let candidate = &remainder[..end];
        let native_marker = candidate.to_ascii_lowercase().contains("risuasset");
        let Some(physical_key) = crate::native_media::decode_physical_key(candidate) else {
            if native_marker {
                retain_unknown_native_url(roots);
            }
            continue;
        };
        if let Some(hash) = cas_hash_from_physical_key(physical_key.as_bytes()) {
            roots.object_hashes.insert(hash);
        } else {
            retain_unknown_native_url(roots);
        }
    }
}

fn starts_with_url_scheme(value: &str) -> bool {
    ["risuasset:", "http:", "https:"].iter().any(|prefix| {
        value
            .get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
    })
}

fn is_url_delimiter(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}'
        )
}

fn retain_unknown_native_url(roots: &mut AssetRootSet) {
    roots.retain_all_objects = true;
    roots
        .blockers
        .insert("native-asset-url-unresolved".to_owned());
}

fn cas_hash_from_physical_key(value: &[u8]) -> Option<String> {
    if value.len() != CAS_PHYSICAL_PREFIX.len() + 65 || !value.starts_with(CAS_PHYSICAL_PREFIX) {
        return None;
    }
    let suffix = &value[CAS_PHYSICAL_PREFIX.len()..];
    if suffix[2] != b'/'
        || !suffix[..2]
            .iter()
            .chain(&suffix[3..])
            .all(|byte| crate::trust_boundary::is_lower_hex_byte(*byte))
    {
        return None;
    }
    let mut hash = Vec::with_capacity(64);
    hash.extend_from_slice(&suffix[..2]);
    hash.extend_from_slice(&suffix[3..]);
    String::from_utf8(hash).ok()
}

fn validate_restore_database(path: &Path) -> StoreResult<()> {
    let connection = Connection::open(path)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(validation("snapshot integrity check failed"));
    }
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != i64::from(super::schema::SCHEMA_VERSION) {
        return Err(validation("snapshot schema version is not supported"));
    }
    Ok(())
}

fn remove_database_files(database_path: &Path) -> StoreResult<()> {
    for path in [
        database_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", database_path.display())),
        PathBuf::from(format!("{}-shm", database_path.display())),
    ] {
        remove_file_if_exists(&path)?;
    }
    Ok(())
}

fn path_error(context: &str, path: &Path, error: std::io::Error) -> StoreError {
    StoreError::Store {
        message: format!("{context} at {}: {error}", path.display()),
    }
}

fn remove_file_if_exists(path: &Path) -> StoreResult<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn validation(message: impl Into<String>) -> StoreError {
    StoreError::Validation {
        message: message.into(),
    }
}

impl super::PersistentStore {
    pub(crate) fn snapshot_restore_body_plan(&mut self, stage_id: &str, revision: i64, authority: &str) -> StoreResult<crate::native_file_jobs::snapshot_bodies::BodyPlan> {
        let (request, stored_authority, state, committed):(String,String,String,Option<i64>) = self.connection.query_row("SELECT request_id,authority,state,revision FROM snapshot_restore_stages WHERE stage_id=?1",[stage_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        let request_revision:Option<i64> = self.connection.query_row("SELECT revision FROM lww_requests WHERE request_id=?1",[request],|row|row.get(0)).optional()?;
        if state != "committed" || committed != Some(revision) || request_revision != Some(revision)
            || stored_authority != authority || self.lww_binding_authority()?.0.to_string() != authority {
            return Err(validation("snapshot body job activation receipt differs"));
        }
        let protection:Option<String>=self.connection.query_row("SELECT protection_job_id FROM snapshot_restore_body_jobs WHERE stage_id=?1",[stage_id],|row|row.get(0)).optional()?;
        let protection_id=match protection {
            Some(id)=>id,
            None=>{
                let id=Uuid::new_v4().to_string();
                self.connection.execute("INSERT INTO snapshot_restore_body_jobs VALUES(?1,?2,0)",rusqlite::params![stage_id,id])?;
                id
            }
        };
        let mut statement=self.connection.prepare("SELECT hash,byte_size,owner,cached FROM snapshot_restore_payloads WHERE stage_id=?1 ORDER BY hash")?;
        let mut rows=statement.query([stage_id])?;
        let mut objects=Vec::new();
        while let Some(row)=rows.next()? {
            objects.push(crate::native_file_jobs::snapshot_bodies::BodyObject {
                hash:row.get(0)?, size:u64::try_from(row.get::<_,i64>(1)?).map_err(|_|validation("snapshot body size is invalid"))?, owner:row.get(2)?, cached:row.get(3)?,
            });
        }
        Ok(crate::native_file_jobs::snapshot_bodies::BodyPlan {
            stage_id:stage_id.to_owned(),revision,authority:authority.to_owned(),protection_id,objects,
            source:self.snapshot_restore_source_path(stage_id)?,policy:self.server_asset_policy().map_err(|error|validation(&error.code))?,
        })
    }

    pub(crate) fn snapshot_restore_bodies_completed(&self,stage_id:&str)->StoreResult<()> {
        self.connection.execute("UPDATE snapshot_restore_body_jobs SET complete=1 WHERE stage_id=?1",[stage_id])?;
        remove_database_files(&self.snapshot_restore_source_path(stage_id)?)?;
        Ok(())
    }

    pub(crate) fn snapshot_restore_stage(&mut self, id: &str, request_id: &str) -> StoreResult<super::StagingResult> {
        if request_id.is_empty() || request_id.len()>256 { return Err(validation("invalid snapshot restore request identity")); }
        let existing:Option<(String,String,String)> = self.connection.query_row("SELECT stage_id,snapshot_id,authority FROM snapshot_restore_stages WHERE request_id=?1",[request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((staging_id,snapshot_id,authority)) = existing {
            if snapshot_id!=id || authority!=self.lww_binding_authority()?.0.to_string() { return Err(validation("snapshot restore request identity mismatch")); }
            return Ok(super::StagingResult {staging_id});
        }
        let archive = Archive::open(&self.snapshots_dir)?;
        let scratch = archive.scratch()?;
        archive.restore(id,&scratch.path)?;
        validate_restore_database(&scratch.path)?;
        let candidate = prepare_restore_candidate(&self.snapshots_dir,&scratch.path)?;
        let outcome = (|| {
            let source = Connection::open(&candidate)?;
            let generation = active_generation(&source)?;
            let (source_revision, source_generation): (i64,String) = source.query_row("SELECT revision,generation FROM snapshot_original_meta WHERE singleton=1", [], |row|Ok((row.get(0)?,row.get(1)?)))?;
            if source_revision != current_revision(&source)? || source_generation != generation { return Err(validation("snapshot original source identity differs")); }
            let reader = open_generation_reader(&candidate,&generation)?;
            let stage = self.stage_portable_records(&reader,&crate::local_backup::NeverCancelled)?;
            let result = (|| {
                let mut original_units=std::collections::BTreeMap::new();
                {
                    let mut rows=source.prepare("SELECT key,value FROM snapshot_original_units ORDER BY key")?;
                    let mut cursor=rows.query([])?;
                    while let Some(row)=cursor.next()? {
                        let key:risunest_sync_wire::unit::UnitKey=row.get::<_,String>(0)?.try_into().map_err(|error:risunest_sync_wire::WireError|validation(error.to_string()))?;
                        let value:risunest_sync_wire::unit::UnitValue=serde_json::from_str(&row.get::<_,String>(1)?)?;
                        value.validate().map_err(|error|validation(error.to_string()))?;
                        if matches!(key.components().first().map(String::as_str),Some("hypa"|"plugin-local")) {return Err(validation("snapshot source contains device units"));}
                        super::lww::validate_received(&source,&key,&value)?;
                        original_units.insert(key,value);
                    }
                }
                let inventory=super::external_capture::original_unit_dependency_inventory(&original_units,&|hash| {
                    let length:Option<i64>=source.query_row("SELECT length(body) FROM message_page_objects WHERE hash=?1",[hash],|row|row.get(0)).optional()?;
                    if length.is_some_and(|length|length<0 || length as u64>risunest_sync_wire::MAX_METADATA_BYTES as u64) {return Err(validation("snapshot control exceeds its byte limit"));}
                    Ok(source.query_row("SELECT body FROM message_page_objects WHERE hash=?1",[hash],|row|row.get(0)).optional()?)
                },&|hash| {
                    let size:Option<i64>=source.query_row("SELECT byte_size FROM asset_objects WHERE object_hash=?1 UNION ALL SELECT length(body) FROM message_page_objects WHERE hash=?1 LIMIT 1",[hash],|row|row.get(0)).optional()?;
                    size.map(|size|u64::try_from(size).map_err(|_|validation("snapshot payload size is invalid"))).transpose()
                },&crate::local_backup::NeverCancelled,false,&mut |_,_,_|Ok(()))?;
                let authority = self.lww_binding_authority()?;
                let tx = self.connection.transaction()?;
                tx.execute("INSERT INTO snapshot_restore_stages VALUES(?1,?2,?3,?4,'staged',NULL)",rusqlite::params![stage.staging_id,request_id,id,authority.0.to_string()])?;
                let mut payloads=source.prepare("SELECT object_hash,byte_size FROM asset_objects ORDER BY object_hash")?;
                let mut payload_rows=payloads.query([])?;
                while let Some(row)=payload_rows.next()? {
                    let hash:String=row.get(0)?;
                    let size:i64=row.get(1)?;
                    let owner:bool=source.query_row("SELECT EXISTS(SELECT 1 FROM asset_owner_heads WHERE manifest_hash=?1 AND present=1)",[&hash],|row|row.get(0))?;
                    tx.execute("INSERT INTO snapshot_restore_payloads VALUES(?1,?2,?3,?4,0)",rusqlite::params![stage.staging_id,hash,size,owner])?;
                }
                for (hash,size) in inventory.payloads {
                    let size=size.ok_or_else(||validation("snapshot payload metadata is unavailable"))?;
                    let previous:Option<i64>=tx.query_row("SELECT byte_size FROM snapshot_restore_payloads WHERE stage_id=?1 AND hash=?2",rusqlite::params![stage.staging_id,hash],|row|row.get(0)).optional()?;
                    if previous.is_some_and(|previous|previous as u64!=size) {return Err(validation("snapshot payload metadata differs"));}
                    let cached:bool=source.query_row("SELECT EXISTS(SELECT 1 FROM message_page_objects WHERE hash=?1)",[&hash],|row|row.get(0))?;
                    tx.execute("INSERT OR IGNORE INTO snapshot_restore_payloads VALUES(?1,?2,?3,0,?4)",rusqlite::params![stage.staging_id,hash,i64::try_from(size).map_err(|_|validation("snapshot payload size exceeds SQLite range"))?,cached])?;
                    if cached {tx.execute("UPDATE snapshot_restore_payloads SET cached=1 WHERE stage_id=?1 AND hash=?2",rusqlite::params![stage.staging_id,hash])?;}
                }
                for (key,value) in &original_units {
                    tx.execute("INSERT INTO snapshot_restore_units VALUES(?1,?2,?3)",rusqlite::params![stage.staging_id,key.as_str(),serde_json::to_string(value)?])?;
                }
                let mut statement = source.prepare("SELECT o.hash,o.body FROM message_page_objects o JOIN message_page_verified_objects v ON v.hash=o.hash")?;
                let mut rows = statement.query([])?;
                while let Some(row) = rows.next()? {
                    super::message_pages::put_object(&tx,&row.get::<_,String>(0)?,&row.get::<_,Vec<u8>>(1)?)?;
                }
                tx.commit()?;
                Ok(super::StagingResult {staging_id:stage.staging_id.clone()})
            })();
            if result.is_err() { self.replace_abort(&stage.staging_id)?; }
            result
        })();
        if let Ok(stage)=&outcome {
            if let Err(failure)=fs::rename(&candidate,self.snapshot_restore_source_path(&stage.staging_id)?) {
                self.snapshot_restore_abort(&stage.staging_id)?;
                remove_database_files(&candidate)?;
                return Err(failure.into());
            }
        } else { remove_database_files(&candidate)?; }
        outcome
    }

    pub(crate) fn snapshot_restore_activate(&mut self,stage_id:&str,expected_revision:i64,binding_authority:risunest_sync_wire::stamp::DecimalU64) -> StoreResult<super::RevisionResult> {
        let (request_id,authority,state,revision):(String,String,String,Option<i64>) = self.connection.query_row("SELECT request_id,authority,state,revision FROM snapshot_restore_stages WHERE stage_id=?1",[stage_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if authority != binding_authority.0.to_string() || self.lww_binding_authority()? != binding_authority { return Err(validation("snapshot replacement authority changed")); }
        if state == "committed" { return Ok(super::RevisionResult {revision:revision.ok_or_else(||validation("snapshot commit receipt missing"))?}); }
        let request_completed:bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM lww_requests WHERE request_id=?1)",[&request_id],|r|r.get(0))?;
        if !request_completed && self.revision()? != expected_revision { return Err(super::StoreError::RevisionConflict {expected:expected_revision,actual:self.revision()?}); }
        let mut units = std::collections::BTreeMap::new();
        {
            let mut statement = self.connection.prepare("SELECT key,value FROM snapshot_restore_units WHERE stage_id=?1 ORDER BY key")?;
            let mut rows = statement.query([stage_id])?;
            while let Some(row) = rows.next()? { units.insert(row.get::<_,String>(0)?.try_into().map_err(|error:risunest_sync_wire::WireError|validation(error.to_string()))?,serde_json::from_str(&row.get::<_,String>(1)?)?); }
        }
        let header = super::lww::Header {binding_authority,request_id};
        let result = self.lww_commit_replacement_units(&header,stage_id,Some(&units))?;
        self.connection.execute("UPDATE snapshot_restore_stages SET state='committed',revision=?2 WHERE stage_id=?1",rusqlite::params![stage_id,result.revision])?;
        Ok(result)
    }

    pub(crate) fn snapshot_restore_abort(&mut self,stage_id:&str) -> StoreResult<()> {
        let request:Option<String> = self.connection.query_row("SELECT request_id FROM snapshot_restore_stages WHERE stage_id=?1 AND state='staged'",[stage_id],|r|r.get(0)).optional()?;
        if let Some(request) = request {
            let issued:bool = self.device_store()?.connection().query_row("SELECT EXISTS(SELECT 1 FROM lww_intents WHERE request_id=?1)",[request],|r|r.get(0))?;
            if issued { return Err(validation("snapshot activation has already been submitted")); }
            self.replace_abort(stage_id)?;
            self.connection.execute("DELETE FROM snapshot_restore_stages WHERE stage_id=?1",[stage_id])?;
            remove_database_files(&self.snapshot_restore_source_path(stage_id)?)?;
        }
        Ok(())
    }

    pub(crate) fn snapshot_restore_source_path(&self,stage_id:&str)->StoreResult<PathBuf> {
        let id=stage_id.strip_prefix("staging-").ok_or_else(||validation("invalid snapshot stage identity"))?;
        let parsed=Uuid::parse_str(id).map_err(|_|validation("invalid snapshot stage identity"))?;
        if parsed.to_string()!=id {return Err(validation("invalid snapshot stage identity"));}
        Ok(self.snapshots_dir.join(format!("restore-source-{stage_id}.sqlite")))
    }
}
