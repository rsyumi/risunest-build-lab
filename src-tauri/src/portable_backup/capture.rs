use super::*;
use crate::{
    asset_repository::{job_pins::DurableCasJob, PayloadCas},
    persistent_store::PersistentStore,
};
use std::path::Path;

pub(crate) struct CapturedLibrary {
    pub(crate) catalog: Catalog,
    pub(crate) repair_required: bool,
}

pub(crate) fn capture_library(
    store: &mut PersistentStore,
    revision: i64,
    job_directory: &Path,
    pins: &mut DurableCasJob,
    preservation: bool,
    probe: &dyn CancellationProbe,
    source_build: &str,
) -> Result<CapturedLibrary> {
    capture_library_with_ready(store, revision, job_directory, pins, preservation, probe, source_build, &|| Ok(()))
}

pub(crate) fn capture_library_with_ready(
    store: &mut PersistentStore,
    revision: i64,
    job_directory: &Path,
    pins: &mut DurableCasJob,
    preservation: bool,
    probe: &dyn CancellationProbe,
    source_build: &str,
    ready: &dyn Fn() -> Result<()>,
) -> Result<CapturedLibrary> {
    capture_library_inner(store, revision, job_directory, pins, preservation, probe, source_build, true, ready)
}

pub(crate) fn capture_library_only(
    store: &mut PersistentStore,
    revision: i64,
    job_directory: &Path,
    pins: &mut DurableCasJob,
    preservation: bool,
    probe: &dyn CancellationProbe,
    source_build: &str,
) -> Result<CapturedLibrary> {
    capture_library_inner(store, revision, job_directory, pins, preservation, probe, source_build, false, &|| Ok(()))
}

fn capture_library_inner(
    store: &mut PersistentStore,
    revision: i64,
    job_directory: &Path,
    pins: &mut DurableCasJob,
    preservation: bool,
    probe: &dyn CancellationProbe,
    source_build: &str,
    include_device: bool,
    ready: &dyn Fn() -> Result<()>,
) -> Result<CapturedLibrary> {
    check(probe)?;
    let (capture,device_sections)=if include_device {
        store.lww_acquire_backup_capture(revision)?
    } else {
        (store.lww_acquire_library_backup_capture(revision)?, Vec::new())
    };
    let lease = capture.lease;
    let outcome = (|| {
        ready()?;
        #[cfg(test)] super::source_io::capture_scope().capture_ready(&lease,revision,store.lww_backup_device_revision(&lease)?);
        let mut catalog = Catalog::create(job_directory, source_build, revision)?;
        let generation = store.portable_source_generation(&lease)?;
        catalog.db.execute(
            "INSERT INTO backup_info VALUES('sourceGeneration',?1)",
            [&generation],
        )?;
        if let Err(error) = store.capture_portable_records(&lease, &mut catalog.db, probe) {
            check(probe)?;
            if !matches!(error, StoreError::Validation { .. }) {
                return Err(error.into());
            }
            if !preservation {
                return Err(Error::SourceNeedsPreservation);
            }
            // Unknown raw schema is the explicit physical SQLite salvage profile. This snapshot
            // may contain operational tables and is never eligible for normal activation.
            let source = catalog.directory.path().join("source.sqlite");
            store.capture_preservation_database(&lease, &source, probe)?;
            catalog.db.execute(
                "UPDATE backup_info SET value='source-sqlite' WHERE key='profile'",
                [],
            )?;
            catalog.add_file(
                "preserved",
                "source.sqlite",
                "{\"profile\":\"source-sqlite\"}",
                Some((&source, std::fs::metadata(&source)?.len())),
                None,
                probe,
            )?;
        }
        let source_dependencies=store.capture_portable_units(&lease,&catalog.db,probe)?;
        let mut payloads=catalog.db.prepare("SELECT hash,body FROM backup_payload_spool ORDER BY hash")?;
        let mut rows=payloads.query([])?;
        while let Some(row)=rows.next()? {
            check(probe)?;
            let hash:String=row.get(0)?;
            let body:Vec<u8>=row.get(1)?;
            catalog.add_reader("unit",&hash,"{}",&mut body.as_slice(),body.len() as u64,&hash,probe)?;
        }
        drop(rows);
        drop(payloads);
        catalog.db.execute_batch("DROP TABLE backup_payload_spool")?;
        for section in &device_sections {
            crate::device_backup::write_native_section(&catalog,section,probe)
                .map_err(|error| Error::Io(std::io::Error::other(error.to_string())))?;
        }
        catalog.begin_inventory()?;
        let cas = PayloadCas::new(store.repository_root())?;
        for (hash,size) in &source_dependencies.payloads {
            if source_dependencies.spooled_payloads.contains(hash) {continue;}
            let size=size.ok_or(Error::Invalid("original payload size is unavailable"))?;
            check(probe)?;
            if cas.stat_object(hash)? == Some(size) {
                pins.pin_existing(&cas,hash,size,crate::asset_repository::job_pins::CasObjectRole::DirectObject)?;
                let path=cas.object_path(hash)?.ok_or(Error::Invalid("pinned backup payload disappeared"))?;
                catalog.add_pinned_file("unit",hash,"{}",&path,size,hash,probe)?;
            } else {
                capture_remote_payload(&catalog,store.repository_root(),"unit",hash,"{}",hash,Some(size),probe)?;
            }
        }
        match catalog.capture_files(store, &cas, pins, preservation, probe) {
            Err(Error::Invalid("registered source payload differs")) if !preservation => {
                return Err(Error::SourceNeedsPreservation)
            }
            result => result?,
        }
        catalog.db.execute_batch("CREATE TEMP TABLE missing_sources AS SELECT kind,logical_key,metadata,expected_hash FROM files WHERE state='missing'")?;
        let mut missing = catalog.db.prepare("SELECT kind,logical_key,metadata,expected_hash FROM missing_sources ORDER BY kind,logical_key")?;
        let mut rows=missing.query([])?;
        while let Some(row)=rows.next()? {
            let kind:String=row.get(0)?;
            let key:String=row.get(1)?;
            let metadata:String=row.get(2)?;
            let hash:Option<String>=row.get(3)?;
            let hash=hash.ok_or(Error::Invalid("backup payload has no verified source identity"))?;
            catalog.db.execute("DELETE FROM files WHERE kind=?1 AND logical_key=?2",rusqlite::params![kind,key])?;
            if !catalog.reference_captured_object(&kind,&key,&metadata,&hash)? {
                capture_remote_payload(&catalog,store.repository_root(),&kind,&key,&metadata,&hash,None,probe)?;
            }
            catalog.db.execute("DELETE FROM diagnostics WHERE code='missing-file' AND subject=?1",[&key])?;
        }
        drop(rows);
        drop(missing);
        catalog.db.execute_batch("DROP TABLE missing_sources")?;
        let validation = catalog.validate_library(probe);
        check(probe)?;
        let repair_required = match validation {
            Ok(_) => false,
            Err(
                Error::Invalid(_) | Error::Json(_) | Error::Store(StoreError::Validation { .. }),
            ) => true,
            Err(error) => return Err(error),
        };
        if repair_required {
            if !preservation {
                return Err(Error::SourceNeedsPreservation);
            }
            catalog.db.execute(
                "INSERT INTO diagnostics VALUES('source-preserved-repair-required','library')",
                [],
            )?;
        }
        Ok(CapturedLibrary {
            catalog,
            repair_required,
        })
    })();
    let release = store.release_revision(&lease);
    match (outcome, release) {
        (Ok(capture), Ok(_)) => Ok(capture),
        (Err(error), Ok(_)) => Err(error),
        (_, Err(error)) => Err(error.into()),
    }
}

fn capture_remote_payload(catalog:&Catalog,root:&Path,kind:&str,key:&str,metadata:&str,hash:&str,expected_size:Option<u64>,probe:&dyn CancellationProbe)->Result<()> {
    check(probe)?;
    let server_check=|| {
        if probe.is_cancelled() {Err(crate::server_sync::SyncError::new("cancelled",499))} else {Ok(())}
    };
    if let Some(mut body)=crate::server_sync::residency::open_transient_server_with_check(root,catalog.directory.path(),hash,&server_check)
        .map_err(|error|Error::Io(std::io::Error::other(error.code)))? {
        let size=body.len()?;
        if expected_size.is_some_and(|expected|expected!=size) {return Err(Error::Invalid("remote backup payload size mismatch"));}
        return catalog.add_reader(kind,key,metadata,&mut body,size,hash,probe);
    }
    let cancel=probe.cancellation_flag().map(crate::external_storage::contract::Cancellation::with_external_flag).unwrap_or_default();
    let mut body=tauri::async_runtime::block_on(crate::external_storage::lww_residency::spool_verified_remote_body(root,hash,catalog.directory.path(),&cancel))
        .map_err(|error|Error::Io(std::io::Error::other(error.to_string())))?
        .ok_or(Error::Invalid("required backup payload is unavailable"))?;
    check(probe)?;
    let size=body.as_file().metadata()?.len();
    if expected_size.is_some_and(|expected|expected!=size) {return Err(Error::Invalid("remote backup payload size mismatch"));}
    catalog.add_reader(kind,key,metadata,body.as_file_mut(),size,hash,probe)
}
