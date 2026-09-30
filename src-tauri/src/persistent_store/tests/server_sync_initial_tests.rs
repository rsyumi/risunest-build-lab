use super::*;

#[test]
fn empty_replicas_seed_and_receive_while_independent_libraries_require_comparison() {
    let server_dir = tempfile::tempdir().unwrap();
    let server = Arc::new(Store::init(server_dir.path()).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let serving = server.clone();
    let task = runtime.spawn(async move {
        axum::serve(listener, http::router(serving)).await.unwrap();
    });
    let bind = |store: &mut PersistentStore| {
        let device = server.add_device().unwrap();
        store
            .server_bind(&ServerConfig {
                directory: None,
                endpoint: endpoint.clone(),
                library_id: device.library_id,
                device_id: device.device_id,
                token: device.token,
            })
            .unwrap();
    };
    let empty_dir = tempfile::tempdir().unwrap();
    let mut empty = PersistentStore::open(empty_dir.path()).unwrap();
    bind(&mut empty);
    assert_eq!(settle(&mut empty).phase, "idle");
    assert_eq!(server.head().unwrap().seq, 0.into());
    let (_source_dir, mut source) = prepared();
    bind(&mut source);
    assert_eq!(settle(&mut source).phase, "idle");
    let seeded_head = server.head().unwrap();
    assert_ne!(seeded_head.seq, 0.into());
    assert_eq!(settle(&mut empty).phase, "idle");
    assert_eq!(
        empty.server_status().unwrap().head,
        Some(seeded_head.clone())
    );
    let generation = active_generation(&empty.connection).unwrap();
    assert_eq!(
        empty
            .connection
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM characters WHERE generation=?1",
                [&generation],
                |r| r.get(0)
            )
            .unwrap(),
        fixture()["characters"].as_array().unwrap().len() as i64
    );
    assert_eq!(
        empty.read_root(None).unwrap().value,
        source.read_root(None).unwrap().value
    );
    drop(empty);
    let mut empty = PersistentStore::open(empty_dir.path()).unwrap();
    assert_eq!(settle(&mut empty).phase, "idle");
    assert_eq!(server.head().unwrap(), seeded_head);

    let initialized_dir = tempfile::tempdir().unwrap();
    let mut initialized = PersistentStore::open(initialized_dir.path()).unwrap();
    let staging = initialized.replace_begin().unwrap();
    initialized.replace_put_root(&staging.staging_id, &source.read_root(None).unwrap().value).unwrap();
    initialized.replace_put_presets(&staging.staging_id, fixture()["botPresets"].as_array().unwrap()).unwrap();
    initialized.replace_commit(&staging.staging_id, Some(0)).unwrap();
    bind(&mut initialized);
    initialized.asset_residency_set_policy(crate::server_sync::residency::AssetPolicy::Remote, || Ok(())).unwrap();
    let preview = settle(&mut initialized);
    assert_eq!(preview.phase, "conflict");
    let counter = Arc::new(super::super::super::server_sync_engine::CycleItemCounter::default());
    let super::super::super::server_sync_engine::Preparation::Ready(mut ready) = initialized.server_prepare_cycle(&CycleOptions {
        resolution: Some(super::super::super::server_sync_engine::Resolution::KeepRemote),
        expected_revision: Some(preview.local_revision),
        expected_head: Some(preview.head),
        cycle_items: Some(counter.clone()),
        ..Default::default()
    }).unwrap() else { panic!("the remote library must be ready to apply") };
    assert_eq!(counter.activity.load(std::sync::atomic::Ordering::Relaxed), 3);
    let downloaded = counter.processed.load(std::sync::atomic::Ordering::Relaxed);
    assert!(downloaded > 0);
    assert_eq!(counter.expected.load(std::sync::atomic::Ordering::Relaxed), downloaded);
    initialized.server_activate_cycle(&mut ready).unwrap();
    let received = initialized.server_publish_cycle(&ready).unwrap();
    assert_eq!(received.phase, "idle");
    assert_eq!(received.applied_records as u64, downloaded);
    let mut expected = source.materialize(None).unwrap()["characters"].clone();
    for character in expected.as_array_mut().unwrap() {
        // Sync excludes the device's selected chat and recent activity.
        let character = character.as_object_mut().unwrap();
        character.remove("chatPage");
        character.remove("lastInteraction");
    }
    assert_eq!(initialized.materialize(None).unwrap()["characters"], expected);
    assert_eq!(server.head().unwrap(), seeded_head);
    initialized.asset_residency_set_policy(crate::server_sync::residency::AssetPolicy::Remote, || Ok(())).unwrap();

    // A new install holds its defaults and one preset of its own; the server's
    // library replaces them without a comparison or a backup.
    let fresh_dir = tempfile::tempdir().unwrap();
    let mut fresh = PersistentStore::open(fresh_dir.path()).unwrap();
    let mut fresh_root = source.read_root(None).unwrap().value;
    fresh_root["username"] = json!("synthetic new install");
    let mut own_preset = fixture()["botPresets"][0].clone();
    own_preset["name"] = json!("Synthetic default preset");
    let staging = fresh.replace_begin().unwrap();
    fresh.replace_put_root(&staging.staging_id, &fresh_root).unwrap();
    fresh.replace_put_presets(&staging.staging_id, &[own_preset]).unwrap();
    fresh.replace_commit(&staging.staging_id, Some(0)).unwrap();
    assert!(fresh.revision().unwrap() > 0);
    bind(&mut fresh);
    fresh.asset_residency_set_policy(crate::server_sync::residency::AssetPolicy::Remote, || Ok(())).unwrap();
    assert_eq!(settle(&mut fresh).phase, "idle");
    assert_eq!(server.head().unwrap(), seeded_head);
    assert_eq!(fresh.server_status().unwrap().head, Some(seeded_head.clone()));
    assert_eq!(fresh.read_root(None).unwrap().value, source.read_root(None).unwrap().value);
    let materialized = fresh.materialize(None).unwrap();
    assert_eq!(materialized["botPresets"], source.materialize(None).unwrap()["botPresets"]);
    assert_eq!(materialized["characters"], expected);
    let backups = fresh_dir.path().join("server-sync/backups");
    assert!(!backups.exists() || std::fs::read_dir(&backups).unwrap().next().is_none());

    let (_independent_dir, mut independent) = prepared();
    let mut root = independent.read_root(None).unwrap().value;
    root["username"] = json!("synthetic independent library");
    independent
        .commit(&WorkingSetCommit {
            root: Some(root.clone()),
            ..empty_working_set_commit(independent.revision().unwrap())
        })
        .unwrap();
    bind(&mut independent);
    let revision = independent.revision().unwrap();
    assert_eq!(settle(&mut independent).phase, "conflict");
    assert_eq!(independent.revision().unwrap(), revision);
    assert_eq!(independent.read_root(None).unwrap().value, root);
    assert!(independent.server_status().unwrap().head.is_none());
    assert_eq!(server.head().unwrap(), seeded_head);
    task.abort();
    runtime.shutdown_timeout(Duration::from_secs(2));
}
