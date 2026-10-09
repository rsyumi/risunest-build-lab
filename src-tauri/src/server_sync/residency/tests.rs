use super::*;

#[test]
fn cancelled_waiter_leaves_the_active_hydration_lock_intact() {
    let lock = std::sync::Mutex::new(());
    let active = lock.lock().unwrap();
    let waiting = std::cell::Cell::new(false);
    let result = lock_with_check(&lock, &|| {
        if waiting.replace(true) {
            Err(SyncError::new("cancelled", 409))
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(error) if error.code == "cancelled"));
    assert!(matches!(
        lock.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    ));
    drop(active);
    assert!(lock_with_check(&lock, &|| Ok(())).is_ok());
}

fn config(device: &str) -> StoredConfig {
    serde_json::from_value(serde_json::json!({
        "endpoint":"http://127.0.0.1:8123/", "libraryId":"library", "deviceId":device,
        "credentialId":"00000000-0000-4000-8000-000000000000"
    }))
    .unwrap()
}
fn head() -> RemoteHead {
    RemoteHead {
        head_id: hash(b"head"),
        ..RemoteHead::genesis("library".into(), "epoch".into()).unwrap()
    }
}
fn object(seed: &[u8]) -> RetainedObject {
    RetainedObject {
        hash: hash(b"payload"),
        size: 7.into(),
        retention_id: hash(seed),
    }
}

#[test]
fn custody_survives_reopen_but_stays_device_local() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut store = Residency::open(root.path()).unwrap();
    store
        .confirm(&config("device"), &head(), &[object(b"first")])
        .unwrap();
    drop(store);
    assert!(root
        .path()
        .join("server-sync/asset-residency.sqlite")
        .is_file());
    assert!(!root.path().join("asset-residency.sqlite").exists());
    let store = Residency::open(root.path()).unwrap();
    let context = Residency::context_id(&config("device"), "epoch");
    assert!(store
        .confirms(&hash(b"payload"), Some(7), &context)
        .unwrap());
    assert!(!store
        .confirms(&hash(b"payload"), Some(8), &context)
        .unwrap());
    assert!(!store.confirms(&hash(b"payload"), Some(7), "other").unwrap());
    let other = Residency::open(other.path()).unwrap();
    assert!(other.object(&hash(b"payload"), None).unwrap().is_none());
}

#[test]
fn the_latest_confirmed_custody_wins_when_an_existing_context_is_reused() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Residency::open(root.path()).unwrap();
    let a = config("device-a");
    let mut b = config("device-b");
    b.library_id = "other-library".into();
    let mut b_head = head();
    b_head.library_id = b.library_id.clone();
    store.confirm(&a, &head(), &[object(b"a-first")]).unwrap();
    let old_a = store.object(&hash(b"payload"), None).unwrap().unwrap();
    assert!(store.begin_release(&old_a).unwrap());
    store.confirm(&b, &b_head, &[object(b"b")]).unwrap();
    let retained_b = store.object(&old_a.hash, None).unwrap().unwrap();
    assert_eq!(retained_b.config.library_id, b.library_id);

    let mut invalid = object(b"invalid-a");
    invalid.retention_id = "invalid".into();
    assert!(store.confirm(&a, &head(), &[invalid]).is_err());
    assert_eq!(store.object(&old_a.hash, None).unwrap().unwrap().context, retained_b.context);

    let mut renewed = serde_json::to_value(&a).unwrap();
    renewed["credentialId"] = serde_json::json!("00000000-0000-4000-8000-000000000001");
    let renewed_a = serde_json::from_value::<StoredConfig>(renewed.clone()).unwrap();
    store.confirm(&renewed_a, &head(), &[object(b"a-renewed")]).unwrap();
    store.finish_release(&old_a).unwrap();
    drop(store);
    let store = Residency::open(root.path()).unwrap();
    let latest = store.object(&old_a.hash, None).unwrap().unwrap();
    assert_eq!(latest.context, old_a.context);
    assert_eq!(latest.retention_id, hash(b"a-renewed"));
    assert_eq!(serde_json::to_value(&latest.config).unwrap()["credentialId"], renewed["credentialId"]);
    assert!(store.object(&old_a.hash, Some(&retained_b.context)).unwrap().is_some());
}

#[test]
fn a_new_device_or_epoch_also_becomes_the_latest_custody() {
    for change_epoch in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Residency::open(root.path()).unwrap();
        let mut registration = config("device");
        let mut remote = head();
        store.confirm(&registration, &remote, &[object(b"first")]).unwrap();
        let original = Residency::context_id(&registration, &remote.epoch);
        if change_epoch { remote.epoch = "new-epoch".into(); }
        else { registration.device_id = "new-device".into(); }
        store.confirm(&registration, &remote, &[object(b"next")]).unwrap();
        let latest = store.object(&hash(b"payload"), None).unwrap().unwrap();
        assert_ne!(latest.context, original);
        assert_eq!(latest.retention_id, hash(b"next"));
        assert!(store.object(&latest.hash, Some(&original)).unwrap().is_some());
    }
}

#[test]
fn target_membership_uses_the_inspected_library_and_epoch_across_retained_routes() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Residency::open(root.path()).unwrap();
    let a = config("device-a");
    let mut b = config("device-b");
    b.library_id = "other-library".into();
    let mut b_head = head();
    b_head.library_id = b.library_id.clone();
    store.confirm(&a, &head(), &[object(b"a")]).unwrap();
    store.confirm(&b, &b_head, &[object(b"b")]).unwrap();
    let digest = hash(b"payload");
    assert_eq!(store.object(&digest, None).unwrap().unwrap().config.library_id, b.library_id);
    assert!(store.target_holds(&digest, &a.library_id, &hash(b"library:epoch")).unwrap());
    assert!(store.target_holds(&digest, &b.library_id, &hash(b"other-library:epoch")).unwrap());
    assert!(!store.target_holds(&digest, &a.library_id, &hash(b"library:new-epoch")).unwrap());
    assert!(!store.target_holds(&digest, &b.library_id, &hash(b"library:epoch")).unwrap());
    let old = store.object(&digest, Some(&Residency::context_id(&a, "epoch"))).unwrap().unwrap();
    store.begin_release(&old).unwrap();
    assert!(!store.target_holds(&digest, &a.library_id, &hash(b"library:epoch")).unwrap());
}

#[test]
fn stale_release_cannot_remove_renewed_custody() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Residency::open(root.path()).unwrap();
    store
        .confirm(&config("device"), &head(), &[object(b"first")])
        .unwrap();
    let old = store.object(&hash(b"payload"), None).unwrap().unwrap();
    assert!(store.begin_release(&old).unwrap());
    assert!(store.object(&old.hash, None).unwrap().is_none());
    store
        .confirm(&config("device"), &head(), &[object(b"second")])
        .unwrap();
    store.finish_release(&old).unwrap();
    assert_eq!(
        store.object(&old.hash, None).unwrap().unwrap().retention_id,
        hash(b"second")
    );
    assert!(!store.begin_release(&old).unwrap());
}

#[test]
fn latest_release_claim_uses_the_newest_id_and_stale_completion_preserves_a_new_retain() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Residency::open(root.path()).unwrap();
    store
        .confirm(&config("device"), &head(), &[object(b"first")])
        .unwrap();
    store
        .confirm(&config("device"), &head(), &[object(b"second")])
        .unwrap();
    let context = Residency::context_id(&config("device"), "epoch");
    let releasing = store
        .begin_latest_release(&hash(b"payload"), &context)
        .unwrap()
        .unwrap();
    assert_eq!(releasing.retention_id, hash(b"second"));

    let mut concurrent = Residency::open(root.path()).unwrap();
    concurrent
        .confirm(&config("device"), &head(), &[object(b"third")])
        .unwrap();
    store.finish_release(&releasing).unwrap();
    assert_eq!(
        store
            .object(&hash(b"payload"), Some(&context))
            .unwrap()
            .unwrap()
            .retention_id,
        hash(b"third")
    );
}

#[test]
fn invalid_confirmation_is_atomic_and_unknown_schema_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Residency::open(root.path()).unwrap();
    let mut invalid = object(b"invalid");
    invalid.size = "999999999999999999999999".to_owned().try_into().unwrap();
    assert!(store
        .confirm(&config("device"), &head(), &[object(b"first"), invalid])
        .is_err());
    assert!(store.page("").unwrap().is_empty());
    let mut wrong = head();
    wrong.library_id = "other".into();
    assert!(store
        .confirm(&config("device"), &wrong, &[object(b"first")])
        .is_err());
    store.db.execute_batch("PRAGMA user_version=2").unwrap();
    drop(store);
    assert!(Residency::open(root.path()).is_err());
}

fn log_path(root: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", Residency::path(root).display()))
}

#[test]
fn an_armed_anchor_keeps_the_log_open_until_released() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    arm_anchor(root.path());
    assert!(!Residency::exists(root.path()));
    drop(Residency::open(other.path()).unwrap());
    assert!(!log_path(other.path()).exists());
    drop(Residency::open(root.path()).unwrap());
    drop(Residency::open(root.path()).unwrap());
    assert!(log_path(root.path()).is_file());
    release_anchor();
    assert!(!log_path(root.path()).exists());
}

#[test]
fn the_store_opens_by_a_plain_drive_path() {
    let root = tempfile::tempdir().unwrap();
    let store = Residency::open(root.path()).unwrap();
    let opened = store.db.path().unwrap();
    assert!(!opened.starts_with(r"\\"), "{opened}");
}

/// Media requests and status reads each open their own connection while
/// custody is confirmed, and the log must still fold back afterwards.
#[test]
fn concurrent_readers_leave_the_log_foldable() {
    let root = tempfile::tempdir().unwrap();
    let mut writer = Residency::open(root.path()).unwrap();
    writer
        .confirm(&config("device"), &head(), &[object(b"first")])
        .unwrap();
    let digest = hash(b"payload");
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let reader = Residency::open(root.path()).unwrap();
                for _ in 0..500 {
                    assert!(reader.object(&digest, None).unwrap().is_some());
                }
            });
        }
        for index in 0..100 {
            let seed = format!("renewed-{index}");
            writer
                .confirm(&config("device"), &head(), &[object(seed.as_bytes())])
                .unwrap();
        }
    });
    let (busy, log, checkpointed): (i64, i64, i64) = writer
        .db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(busy, 0, "log={log} checkpointed={checkpointed}");
    assert_eq!(std::fs::metadata(log_path(root.path())).unwrap().len(), 0);
}

#[test]
fn transient_fixture_rejects_corrupt_body_and_reclaims_its_scratch() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let bytes = b"synthetic-body";
    let corrupt = b"synthetic-bodY";
    assert_eq!(corrupt.len(), bytes.len());
    let digest = hash(bytes);
    test_remote::hold(root.path(), &[(&digest, bytes.len() as u64)]);
    test_remote::serve(root.path(), &digest, corrupt.to_vec());
    let error = open_transient_with_check(root.path(), scratch.path(), &digest, &|| Ok(()))
        .err().expect("a corrupt fixture body must be refused");
    assert_eq!(error.code, "transfer-target-mismatch");
    assert_eq!(test_remote::fetched(root.path()), 1);
    assert!(crate::asset_repository::PayloadCas::new(root.path()).unwrap()
        .stat_object(&digest).unwrap().is_none());
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[test]
fn transient_fixture_rejects_wrong_custody_size_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let bytes = b"synthetic-body";
    let digest = hash(bytes);
    test_remote::hold(root.path(), &[(&digest, bytes.len() as u64 + 1)]);
    test_remote::serve(root.path(), &digest, bytes.to_vec());
    let error = open_transient_with_check(root.path(), scratch.path(), &digest, &|| Ok(()))
        .err().expect("a fixture body with the wrong custody size must be refused");
    assert_eq!(error.code, "hydration-size-mismatch");
    assert_eq!(test_remote::fetched(root.path()), 1);
    assert!(crate::asset_repository::PayloadCas::new(root.path()).unwrap()
        .stat_object(&digest).unwrap().is_none());
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

/// A page of sizes is what `object` finds one digest at a time, and a stored configuration
/// that does not parse fails the page as it fails `object`.
#[test]
fn active_sizes_are_what_object_finds_for_each_digest() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Residency::open(root.path()).unwrap();
    let held = |seed: &[u8], size: u64| RetainedObject {
        hash: hash(seed),
        size: size.into(),
        retention_id: hash(&[seed, b"-retention"].concat()),
    };
    store.confirm(&config("device-a"), &head(), &[held(b"one", 7), held(b"two", 9), held(b"three", 11)]).unwrap();
    store.confirm(&config("device-b"), &head(), &[held(b"two", 13), held(b"one", 17)]).unwrap();
    let three = store.object(&hash(b"three"), None).unwrap().unwrap();
    assert!(store.begin_release(&three).unwrap());
    let later_one = store.object(&hash(b"one"), None).unwrap().unwrap();
    assert_eq!(later_one.size, 17);
    assert!(store.begin_release(&later_one).unwrap());

    let digests = vec![hash(b"one"), hash(b"two"), hash(b"three"), hash(b"absent")];
    let one_at_a_time = digests.iter().map(|digest| store.object(digest, None).unwrap().map(|object| object.size)).collect::<Vec<_>>();
    assert_eq!(one_at_a_time, [Some(7), Some(13), None, None]);
    assert_eq!(store.active_sizes(&digests).unwrap(), one_at_a_time);
    let paged = (0..1100).map(|index| hash(format!("absent-{index}").as_bytes())).chain(digests.iter().cloned()).collect::<Vec<_>>();
    let sizes = store.active_sizes(&paged).unwrap();
    assert!(sizes[..1100].iter().all(Option::is_none));
    assert_eq!(sizes[1100..], one_at_a_time[..]);
    assert_eq!(store.active_sizes(&["invalid".into()]).unwrap_err().code, store.object("invalid", None).err().unwrap().code);

    store.db.execute("UPDATE contexts SET config='not json' WHERE device_id='device-b'", []).unwrap();
    assert_eq!(store.object(&hash(b"two"), None).err().unwrap().code, "invalid-retention-config");
    assert_eq!(store.active_sizes(&[hash(b"one"), hash(b"absent")]).unwrap(), [Some(7), None]);
    for digests in [vec![hash(b"two")], vec![hash(b"one"), hash(b"two")]] {
        assert_eq!(store.active_sizes(&digests).unwrap_err().code, "invalid-retention-config");
    }
}
