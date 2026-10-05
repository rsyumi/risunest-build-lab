use super::*;
use crate::persistent_store::portable::digest_raw_tables;
use std::{
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
};
struct Never;
impl CancellationProbe for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn preservation_keeps_distinct_source_paths_even_when_bytes_are_live() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = fixture(directory.path());
    let hash = hex::encode(Sha256::digest(b"synthetic file bytes"));
    catalog.db.execute("INSERT INTO asset_aliases VALUES('assets/a',?1,'asset',20,'application/octet-stream','','',NULL,NULL,NULL,'{}')", [&hash]).unwrap();
    for (key, metadata) in [
        (
            "assets/orphan.bin".to_owned(),
            "{\"storage\":\"unclassified\"}",
        ),
        (
            format!("assets/objects/{}/{}", &hash[..2], &hash[2..]),
            "{\"storage\":\"cas\"}",
        ),
    ] {
        catalog
            .add_file(
                "preserved",
                &key,
                metadata,
                Some((&directory.path().join("payload"), 20)),
                None,
                &Never,
            )
            .unwrap();
    }
    let path = directory.path().join("backup.risunest");
    catalog.write_candidate(&path, false, &Never).unwrap();
    let archive =
        VerifiedArchive::open(File::open(path).unwrap(), directory.path(), &Never).unwrap();
    let inventory = RestoreInventory::build(&archive, directory.path(), &Never).unwrap();
    let store=crate::persistent_store::PersistentStore::open(directory.path()).unwrap();
    let mut pins=crate::asset_repository::job_pins::DurableCasJob::begin(directory.path(),"preserve-distinct",crate::asset_repository::job_pins::CasJobKind::LocalBackupRestore,crate::asset_repository::job_pins::CasJobOwner::for_test(),0).unwrap();
    let report = inventory
        .preserve(&archive, &store, &mut pins, &Never)
        .unwrap()
        .unwrap();
    assert_eq!(report.files, "1");
    assert_eq!(report.bytes, "20");
    let index = rusqlite::Connection::open(std::path::Path::new(&report.path).join("index.sqlite"))
        .unwrap();
    let (key, metadata): (String, String) = index
        .query_row("SELECT logical_key,metadata FROM source_files", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert!(key.ends_with("/assets/orphan.bin"));
    assert_eq!(metadata, "{\"storage\":\"unclassified\"}");
    assert_eq!(
        fs::read(
            std::path::Path::new(&report.path)
                .join("objects")
                .join(hash)
        )
        .unwrap(),
        b"synthetic file bytes"
    );
}

#[test]
fn present_unreferenced_preservation_reuses_cas_without_archive_body_io_and_rebacks_up() {
    let directory=tempfile::tempdir().unwrap();
    let catalog=fixture(directory.path());
    let hash=hex::encode(Sha256::digest(b"synthetic file bytes"));
    catalog.add_file("preserved","assets/unreferenced.bin","{}",Some((&directory.path().join("payload"),20)),Some(&hash),&Never).unwrap();
    let archive_path=directory.path().join("preserved.risunest");
    catalog.write_candidate(&archive_path,false,&Never).unwrap();
    let mut store=crate::persistent_store::PersistentStore::open(directory.path()).unwrap();
    let cas=crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let object=cas.prepare_bytes(b"synthetic file bytes").unwrap();
    store.asset_object_catalog().register(&[crate::persistent_store::asset_object_catalog::AssetObjectRegistration {object_hash:object.content_hash.clone(),byte_size:object.byte_size}],0).unwrap();
    let mut pins=crate::asset_repository::job_pins::DurableCasJob::begin(directory.path(),"preserve-present",crate::asset_repository::job_pins::CasJobKind::LocalBackupRestore,crate::asset_repository::job_pins::CasJobOwner::for_test(),0).unwrap();
    source_io::reset_source_io();
    let archive=VerifiedArchive::open_for_restore(File::open(archive_path).unwrap(),directory.path(),&Never).unwrap();
    let inventory=RestoreInventory::build(&archive,directory.path(),&Never).unwrap();
    crate::asset_repository::body_io::reset_body_io();
    let report=inventory.preserve(&archive,&store,&mut pins,&Never).unwrap().unwrap();
    let observed=source_io::take_source_io();
    assert!(observed.complete());
    assert!(observed.objects.is_empty());
    let destination=crate::asset_repository::body_io::take_body_io();
    assert!(destination.complete());
    let assets=destination.asset_work();
    assert_eq!(assets.opens,0);
    assert_eq!(assets.read_bytes,0);
    assert_eq!(assets.staging_write_attempts,0);
    assert_eq!(assets.publication_attempts,0);
    assert!(assets.body_sha.values().all(|work|work.bytes==0));
    assert_eq!(report.bytes,"0");
    let index=rusqlite::Connection::open(std::path::Path::new(&report.path).join("index.sqlite")).unwrap();
    let storage:String=index.query_row("SELECT storage_kind FROM source_files",[],|row|row.get(0)).unwrap();
    assert_eq!(storage,"cas");
    assert!(fs::read_dir(std::path::Path::new(&report.path).join("objects")).unwrap().next().is_none());
    let recapture=Catalog::create(directory.path(),"synthetic-recapture",0).unwrap();
    recapture.capture_preserved_sources(directory.path(),&Never).unwrap();
    let copied:String=recapture.db.query_row("SELECT lower(hex(object_hash)) FROM files",[],|row|row.get(0)).unwrap();
    assert_eq!(copied,hash);
    let roots=store.asset_gc_dry_run(10,None,100,0).unwrap();
    assert!(roots.report.marked_hashes.contains(&hash));
}

#[test]
fn independent_backup_reads_server_held_body_without_source_promotion_or_outbox_ack() {
    use crate::server_sync::lww_tests::{LocalServerFixture, local, put_asset, save};
    let server=LocalServerFixture::new();
    let (root,mut store)=local();
    let initial=store.replace_begin().unwrap();
    store.replace_put_root(&initial.staging_id,&serde_json::json!({"language":"ko"})).unwrap();
    store.replace_commit(&initial.staging_id,Some(0)).unwrap();
    let client=server.client(&store);
    let body=vec![73;128*1024+1];
    let hash=put_asset(&mut store,"assets/synthetic-held.bin",&body).object_hash.unwrap();
    let request=crate::server_sync::lww_tests::header(&store);
    client.push(&mut store,&request,&[]).unwrap();
    store.asset_residency_set_policy(crate::server_sync::residency::AssetPolicy::Remote,||Ok(())).unwrap();
    store.asset_residency_evict(||Ok(())).unwrap();
    let cas=crate::asset_repository::PayloadCas::new(root.path()).unwrap();
    assert!(cas.stat_object(&hash).unwrap().is_none());
    save(&mut store,&["root","language"],serde_json::json!("en"));
    assert_eq!(store.read_root(None).unwrap().value["language"],serde_json::json!("en"),"native fixture must contain a real projected root before capture");
    let authority=store.lww_binding_authority().unwrap();
    let before_outbox=serde_json::to_value(store.lww_read_outbox(authority,100).unwrap()).unwrap();
    let before_clock=serde_json::to_value(store.lww_clock_state().unwrap()).unwrap();
    let before_progress=serde_json::to_value(store.lww_receive_progress(authority).unwrap()).unwrap();
    assert!(!before_outbox["entries"].as_array().unwrap().is_empty());
    let revision=store.revision().unwrap();
    let job=tempfile::tempdir_in(root.path()).unwrap();
    let mut pins=crate::asset_repository::job_pins::DurableCasJob::begin(root.path(),"synthetic-held-backup",crate::asset_repository::job_pins::CasJobKind::OfficialPublicationOrExportPreparation,crate::asset_repository::job_pins::CasJobOwner::for_test(),0).unwrap();
    source_io::reset_source_io();
    let capture=capture_library(&mut store,revision,job.path(),&mut pins,false,&Never,"synthetic").unwrap();
    assert!(!capture.repair_required,"synthetic validation: {:?}",capture.catalog.validate_library(&Never));
    assert_eq!(source_io::snapshot_source_io().captures.len(),1);
    assert_eq!(source_io::snapshot_source_io().captures[0].revision,revision);
    let path=job.path().join("held.risunest");
    capture.catalog.write_candidate(&path,false,&Never).unwrap();
    assert_eq!(serde_json::to_value(store.lww_read_outbox(authority,100).unwrap()).unwrap(),before_outbox);
    assert_eq!(serde_json::to_value(store.lww_clock_state().unwrap()).unwrap(),before_clock);
    assert_eq!(serde_json::to_value(store.lww_receive_progress(authority).unwrap()).unwrap(),before_progress);
    assert!(cas.stat_object(&hash).unwrap().is_none());
    assert!(crate::server_sync::residency::Residency::open(root.path()).unwrap().object(&hash,None).unwrap().is_some());
    drop(server);
    let archive=VerifiedArchive::open_for_restore(File::open(path).unwrap(),job.path(),&Never).unwrap();
    let (mut input,size)=archive.open_object(&hash).unwrap();
    let mut copied=Vec::new();
    input.read_to_end(&mut copied).unwrap();
    assert_eq!(size,body.len() as u64);
    assert_eq!(copied,body);
}

#[test]
fn archived_payload_roots_are_installed_and_missing_payload_is_attributed_to_character() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = fixture(directory.path());
    let hash = hex::encode(Sha256::digest(b"synthetic file bytes"));
    let archived = serde_json::json!({"objectHash":hash,"archivedAt":1,"conversationCount":1,"messageCount":1,"assetHashes":[],"sharedObjectHash":hash,"sharedAssetHashes":[],"identityRemap":[]});
    catalog.db.execute("INSERT INTO characters VALUES('archived',0,0,0,'Archived',NULL,0,'character',NULL,NULL,?1,?2)", rusqlite::params![r#"{"chaId":"archived","name":"Archived","type":"character"}"#,archived.to_string()]).unwrap();
    let path = directory.path().join("archived.risunest");
    catalog.write_candidate(&path, false, &Never).unwrap();
    let archive = VerifiedArchive::open(File::open(path).unwrap(), directory.path(), &Never).unwrap();
    let inventory = RestoreInventory::build(&archive, directory.path(), &Never).unwrap();
    assert!(inventory.db.query_row("SELECT EXISTS(SELECT 1 FROM live_objects WHERE hash=?1)", [&hash], |row| row.get::<_,bool>(0)).unwrap());

    let missing = "f".repeat(64);
    let catalog = fixture(directory.path());
    let archived = serde_json::json!({"objectHash":missing,"archivedAt":1,"conversationCount":1,"messageCount":1,"assetHashes":[],"sharedObjectHash":hash,"sharedAssetHashes":[],"identityRemap":[]});
    catalog.db.execute("INSERT INTO characters VALUES('archived',0,0,0,'Archived',NULL,0,'character',NULL,NULL,?1,?2)", rusqlite::params![r#"{"chaId":"archived","name":"Archived","type":"character"}"#,archived.to_string()]).unwrap();
    let path = directory.path().join("missing.risunest");
    catalog.write_candidate(&path, false, &Never).unwrap();
    let archive = VerifiedArchive::open(File::open(path).unwrap(), directory.path(), &Never).unwrap();
    assert!(RestoreInventory::build(&archive, directory.path(), &Never).is_err());
    let mut findings = crate::data_health::Findings::new(100);
    archive.scan_library(&mut findings, &Never).unwrap();
    assert!(findings.items.iter().any(|finding| match finding.owner.kind.as_str() {
        "character" => finding.owner.id == "archived",
        "conversation" | "message" => finding.owner.id.split('/').next() == Some("archived"),
        _ => false,
    }));
}

fn fixture(directory: &std::path::Path) -> Catalog {
    let catalog = Catalog::create(directory, "synthetic-test", 7).unwrap();
    catalog
        .db
        .execute("INSERT INTO root VALUES(?1)", ["{ \"z\":null, \"a\":[] }"])
        .unwrap();
    fs::write(directory.join("payload"), b"synthetic file bytes").unwrap();
    fs::write(directory.join("empty"), b"").unwrap();
    for (kind, key, metadata) in [
        ("asset", "assets/a", "{\"x\":1}"),
        ("inlay", "shared", "{\"x\":2}"),
    ] {
        catalog
            .add_file(
                kind,
                key,
                metadata,
                Some((&directory.join("payload"), 20)),
                None,
                &Never,
            )
            .unwrap();
    }
    catalog
        .add_file(
            "asset",
            "empty",
            "{}",
            Some((&directory.join("empty"), 0)),
            None,
            &Never,
        )
        .unwrap();
    catalog
}

#[test]
fn zip_sqlite_round_trip_preserves_raw_values_aliases_and_empty_objects() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup.risunest");
    let catalog = fixture(directory.path());
    let before = digest_raw_tables(&catalog.db, &Never).unwrap();
    catalog.write_candidate(&path, false, &Never).unwrap();
    let archive =
        VerifiedArchive::open(File::open(&path).unwrap(), directory.path(), &Never).unwrap();
    assert_eq!(archive.manifest.object_count, "2");
    assert_eq!(archive.manifest.file_count, "3");
    assert_eq!(archive.manifest.pack_count, "1");
    assert!(!archive.manifest.device_included);
    assert_eq!(before, digest_raw_tables(&archive.db, &Never).unwrap());
    let hashes = archive
        .db
        .prepare("SELECT sha256 FROM objects ORDER BY sha256")
        .unwrap()
        .query_map([], |r| r.get::<_, Vec<u8>>(0).map(hex::encode))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for hash in hashes {
        let mut bytes = Vec::new();
        let size = archive.copy_object(&hash, &mut bytes, &Never).unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(hex::encode(Sha256::digest(&bytes)), hash);
    }
    let metadata=archive.db.prepare("SELECT metadata FROM files WHERE object_hash IS NOT NULL AND logical_key!='empty' ORDER BY kind").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().collect::<std::result::Result<Vec<_>,_>>().unwrap();
    assert_eq!(metadata, vec!["{\"x\":1}", "{\"x\":2}"]);
}

#[test]
fn corrupt_payload_and_truncated_directory_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup.risunest");
    fixture(directory.path())
        .write_candidate(&path, false, &Never)
        .unwrap();
    let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
    let start = zip.by_name("payloads/000000.bin").unwrap().data_start();
    drop(zip);
    {
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(start)).unwrap();
        file.write_all(b"X").unwrap();
    }
    assert!(VerifiedArchive::open(File::open(&path).unwrap(), directory.path(), &Never).is_err());
    {
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(file.metadata().unwrap().len() - 4).unwrap();
    }
    assert!(VerifiedArchive::open(File::open(&path).unwrap(), directory.path(), &Never).is_err());
}

#[test]
fn changed_empty_source_is_rejected_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = fixture(directory.path());
    let source: String = catalog
        .db
        .query_row(
            "SELECT path FROM sources JOIN objects USING(sha256) WHERE byte_length=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    fs::write(source, b"changed after capture").unwrap();
    assert!(catalog
        .write_candidate(&directory.path().join("candidate.part"), false, &Never)
        .is_err());
}

#[test]
fn missing_and_empty_are_distinct_and_force_repair_status() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup.risunest");
    let catalog = fixture(directory.path());
    catalog
        .add_file("asset", "absent", "{}", None, None, &Never)
        .unwrap();
    catalog.write_candidate(&path, false, &Never).unwrap();
    let archive =
        VerifiedArchive::open(File::open(&path).unwrap(), directory.path(), &Never).unwrap();
    assert!(archive.manifest.repair_required);
    assert_eq!(
        archive
            .db
            .query_row(
                "SELECT state FROM files WHERE logical_key='absent'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "missing"
    );
    assert_eq!(
        archive
            .db
            .query_row(
                "SELECT state FROM files WHERE logical_key='empty'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "present"
    );
}

#[test]
fn source_hash_and_length_mismatch_never_become_successful_objects() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::create(directory.path(), "synthetic", 0).unwrap();
    let source = directory.path().join("source");
    fs::write(&source, b"abc").unwrap();
    for (size, hash) in [(2, None), (4, None), (3, Some(EMPTY_HASH))] {
        assert!(catalog
            .add_file("asset", "a", "{}", Some((&source, size)), hash, &Never)
            .is_err());
    }
    assert_eq!(
        catalog
            .db
            .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn native_file_lifecycle_duplicate_stream_object_drops_its_unreferenced_temporary_spool() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = fixture(directory.path());
    let before = fs::read_dir(catalog.directory.path()).unwrap().count();
    let bytes = b"synthetic file bytes";
    let hash = hex::encode(Sha256::digest(bytes));
    let mut input = std::io::Cursor::new(bytes);

    catalog
        .add_reader(
            "device",
            &hash,
            "{}",
            &mut input,
            bytes.len() as u64,
            &hash,
            &Never,
        )
        .unwrap();

    assert_eq!(
        fs::read_dir(catalog.directory.path()).unwrap().count(),
        before
    );
    let source_count: i64 = catalog
        .db
        .query_row("SELECT count(*) FROM sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_count, 2);
}

#[test]
fn external_sql_objects_are_rejected_without_executing_them() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::create(directory.path(), "synthetic", 0).unwrap();
    catalog
        .db
        .execute_batch("CREATE VIEW injected AS SELECT 1")
        .unwrap();
    assert!(catalog::verify_schema(&catalog.db).is_err());
}

#[test]
fn decimals_do_not_round_or_accept_noncanonical_values() {
    assert_eq!(decimal("9007199254740993").unwrap(), 9_007_199_254_740_993);
    for invalid in ["-1", "01", "1.0", "1e4", "", "9223372036854775808"] {
        assert!(decimal(invalid).is_err());
    }
}

/// Rebuild a synthetic archive with a catalog mutation and an updated catalog hash. These tests
/// exercise semantic/range verification, not just the outer SHA mismatch check.
fn mutate_catalog(source: &std::path::Path, output: &std::path::Path, sql: &str) {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("mutated.sqlite");
    let mut archive = zip::ZipArchive::new(File::open(source).unwrap()).unwrap();
    let mut parts = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        parts.push((name, bytes));
    }
    let index = parts
        .iter()
        .position(|(name, _)| name == "archive.sqlite")
        .unwrap();
    fs::write(&db_path, &parts[index].1).unwrap();
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(sql).unwrap();
    db.close().unwrap();
    parts[index].1 = fs::read(&db_path).unwrap();
    let hash = hex::encode(Sha256::digest(&parts[index].1));
    let size = parts[index].1.len().to_string();
    let manifest_index = parts
        .iter()
        .position(|(name, _)| name == "manifest.json")
        .unwrap();
    let mut manifest: Manifest = serde_json::from_slice(&parts[manifest_index].1).unwrap();
    manifest.catalog_sha256 = hash;
    manifest.catalog_bytes = size;
    parts[manifest_index].1 = serde_json::to_vec(&manifest).unwrap();
    let mut zip = zip::ZipWriter::new(File::create(output).unwrap());
    for (name, bytes) in parts {
        zip.start_file(
            name,
            zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .large_file(true),
        )
        .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn correctly_hashed_catalog_still_rejects_invalid_ranges_references_and_sql() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.risunest");
    fixture(directory.path())
        .write_candidate(&source, false, &Never)
        .unwrap();
    for (index, sql) in [
        "UPDATE objects SET offset=-1 WHERE byte_length>0",
        "UPDATE objects SET offset=9223372036854775807 WHERE byte_length>0",
        "UPDATE objects SET byte_length=byte_length+1 WHERE byte_length>0",
        "UPDATE objects SET pack_id=17 WHERE byte_length>0",
        "UPDATE objects SET offset=1 WHERE byte_length=0",
        "UPDATE files SET object_hash=zeroblob(32) WHERE logical_key='empty'",
        "UPDATE files SET expected_hash='invalid' WHERE logical_key='empty'",
        "CREATE TRIGGER injected AFTER INSERT ON root BEGIN DELETE FROM root; END",
        "ALTER TABLE root ADD COLUMN unexpected TEXT",
    ]
    .iter()
    .enumerate()
    {
        let path = directory.path().join(format!("mutated-{index}.risunest"));
        mutate_catalog(&source, &path, sql);
        assert!(
            VerifiedArchive::open(File::open(path).unwrap(), directory.path(), &Never).is_err(),
            "mutation {index} was accepted"
        );
    }
}

#[test]
fn duplicate_and_unexpected_zip_names_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.risunest");
    fixture(directory.path())
        .write_candidate(&source, false, &Never)
        .unwrap();
    for (index, name) in ["format.json", "../outside", "unknown.txt"]
        .iter()
        .enumerate()
    {
        let destination = directory.path().join(format!("extra-{index}.risunest"));
        fs::copy(&source, &destination).unwrap();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&destination)
            .unwrap();
        let mut writer = zip::ZipWriter::new_append(file).unwrap();
        writer
            .start_file(*name, zip::write::FileOptions::default())
            .unwrap();
        writer.write_all(b"{}").unwrap();
        writer.finish().unwrap();
        assert!(
            VerifiedArchive::open(File::open(destination).unwrap(), directory.path(), &Never)
                .is_err()
        );
    }
}

#[test]
fn candidates_never_overwrite_an_existing_destination() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("existing");
    fs::write(&path, b"existing destination").unwrap();
    assert!(fixture(directory.path())
        .write_candidate(&path, false, &Never)
        .is_err());
    assert_eq!(fs::read(path).unwrap(), b"existing destination");
}

#[test]
fn disk_full_during_each_archive_region_cannot_produce_a_valid_backup() {
    struct DiskFull {
        file: File,
        remaining: u64,
    }
    impl Write for DiskFull {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "synthetic disk full",
                ));
            }
            let size = bytes.len().min(self.remaining as usize);
            let written = self.file.write(&bytes[..size])?;
            self.remaining -= written as u64;
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }
    impl Seek for DiskFull {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.file.seek(from)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let reference = directory.path().join("reference.risunest");
    fixture(directory.path())
        .write_candidate(&reference, false, &Never)
        .unwrap();
    let size = fs::metadata(reference).unwrap().len();
    for budget in [32, size / 2, size - 64] {
        let path = directory.path().join(format!("failed-{budget}.part"));
        let mut output = DiskFull {
            file: File::create(&path).unwrap(),
            remaining: budget,
        };
        assert!(fixture(directory.path())
            .write_archive(&mut output, false, &Never)
            .is_err());
        drop(output);
        assert!(
            VerifiedArchive::open(File::open(path).unwrap(), directory.path(), &Never).is_err()
        );
    }
}

#[test]
fn cancellation_during_archive_construction_cannot_produce_a_valid_backup() {
    struct Cancel(std::cell::Cell<usize>);
    impl CancellationProbe for Cancel {
        fn is_cancelled(&self) -> bool {
            let calls = self.0.get() + 1;
            self.0.set(calls);
            calls > 3
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancelled.part");
    assert!(matches!(
        fixture(directory.path()).write_candidate(&path, false, &Cancel(std::cell::Cell::new(0))),
        Err(Error::Cancelled)
    ));
    assert!(VerifiedArchive::open(File::open(path).unwrap(), directory.path(), &Never).is_err());
}
#[test]
fn restore_metadata_reader_observes_real_ranges_without_opening_asset_bodies() {
    let directory=tempfile::tempdir().unwrap();
    let path=directory.path().join("observed.risunest");
    fixture(directory.path()).write_candidate(&path,false,&Never).unwrap();
    source_io::reset_source_io();
    let archive=VerifiedArchive::open_for_restore(File::open(&path).unwrap(),directory.path(),&Never).unwrap();
    let work=source_io::take_source_io();
    assert!(work.complete());
    assert!(work.objects.is_empty());
    assert!(work.catalog_hashed_bytes>0);
    assert!(!work.metadata_ranges.is_empty());
    assert!(!work.declared_ranges.is_empty());
    for metadata in &work.metadata_ranges {
        for body in work.declared_ranges.values() {
            assert!(metadata.bytes==0 || body.bytes==0 || metadata.offset+metadata.bytes<=body.offset || body.offset+body.bytes<=metadata.offset,"metadata reader consumed object bytes");
        }
    }
    source_io::reset_source_io();
    let hash=hex::encode(Sha256::digest(b"synthetic file bytes"));
    let (mut body,size)=archive.open_object(&hash).unwrap();
    let mut bytes=Vec::new();
    body.read_to_end(&mut bytes).unwrap();
    drop(body);
    assert_eq!(bytes,b"synthetic file bytes");
    let work=source_io::take_source_io();
    assert!(work.complete());
    let row=&work.objects[&hash];
    assert_eq!(row.opens,1);
    assert_eq!(row.bytes,size);
    assert_eq!(row.hashed_bytes,size);
    assert_eq!(row.hash_checks,1);
    assert!(row.ranges.iter().any(|range|range.bytes==size));
}

#[test]
fn portable_source_observer_marks_partial_reads_incomplete_and_attaches_worker_scope() {
    let directory=tempfile::tempdir().unwrap();
    let path=directory.path().join("partial.risunest");
    fixture(directory.path()).write_candidate(&path,false,&Never).unwrap();
    source_io::reset_source_io();
    let scope=source_io::capture_scope();
    let hash=hex::encode(Sha256::digest(b"synthetic file bytes"));
    let observed=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls=observed.clone();
    let expected=hash.clone();
    source_io::on_object_read(move |hash| {assert_eq!(hash,expected);calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);});
    std::thread::spawn(move|| {
        let _attachment=source_io::attach(&scope);
        let archive=VerifiedArchive::open_for_restore(File::open(path).unwrap(),directory.path(),&Never).unwrap();
        let (mut body,_)=archive.open_object(&hash).unwrap();
        body.read_exact(&mut [0;3]).unwrap();
    }).join().unwrap();
    let work=source_io::take_source_io();
    assert!(!work.complete());
    assert_eq!(work.workers.len(),1);
    let row=work.objects.values().next().unwrap();
    assert_eq!(row.bytes,3);
    assert_eq!(row.hashed_bytes,3);
    assert_eq!(row.incomplete,1);
    assert_eq!(row.outstanding,0);
    assert_eq!(row.hash_checks,0);
    assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst),1);
}
