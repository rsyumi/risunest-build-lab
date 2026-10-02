use super::*;


#[test]
fn empty_inventory_and_cache_do_not_create_directories() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        cache_usage(root.path(), None, &BTreeSet::new(), None, false)
            .unwrap()
            .total_bytes,
        0
    );
    assert!(!root.path().join("server-sync").exists());
}


#[test]
fn cache_cleanup_keeps_referenced_objects_metadata_and_local_cas() {
    let root = tempfile::tempdir().unwrap();
    let identity = "a".repeat(64);
    let directory = root.path().join("server-sync").join(&identity);
    let open = || super::super::cache::Cache::open(&directory).unwrap();
    let cache = open();
    // Both stores take part: a body above the threshold is a file, and the
    // small ones share the cache's object database.
    let small = |seed: u8| vec![seed; super::super::cache::SMALL_OBJECT_BYTES / 2];
    let large = |seed: u8| vec![seed; super::super::cache::SMALL_OBJECT_BYTES + 1];
    let protected = cache.put(&small(1)).unwrap();
    let protected_file = cache.put(&large(2)).unwrap();
    let unused_file = cache.put(&large(3)).unwrap();
    let unused = (0..32)
        .map(|seed| cache.put(&small(64 + seed)).unwrap())
        .collect::<Vec<_>>();
    fs::write(directory.join("transfers.sqlite"), b"protected metadata").unwrap();
    let live = crate::asset_repository::PayloadCas::new(root.path()).unwrap();
    let local = live.prepare_bytes(b"unsent local source").unwrap();
    let hashes = BTreeSet::from([protected.clone(), protected_file.clone()]);
    let usage = cache_usage(root.path(), Some(&identity), &hashes, None, false).unwrap();
    assert_eq!(
        usage.reclaimable_bytes,
        (unused.len() * small(0).len() + large(0).len()) as u64
    );
    // Disk is files plus the database allocation. The bodies inside that
    // allocation are reported as their own bytes, not as the pages holding them.
    let in_database = ((unused.len() + 1) * small(0).len()) as u64;
    assert!(usage.database_bytes >= in_database);
    assert!(usage.cache_bytes >= usage.database_bytes + (2 * large(0).len()) as u64);
    // Still needed and clearable split the temporary files between them.
    assert_eq!(usage.protected_bytes + usage.reclaimable_bytes, usage.cache_bytes);
    assert_eq!(usage.ledger_bytes, 0);
    assert_eq!(usage.total_bytes, usage.cache_bytes);
    assert!(cache_usage(
        root.path(),
        Some(&identity),
        &hashes,
        Some("resolve-pending-operation-first"),
        true
    )
    .is_err());
    assert!(cache.stat_derived(&unused[0]).unwrap().is_some());
    // A cleanup runs with no cycle holding the cache, so its checkpoint can
    // return the freed pages instead of leaving them in the write-ahead log.
    drop(cache);
    let cleaned = cache_usage(root.path(), Some(&identity), &hashes, None, true).unwrap();
    assert_eq!(cleaned.reclaimable_bytes, 0);
    assert_eq!(cleaned.protected_bytes, cleaned.cache_bytes);
    assert_eq!(cleaned.total_bytes, cleaned.cache_bytes);
    let cache = open();
    for hash in unused.iter().chain([&unused_file]) {
        assert!(cache.stat_derived(hash).unwrap().is_none());
    }
    assert_eq!(cache.read(&protected, 1 << 20).unwrap(), small(1));
    assert_eq!(cache.read(&protected_file, 1 << 20).unwrap(), large(2));
    assert!(cleaned.database_bytes < usage.database_bytes);
    assert!(live.open_object(&local.content_hash).unwrap().is_some());
    assert!(directory.join("transfers.sqlite").exists());
}

#[test]
fn cache_usage_counts_files_beside_the_caches_without_opening_them_as_caches() {
    let root = tempfile::tempdir().unwrap();
    let server = root.path().join("server-sync");
    let identity = "a".repeat(64);
    let cache = super::super::cache::Cache::open(&server.join(&identity)).unwrap();
    cache
        .put(&vec![7; super::super::cache::SMALL_OBJECT_BYTES + 1])
        .unwrap();
    drop(cache);
    let usage = || cache_usage(root.path(), Some(&identity), &BTreeSet::new(), None, false);
    let without = usage().unwrap();
    // The asset residency ledger is a file beside the per-connection caches.
    let ledger = server.join("asset-residency.sqlite");
    fs::write(&ledger, b"ledger").unwrap();
    let with = usage().unwrap();
    assert_eq!(with.total_bytes, without.total_bytes + 6);
    assert_eq!(with.ledger_bytes, 6);
    assert_eq!(with.total_bytes, with.cache_bytes + with.ledger_bytes);
    assert_eq!(with.cache_bytes, without.cache_bytes);
    assert_eq!(with.protected_bytes, without.protected_bytes);
    assert_eq!(with.reclaimable_bytes, without.reclaimable_bytes);
    assert_eq!(with.database_bytes, without.database_bytes);
    let cleaned = cache_usage(root.path(), Some(&identity), &BTreeSet::new(), None, true).unwrap();
    assert_eq!(cleaned.ledger_bytes, 6);
    assert_eq!(fs::read(&ledger).unwrap(), b"ledger");
}

#[test]
fn chunk_cleanup_removes_only_spools_without_resumable_transfer_rows() {
    let root = tempfile::tempdir().unwrap();
    let identity = "a".repeat(64);
    let cache = root.path().join("server-sync").join(&identity);
    super::super::cache::Cache::open(&cache).unwrap();
    let pending = "b".repeat(64);
    let completed = "c".repeat(64);
    for hash in [&pending, &completed] {
        fs::create_dir_all(cache.join("staging").join(hash)).unwrap();
        fs::write(cache.join("staging").join(hash).join("0"), b"synthetic chunk").unwrap();
    }
    let db = rusqlite::Connection::open(cache.join("transfers.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE chunks(target TEXT,part INTEGER,hash TEXT,size INTEGER)").unwrap();
    db.execute("INSERT INTO chunks VALUES(?1,0,?2,15)", rusqlite::params![pending, "d".repeat(64)]).unwrap();
    drop(db);
    cache_usage(root.path(), Some(&identity), &BTreeSet::new(), None, true).unwrap();
    assert!(cache.join("staging").join(&pending).join("0").is_file());
    assert!(!cache.join("staging").join(&completed).join("0").exists());
}


#[cfg(unix)]
fn directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[cfg(windows)]
fn directory_link(target: &Path, link: &Path) {
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ItemType Junction -Path $env:RISUNEST_TEST_LINK -Target $env:RISUNEST_TEST_TARGET -ErrorAction Stop | Out-Null"])
        .env("RISUNEST_TEST_LINK", link)
        .env("RISUNEST_TEST_TARGET", target)
        .status().unwrap();
    assert!(status.success());
}

#[test]
fn cache_management_rejects_a_linked_cache_root_and_keeps_external_files() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let external = super::super::cache::Cache::open(outside.path()).unwrap();
    let hash = external.put(b"synthetic external object").unwrap();
    fs::create_dir(root.path().join("server-sync")).unwrap();
    directory_link(
        outside.path(),
        &root.path().join("server-sync").join("a".repeat(64)),
    );
    for clean in [false, true] {
        assert!(cache_usage(root.path(), None, &BTreeSet::new(), None, clean).is_err());
    }
    assert!(external.stat_derived(&hash).unwrap().is_some());
}
