use super::{json, random_id, uploads::now, Device, Store};
use crate::{Error, Result};
use risunest_sync_wire::{Domain, RemoteHead, Sequence};
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceResult {
    pub min_retained_seq: Sequence,
    pub objects_removed: u64,
    pub wal_checkpoint: WalCheckpoint,
    pub trash_failed: u64,
    pub trash_backlog: u64,
}
#[derive(Default)]
pub struct TrashPass {
    pub selected: u64,
    pub removed: u64,
    pub objects_removed: u64,
    pub failed: u64,
    pub backlog: u64,
}
/// What `PRAGMA wal_checkpoint` reported: whether a reader or writer blocked it,
/// how many frames the log held, and how many reached the database file.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalCheckpoint {
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
}
impl WalCheckpoint {
    pub fn incomplete(&self) -> bool {
        self.busy != 0 || self.checkpointed_frames < self.log_frames
    }
}
fn checkpoint(db: &Connection) -> Result<WalCheckpoint> {
    let (busy, log_frames, checkpointed_frames) =
        db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    Ok(WalCheckpoint {
        busy,
        log_frames,
        checkpointed_frames,
    })
}
#[derive(Default)]
pub(super) struct OrphanScan {
    shard: u8,
    objects: Option<std::fs::ReadDir>,
    staging: Option<std::fs::ReadDir>,
}
impl Store {
    fn reconcile_orphans(&self, db: &Connection) -> Result<()> {
        let mut scan = self
            .orphan_cursor
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        if scan.objects.is_none() {
            let path = self
                .root
                .join("objects")
                .join(format!("{:02x}", scan.shard));
            super::objects::check_path(&path)?;
            scan.objects = match std::fs::read_dir(path) {
                Ok(entries) => Some(entries),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
        }
        let entries = scan
            .objects
            .as_mut()
            .map(|entries| entries.by_ref().take(1024).collect::<Vec<_>>())
            .unwrap_or_default();
        if entries.len() < 1024 {
            scan.objects = None;
            scan.shard = scan.shard.wrapping_add(1);
        }
        for entry in entries {
            let entry = entry?;
            let hash = entry.file_name().to_string_lossy().into_owned();
            if risunest_sync_wire::validate_hash(&hash).is_err()
                || entry.path() != self.object_path(&hash)?
            {
                continue;
            }
            if !entry.file_type()?.is_file() {
                continue;
            }
            db.execute("INSERT OR IGNORE INTO object_trash SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM objects WHERE hash=?1)", [&hash])?;
        }
        if scan.staging.is_none() {
            scan.staging = Some(std::fs::read_dir(self.root.join("staging"))?);
        }
        let entries = scan
            .staging
            .as_mut()
            .unwrap()
            .by_ref()
            .take(1024)
            .collect::<Vec<_>>();
        if entries.len() < 1024 {
            scan.staging = None;
        }
        let mut staging_changed = false;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".risunest-tmp-") {
                let paths = self
                    .temporary_paths
                    .lock()
                    .map_err(|_| Error::new("storage-unavailable", 503))?;
                if paths.contains(&entry.path()) {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if meta.is_file()
                    && meta
                        .modified()?
                        .elapsed()
                        .is_ok_and(|age| age.as_secs() >= 3600)
                {
                    std::fs::remove_file(entry.path())?;
                    staging_changed = true;
                }
                continue;
            }
            let Some((upload, ordinal)) = name
                .strip_suffix(".chunk")
                .and_then(|name| name.rsplit_once('-'))
            else {
                continue;
            };
            if risunest_sync_wire::validate_id(upload).is_err() {
                continue;
            }
            let Ok(ordinal) = ordinal.parse::<i64>() else {
                continue;
            };
            if ordinal < 0 || !entry.file_type()?.is_file() {
                continue;
            }
            db.execute("INSERT OR IGNORE INTO staging_trash SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM upload_chunks WHERE upload=?1 AND ordinal=?2)", params![upload, ordinal])?;
        }
        if staging_changed {
            super::objects::sync_directory(&self.root.join("staging"))?;
        }
        Ok(())
    }
    pub(super) fn drain_staging_trash(
        &self,
        db: &Connection,
        only: Option<&str>,
    ) -> Result<TrashPass> {
        let mut cursors = self
            .trash_cursors
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        let rows = {
            let mut statement = db.prepare("SELECT rowid,upload,ordinal FROM staging_trash WHERE (?1 IS NULL AND rowid>?2) OR upload=?1 ORDER BY rowid LIMIT 1024")?;
            let rows = statement
                .query_map(params![only, cursors.0], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        if only.is_none() {
            cursors.0 = if rows.len() < 1024 {
                0
            } else {
                rows.last().unwrap().0
            };
        }
        drop(cursors);
        let mut pass = TrashPass {
            selected: rows.len() as u64,
            ..Default::default()
        };
        let mut removed = Vec::new();
        for (row, upload, ordinal) in rows {
            let index =
                u64::try_from(ordinal).map_err(|_| Error::new("corrupt-staging-trash", 503))?;
            match std::fs::remove_file(self.chunk_path(&upload, index)?) {
                Ok(()) => removed.push(row),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => removed.push(row),
                Err(_) => pass.failed += 1,
            }
        }
        if !removed.is_empty() {
            super::objects::sync_directory(&self.root.join("staging"))?;
            let tx = db.unchecked_transaction()?;
            for row in &removed {
                tx.execute("DELETE FROM staging_trash WHERE rowid=?1", [row])?;
            }
            tx.commit()?;
        }
        pass.removed = removed.len() as u64;
        Ok(pass)
    }
    pub fn drain_trash(&self) -> Result<TrashPass> {
        let _gate = self
            .objects_gate
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        self.drain_trash_locked(&mut *self.db()?)
    }
    fn drain_trash_locked(&self, db: &mut Connection) -> Result<TrashPass> {
        let mut pass = self.drain_staging_trash(db, None)?;
        let mut cursors = self
            .trash_cursors
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        let rows = {
            let mut statement = db.prepare(
                "SELECT rowid,hash FROM object_trash WHERE rowid>?1 ORDER BY rowid LIMIT 1024",
            )?;
            let rows = statement
                .query_map([cursors.1], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        cursors.1 = if rows.len() < 1024 {
            0
        } else {
            rows.last().unwrap().0
        };
        drop(cursors);
        pass.selected += rows.len() as u64;
        let mut removed = Vec::new();
        let mut directories = std::collections::BTreeSet::new();
        let mut obsolete = Vec::new();
        for (row, digest) in rows {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE hash=?1)",
                [&digest],
                |r| r.get(0),
            )?;
            if exists {
                obsolete.push(row);
                continue;
            }
            let path = self.object_path(&digest)?;
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    directories.insert(path.parent().unwrap().to_owned());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let parent = path.parent().unwrap();
                    directories.insert(if parent.is_dir() {
                        parent.to_owned()
                    } else {
                        self.root.join("objects")
                    });
                }
                Err(_) => {
                    pass.failed += 1;
                    continue;
                }
            }
            removed.push((row, digest));
        }
        for directory in directories {
            super::objects::sync_directory(&directory)?;
        }
        let hashes = removed
            .iter()
            .map(|(_, hash)| hash.as_str())
            .collect::<Vec<_>>();
        let tx = db.transaction()?;
        risunest_small_object_store::delete_batch(&tx, &hashes)
            .map_err(super::objects::body_error)?;
        for row in obsolete.iter().chain(removed.iter().map(|(row, _)| row)) {
            tx.execute("DELETE FROM object_trash WHERE rowid=?1", [row])?;
        }
        tx.commit()?;
        pass.objects_removed = removed.len() as u64;
        pass.removed += pass.objects_removed;
        pass.backlog = db.query_row(
            "SELECT (SELECT count(*) FROM object_trash)+(SELECT count(*) FROM staging_trash)",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;
        if !removed.is_empty() {
            db.execute_batch("PRAGMA incremental_vacuum;")?;
        }
        Ok(pass)
    }
    pub(super) fn lease_object(db: &Connection, device: &Device, digest: &str) -> Result<()> {
        db.execute("INSERT INTO object_leases VALUES(?1,?2,?3) ON CONFLICT(device,hash) DO UPDATE SET expires=excluded.expires",params![device.id,digest,now()?+86400])?;
        Ok(())
    }
    pub fn pin_objects(&self, device: &Device, hashes: &[String]) -> Result<()> {
        if hashes.len() > 1024 {
            return Err(Error::new("too-many-candidates", 400));
        }
        let mut db = self.db()?;
        Self::require_device(&db, device)?;
        let tx = db.transaction()?;
        for digest in hashes {
            risunest_sync_wire::validate_hash(digest)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE hash=?1)",
                [digest],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(Error::new("object-not-found", 404));
            }
            Self::lease_object(&tx, device, digest)?;
        }
        tx.commit()?;
        Ok(())
    }
    /// A revoked device cannot authenticate again, so its acknowledgements and
    /// receipts are unreadable once no commit job of its own is pending. The row
    /// itself leaves only when nothing else still names it, which keeps custody
    /// held for a replacement registration and work that is still finishing.
    fn purge_revoked_devices(tx: &Connection) -> Result<()> {
        tx.execute("DELETE FROM device_section_acks WHERE device IN (SELECT id FROM devices WHERE revoked=1)",[])?;
        tx.execute("DELETE FROM receipts WHERE device IN (SELECT id FROM devices WHERE revoked=1 AND NOT EXISTS(SELECT 1 FROM commit_jobs WHERE device=devices.id))",[])?;
        tx.execute(
            "DELETE FROM devices WHERE revoked=1
             AND NOT EXISTS(SELECT 1 FROM device_section_acks WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM object_leases WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM object_custody WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM read_pins WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM checkpoints WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM uploads WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM download_deltas WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM staged_changes WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM receipts WHERE device=devices.id)
             AND NOT EXISTS(SELECT 1 FROM commit_jobs WHERE device=devices.id)",
            [],
        )?;
        Ok(())
    }
    /// Explicit maintenance uses every active device acknowledgement and read pin.
    /// Offline devices are never silently forgotten. Tombstones stay as identity
    /// fences; payloads and acknowledged journal bodies can be reclaimed.
    pub fn maintain(&self) -> Result<MaintenanceResult> {
        let _gate = self
            .objects_gate
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        let current = now()?;
        tx.execute("DELETE FROM download_deltas WHERE state!='working' AND (expires<=?1 OR device IN (SELECT id FROM devices WHERE revoked=1))", [current])?;
        tx.execute("DELETE FROM upload_deltas WHERE upload IN (SELECT id FROM uploads WHERE state='complete')", [])?;
        tx.execute("DELETE FROM upload_delta_bases WHERE upload IN (SELECT id FROM uploads WHERE state='complete')", [])?;
        tx.execute("INSERT OR IGNORE INTO staging_trash SELECT c.upload,c.ordinal FROM upload_chunks c JOIN uploads u ON c.upload=u.id WHERE u.state!='finalizing' AND (u.state='complete' OR u.expires<=?1 OR u.device IN (SELECT id FROM devices WHERE revoked=1))",[current])?;
        tx.execute("DELETE FROM upload_chunks WHERE (upload,ordinal) IN (SELECT upload,ordinal FROM staging_trash)",[])?;
        tx.execute("DELETE FROM uploads WHERE state!='finalizing' AND (expires<=?1 OR device IN (SELECT id FROM devices WHERE revoked=1))",[current])?;
        tx.execute("DELETE FROM read_pins WHERE expires<=?1 OR device IN (SELECT id FROM devices WHERE revoked=1)",[current])?;
        tx.execute("DELETE FROM checkpoints WHERE expires<=?1 OR device IN (SELECT id FROM devices WHERE revoked=1)",[current])?;
        tx.execute("DELETE FROM object_leases WHERE expires<=?1 OR device IN (SELECT id FROM devices WHERE revoked=1)",[current])?;
        tx.execute("DELETE FROM staged_changes WHERE (expires<=?1 OR device IN (SELECT id FROM devices WHERE revoked=1)) AND id NOT IN (SELECT stage FROM commit_jobs)",[current])?;
        Self::purge_revoked_devices(&tx)?;
        let mut head = Self::read_head(&tx)?;
        let mut pinned = head.seq.clone();
        {
            let mut stmt = tx.prepare("SELECT after_seq FROM read_pins")?;
            for value in stmt.query_map([], |r| r.get::<_, String>(0))? {
                pinned = pinned.min(Sequence::try_from(value?)?);
            }
        }
        // One section's acknowledgement never reclaims another section's journal.
        let mut floor: Option<Sequence> = None;
        for domain in Domain::ALL {
            let section_floor = Self::read_section_ack_floor(&tx, domain, &head.seq)?
                .min(pinned.clone())
                .max(head.section(domain)?.gc_floor.clone());
            tx.execute(
                "DELETE FROM changes WHERE domain=?1 AND (length(seq),seq)<=(?2,?3)",
                params![
                    domain.as_str(),
                    section_floor.as_str().len() as i64,
                    section_floor.as_str()
                ],
            )?;
            floor = Some(match floor {
                Some(value) => value.min(section_floor.clone()),
                None => section_floor.clone(),
            });
            let section = head
                .sections
                .get_mut(&domain)
                .ok_or(Error::new("corrupt-metadata", 503))?;
            section.gc_floor = section_floor;
        }
        let floor = floor
            .ok_or(Error::new("corrupt-metadata", 503))?
            .max(head.min_retained_seq.clone());
        // Receipts are pruned only once their resulting head is acknowledged.
        tx.execute("DELETE FROM receipts WHERE created<=unixepoch()-86400 AND (length(json_extract(body,'$.head.seq')),json_extract(body,'$.head.seq'))<=(?1,?2)",params![floor.as_str().len() as i64,floor.as_str()])?;
        tx.execute(
            "DELETE FROM commits WHERE (length(seq),seq)<(?1,?2)",
            params![floor.as_str().len() as i64, floor.as_str()],
        )?;
        head.min_retained_seq = floor.clone();
        tx.execute("UPDATE library SET head=?1", [json(&head)?])?;
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS gc_roots(hash TEXT PRIMARY KEY); DELETE FROM gc_roots;
            INSERT OR IGNORE INTO gc_roots SELECT hash FROM object_leases;
            INSERT OR IGNORE INTO gc_roots SELECT hash FROM object_custody;
            INSERT OR IGNORE INTO gc_roots SELECT hash FROM uploads WHERE expires>unixepoch();
            INSERT OR IGNORE INTO gc_roots SELECT hash FROM upload_delta_bases;
            INSERT OR IGNORE INTO gc_roots SELECT hash FROM download_delta_bases;
            CREATE TEMP TABLE IF NOT EXISTS gc_versions(body TEXT); DELETE FROM gc_versions;
            INSERT INTO gc_versions SELECT version FROM records UNION ALL SELECT version FROM checkpoint_records;
            INSERT INTO gc_versions SELECT json_extract(body,'$.before') FROM changes UNION ALL SELECT json_extract(body,'$.after') FROM changes;
            INSERT INTO gc_versions SELECT json_extract(body,'$.before') FROM staged_records UNION ALL SELECT json_extract(body,'$.after') FROM staged_records UNION ALL SELECT json_extract(body,'$.version') FROM staged_fences;
            INSERT OR IGNORE INTO gc_roots SELECT json_extract(body,'$.objectHash') FROM gc_versions WHERE json_extract(body,'$.objectHash') IS NOT NULL;
            INSERT OR IGNORE INTO gc_roots SELECT json_extract(body,'$.descriptorHash') FROM gc_versions WHERE json_extract(body,'$.descriptorHash') IS NOT NULL;
            CREATE TEMP TABLE IF NOT EXISTS gc_alive(hash TEXT PRIMARY KEY); DELETE FROM gc_alive;
            WITH RECURSIVE edges(source,target) AS (
                SELECT hash,object FROM descriptors UNION ALL SELECT hash,json_extract(body,'$.dependencyRoot') FROM descriptors UNION ALL SELECT hash,json_extract(body,'$.relationRoot') FROM descriptors
                UNION ALL SELECT descriptors.hash,json_each.value FROM descriptors,json_each(descriptors.body,'$.dependencies')
                UNION ALL SELECT root,child FROM reference_children UNION ALL SELECT root,object FROM reference_objects
            ), alive(hash) AS (SELECT hash FROM gc_roots UNION SELECT target FROM edges JOIN alive ON source=alive.hash WHERE target IS NOT NULL)
            INSERT INTO gc_alive SELECT hash FROM alive;
            DELETE FROM descriptors WHERE hash NOT IN gc_alive;
            DELETE FROM reference_children WHERE root NOT IN gc_alive;
            DELETE FROM reference_objects WHERE root NOT IN gc_alive;
            DELETE FROM reference_relations WHERE root NOT IN gc_alive;
            DELETE FROM reference_nodes WHERE hash NOT IN gc_alive;
            INSERT OR IGNORE INTO object_trash SELECT hash FROM objects WHERE hash NOT IN gc_alive;
            DELETE FROM objects WHERE hash NOT IN gc_alive;
            DELETE FROM gc_versions; DELETE FROM gc_roots; DELETE FROM gc_alive;")?;
        tx.commit()?;
        self.announce_head();
        self.reconcile_orphans(&db)?;
        let trash = self.drain_trash_locked(&mut db)?;
        // A write-ahead log that never folds back grows without bound. Its three
        // result values travel with the maintenance outcome so a checkpoint the
        // database refuses is visible instead of silent.
        let wal_checkpoint = checkpoint(&db)?;
        Ok(MaintenanceResult {
            min_retained_seq: floor,
            objects_removed: trash.objects_removed,
            trash_failed: trash.failed,
            trash_backlog: trash.backlog,
            wal_checkpoint,
        })
    }
    /// Folds the write-ahead log back into the database file. Called on a clean
    /// stop so the next start does not rebuild an index over stale frames.
    pub fn checkpoint_wal(&self) -> Result<WalCheckpoint> {
        checkpoint(&*self.db()?)
    }
    /// Call only after restoring a stopped, complete server-directory backup.
    /// Old clients must reconcile against the restored checkpoint under a new epoch.
    pub fn rotate_restored_epoch(&self) -> Result<()> {
        let _gate = self
            .objects_gate
            .lock()
            .map_err(|_| Error::new("storage-unavailable", 503))?;
        let mut db = self.db()?;
        let tx = db.transaction()?;
        let head = RemoteHead::genesis(Self::read_head(&tx)?.library_id, random_id()?)?;
        tx.execute(
            "INSERT OR IGNORE INTO staging_trash SELECT upload,ordinal FROM upload_chunks",
            [],
        )?;
        tx.execute_batch("DELETE FROM changes; DELETE FROM commits; DELETE FROM receipts; DELETE FROM commit_jobs; DELETE FROM staged_changes; DELETE FROM read_pins; DELETE FROM checkpoints; DELETE FROM uploads; DELETE FROM download_deltas; DELETE FROM object_leases; DELETE FROM scope_versions; DELETE FROM device_section_acks;")?;
        tx.execute("UPDATE library SET head=?1", [json(&head)?])?;
        tx.commit()?;
        self.announce_head();
        self.drain_staging_trash(&db, None)?;
        Ok(())
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn orphan_reconciliation_preserves_resumable_chunks_and_reclaims_unaccounted_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path()).unwrap();
        let device = store.add_device().unwrap();
        let digest = risunest_sync_wire::hash(b"orphan");
        let object = store.object_path(&digest).unwrap();
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&object, b"orphan").unwrap();
        store.orphan_cursor.lock().unwrap().shard = u8::from_str_radix(&digest[..2], 16).unwrap();
        let live = "a".repeat(64);
        let orphan = "b".repeat(64);
        {
            let db = store.db().unwrap();
            db.execute("INSERT INTO uploads(id,device,hash,size,expires) VALUES(?1,?2,?3,6,unixepoch()+3600)", params![live, device.device_id, digest]).unwrap();
            db.execute(
                "INSERT INTO upload_chunks VALUES(?1,0,?2,6)",
                params![live, digest],
            )
            .unwrap();
        }
        std::fs::write(store.chunk_path(&live, 0).unwrap(), b"resume").unwrap();
        std::fs::write(store.chunk_path(&orphan, 0).unwrap(), b"orphan").unwrap();
        store.maintain().unwrap();
        assert!(!object.exists());
        assert!(!store.chunk_path(&orphan, 0).unwrap().exists());
        assert!(store.chunk_path(&live, 0).unwrap().exists());
    }

    #[test]
    fn online_sweep_keeps_active_temporary_writers_even_when_old() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init(dir.path()).unwrap();
        let active = store.staging_temp().unwrap();
        let times = std::fs::FileTimes::new()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200));
        active.as_file().set_times(times).unwrap();
        let orphan = store.root.join("staging/.risunest-tmp-abandoned");
        std::fs::write(&orphan, b"partial").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&orphan)
            .unwrap()
            .set_times(times)
            .unwrap();
        store.maintain().unwrap();
        assert!(active.path().exists());
        assert!(!orphan.exists());
    }

    #[test]
    fn startup_reclaims_only_old_owned_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::init(dir.path()).unwrap());
        for folder in [dir.path().to_owned(), dir.path().join("staging")] {
            let old = folder.join(".risunest-tmp-old");
            std::fs::write(&old, b"abandoned").unwrap();
            let times = std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200));
            std::fs::OpenOptions::new()
                .write(true)
                .open(&old)
                .unwrap()
                .set_times(times)
                .unwrap();
            std::fs::write(folder.join(".risunest-tmp-recent"), b"recent").unwrap();
            std::fs::write(folder.join("unrelated"), b"keep").unwrap();
        }
        drop(Store::open(dir.path()).unwrap());
        for folder in [dir.path().to_owned(), dir.path().join("staging")] {
            assert!(!folder.join(".risunest-tmp-old").exists());
            assert_eq!(
                folder.join(".risunest-tmp-recent").exists(),
                folder == dir.path()
            );
            assert!(folder.join("unrelated").exists());
        }
    }
}
