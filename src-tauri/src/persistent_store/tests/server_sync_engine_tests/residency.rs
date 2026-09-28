use super::*;
use crate::asset_repository::PayloadCas;
use crate::server_sync::residency::{open_or_hydrate, AssetPolicy, Residency};

mod media_admission;
mod regressions;
mod references;

struct Fixture {
    _server_root: tempfile::TempDir,
    server: Arc<Store>,
    runtime: tokio::runtime::Runtime,
    task: tokio::task::JoinHandle<()>,
    endpoint: String,
}
impl Fixture {
    fn new() -> Self {
        Self::measured(None)
    }
    fn measured(first_upload: Option<Arc<std::sync::Mutex<Option<std::time::Instant>>>>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let server = Arc::new(Store::init(root.path()).unwrap());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let remote = server.clone();
        let task = runtime.spawn(async move {
            let router = http::router(remote).layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
                let first_upload = first_upload.clone();
                async move {
                    if request.uri().path().starts_with("/uploads") {
                        if let Some(first_upload) = first_upload { first_upload.lock().unwrap().get_or_insert_with(std::time::Instant::now); }
                    }
                    next.run(request).await
                }
            }));
            axum::serve(listener, router).await.unwrap()
        });
        Self {
            _server_root: root,
            server,
            runtime,
            task,
            endpoint,
        }
    }
    fn bind(&self, store: &mut PersistentStore) {
        let device = self.server.add_device().unwrap();
        store
            .server_bind(&ServerConfig {
                directory: None,
                endpoint: self.endpoint.clone(),
                library_id: device.library_id,
                device_id: device.device_id,
                token: device.token,
            })
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn commit_owner(
    store: &mut PersistentStore,
    entries: &[crate::asset_repository::owner_manifest_codec::OwnerManifestEntry],
) -> String {
    let generation = active_generation(&store.connection).unwrap();
    let raw: String = store
        .connection
        .query_row(
            "SELECT detail FROM characters WHERE generation=?1 AND character_id='char-a'",
            [generation],
            |r| r.get(0),
        )
        .unwrap();
    let mut detail: Value = serde_json::from_str(&raw).unwrap();
    detail["additionalAssets"] = json!(entries
        .iter()
        .map(|entry| entry.tuple.clone())
        .collect::<Vec<_>>());
    let manifest = PayloadCas::new(store.repository_root())
        .unwrap()
        .prepare_bytes(
            &crate::asset_repository::owner_manifest_codec::encode_owner_manifest(entries).unwrap(),
        )
        .unwrap();
    store
        .commit(&WorkingSetCommit {
            character_details: Some(vec![detail]),
            asset_owner_heads: Some(vec![AssetOwnerHead::present(
                AssetOwnerLocator::CharacterAdditionalAssets {
                    character_id: "char-a".into(),
                },
                manifest.content_hash.clone(),
                entries.len() as i64,
            )]),
            ..empty_working_set_commit(store.revision().unwrap())
        })
        .unwrap();
    manifest.content_hash
}
fn put(store: &mut PersistentStore, key: &str, bytes: &[u8]) -> AssetAlias {
    let object = PayloadCas::new(store.repository_root())
        .unwrap()
        .prepare_bytes(bytes)
        .unwrap();
    store
        .asset_object_catalog()
        .register(
            &[
                crate::persistent_store::asset_object_catalog::AssetObjectRegistration {
                    object_hash: object.content_hash.clone(),
                    byte_size: object.byte_size,
                },
            ],
            1,
        )
        .unwrap();
    let alias = AssetAlias {
        key: key.into(),
        object_hash: Some(object.content_hash),
        kind: "asset".into(),
        size: bytes.len() as i64,
        mime: "image/png".into(),
        name: "synthetic".into(),
        ext: "png".into(),
        inlay_type: None,
        width: None,
        height: None,
        metadata: json!({}),
    };
    store
        .commit_asset_alias(&alias, store.revision().unwrap())
        .unwrap();
    alias
}

#[test]
fn remote_replica_skips_body_displays_directly_hydrates_bytes_and_exports_complete_backup() {
    let fixture = Fixture::new();
    let (_first_root, mut first) = prepared();
    let (_second_root, mut second) = prepared();
    fixture.bind(&mut first);
    fixture.bind(&mut second);
    assert_eq!(settle(&mut first).phase, "idle");
    assert_eq!(settle(&mut second).phase, "idle");
    second
        .asset_residency_set_policy(AssetPolicy::Remote, || Ok(()))
        .unwrap();
    let bytes = vec![42; 128 * 1024 + 17];
    let alias = put(&mut first, "assets/synthetic-remote.png", &bytes);
    let hash = alias.object_hash.as_ref().unwrap();
    assert_eq!(settle(&mut first).phase, "idle");
    assert_eq!(settle(&mut second).phase, "idle");
    let cas = PayloadCas::new(second.repository_root()).unwrap();
    assert_eq!(
        cas.stat_object(hash).unwrap(),
        None,
        "remote sync must not download the opaque body"
    );
    assert!(second
        .asset_residency_status()
        .unwrap()
        .has_remote_or_missing());
    assert!(
        second.asset_gc_dry_run(512, None, 10000, 0).is_ok(),
        "GC must recognize remotely resident roots"
    );
    assert_eq!(
        second.server_unbind().unwrap_err().code,
        "download-all-assets-before-unbind"
    );
    let provider = crate::server_sync::media::MediaProvider::new(
        second.repository_root().to_path_buf(),
        "http://127.0.0.1:12345".into(),
    )
    .unwrap();
    let object = risunest_sync_connect::media::MediaObject {
        hash: hash.clone(),
        mime: "image/png".into(),
        size: (bytes.len() as u64).into(),
    };
    let url = provider.url(&object, false).unwrap();
    assert!(url.starts_with(&fixture.endpoint));
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .header("range", "bytes=7-21")
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 206);
    assert_eq!(response.bytes().unwrap().as_ref(), &bytes[7..22]);
    assert_eq!(
        cas.stat_object(hash).unwrap(),
        None,
        "display must stay remote"
    );
    let native =
        crate::native_media::streaming::MediaServer::start(second.repository_root().to_path_buf())
            .unwrap();
    let key = crate::asset_repository::object_physical_key(hash);
    let display = format!(
        "{}{}?mime=image%2Fpng&size={}",
        native.test_base_url(),
        hex::encode(key),
        bytes.len()
    );
    let browser = reqwest::blocking::Client::new();
    let response = browser
        .get(&display)
        .header("range", "bytes=3-12")
        .send()
        .unwrap();
    assert!(response.url().as_str().starts_with(&fixture.endpoint));
    assert_eq!(response.status().as_u16(), 206);
    assert_eq!(response.bytes().unwrap().as_ref(), &bytes[3..13]);
    assert_eq!(
        cas.stat_object(hash).unwrap(),
        None,
        "native resolver must redirect without a file download"
    );
    use std::io::Read;
    let mut actual = Vec::new();
    open_or_hydrate(second.repository_root(), hash)
        .unwrap()
        .unwrap()
        .read_to_end(&mut actual)
        .unwrap();
    assert_eq!(actual, bytes);
    assert_eq!(
        second
            .asset_residency_evict(|| Ok(()))
            .unwrap()
            .evicted_bytes,
        bytes.len() as u64
    );
    assert_eq!(cas.stat_object(hash).unwrap(), None);
    // Alias metadata edits do not require fetching and re-uploading its body.
    let mut edited = alias.clone();
    edited.name = "renamed synthetic".into();
    second
        .commit_asset_alias(&edited, second.revision().unwrap())
        .unwrap();
    assert_eq!(settle(&mut second).phase, "idle");
    assert_eq!(cas.stat_object(hash).unwrap(), None);
    let backup = tempfile::tempdir().unwrap();
    let staging = backup.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let revision = second.revision().unwrap();
    crate::portable_backup::create_verified_library_backup(
        &mut second,
        revision,
        &backup.path().join("synthetic.risunest"),
        &staging,
        &crate::local_backup::NeverCancelled,
    )
    .unwrap();
    assert_eq!(cas.read_object(hash).unwrap().unwrap(), bytes);
    second.asset_residency_evict(|| Ok(())).unwrap();
    second
        .asset_residency_set_policy(AssetPolicy::Full, || Ok(()))
        .unwrap();
    assert!(!second
        .asset_residency_status()
        .unwrap()
        .has_remote_or_missing());
    assert_eq!(
        first
            .device_store()
            .unwrap()
            .asset_residency_policy()
            .unwrap(),
        AssetPolicy::Full
    );
}

#[test]
fn snapshot_only_asset_is_uploaded_and_custody_released_only_after_snapshot_deletion() {
    let fixture = Fixture::new();
    let (_root, mut store) = prepared();
    fixture.bind(&mut store);
    assert_eq!(settle(&mut store).phase, "idle");
    let alias = put(
        &mut store,
        "assets/snapshot-only.png",
        b"synthetic historical bytes",
    );
    let hash = alias.object_hash.as_ref().unwrap();
    let snapshot = store.snapshot_create("synthetic-retention").unwrap();
    store
        .delete_asset_alias("asset", &alias.key, store.revision().unwrap())
        .unwrap();
    store
        .asset_residency_set_policy(AssetPolicy::Remote, || Ok(()))
        .unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    assert_eq!(cas.stat_object(hash).unwrap(), None);
    let residency = Residency::open(store.repository_root()).unwrap();
    assert!(residency.object(hash, None).unwrap().is_some());
    store.asset_residency_release_unused(|| Ok(())).unwrap();
    assert!(residency.object(hash, None).unwrap().is_some());
    store.snapshot_restore_request(&snapshot.id).unwrap();
    drop(store);
    let mut store = PersistentStore::open(_root.path()).unwrap();
    assert!(store
        .read_asset_alias("asset", &alias.key, None)
        .unwrap()
        .is_some());
    assert_eq!(
        cas.stat_object(hash).unwrap(),
        None,
        "snapshot restore keeps media remote"
    );
    assert_eq!(
        std::io::read_to_string(
            open_or_hydrate(store.repository_root(), hash)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        "synthetic historical bytes"
    );
    store
        .delete_asset_alias("asset", &alias.key, store.revision().unwrap())
        .unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    store.snapshot_delete(&snapshot.id).unwrap();
    store.asset_residency_release_unused(|| Ok(())).unwrap();
    assert!(residency.object(hash, None).unwrap().is_none());
    let config = store.server_config().unwrap().unwrap();
    let device = fixture
        .server
        .authenticate(&config.library_id, &config.token)
        .unwrap();
    assert!(fixture
        .server
        .retained_objects(&device, &fixture.server.head().unwrap().epoch, None)
        .unwrap()
        .objects
        .iter()
        .all(|object| object.hash != *hash));
}

#[test]
#[ignore = "manual synthetic Android WebView verification server"]
fn android_media_fixture_server() {
    let output = std::path::PathBuf::from(std::env::var("RISUNEST_MEDIA_FIXTURE_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let fixture = Fixture::new();
    let (_root, mut store) = prepared();
    fixture.bind(&mut store);
    let size = 1024u32;
    let pixels = size * size * 4;
    let mut bmp = Vec::new();
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(54 + pixels).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&size.to_le_bytes());
    bmp.extend_from_slice(&size.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    for _ in 0..size * size {
        bmp.extend_from_slice(&[20, 80, 160, 255]);
    }
    let mut alias = put(&mut store, "assets/android-media.bmp", &bmp);
    alias.mime = "image/bmp".into();
    alias.ext = "bmp".into();
    store
        .commit_asset_alias(&alias, store.revision().unwrap())
        .unwrap();
    assert_eq!(settle(&mut store).phase, "idle");
    let device = fixture.server.add_device().unwrap();
    std::fs::write(output.join("fixture.json"), serde_json::to_vec(&json!({
        "config": {"endpoint":fixture.endpoint,"libraryId":device.library_id,"deviceId":device.device_id,"token":device.token},
        "alias": alias
    })).unwrap()).unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1200);
    while std::time::Instant::now() < until && !output.join("stop").exists() {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[test]
fn replacement_registration_reads_and_releases_historical_custody() {
    let fixture = Fixture::new();
    let (_root, mut store) = prepared();
    fixture.bind(&mut store);
    let alias = put(
        &mut store,
        "assets/recovery.png",
        b"synthetic recovery bytes",
    );
    store
        .asset_residency_set_policy(AssetPolicy::Remote, || Ok(()))
        .unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let old = store.server_config().unwrap().unwrap();
    let media = crate::server_sync::media::MediaProvider::new(
        store.repository_root().to_owned(),
        "http://127.0.0.1:8123".into(),
    )
    .unwrap();
    let object = risunest_sync_connect::media::MediaObject {
        hash: alias.object_hash.clone().unwrap(),
        size: (alias.size as u64).into(),
        mime: alias.mime.clone(),
    };
    let old_url = media.url(&object, false).unwrap();
    fixture.server.revoke_device(&old.device_id).unwrap();
    let next = fixture.server.add_device().unwrap();
    store
        .server_replace_registration(
            &ServerConfig {
                directory: None,
                endpoint: fixture.endpoint.clone(),
                library_id: next.library_id,
                device_id: next.device_id,
                token: next.token,
            },
            store.revision().unwrap(),
        )
        .unwrap();
    let refreshed_url = media.url(&object, false).unwrap();
    assert_ne!(old_url, refreshed_url);
    assert_eq!(
        reqwest::blocking::get(refreshed_url)
            .unwrap()
            .status()
            .as_u16(),
        200
    );
    let hash = alias.object_hash.unwrap();
    assert_eq!(
        std::io::read_to_string(
            open_or_hydrate(store.repository_root(), &hash)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        "synthetic recovery bytes"
    );
    store
        .delete_asset_alias("asset", &alias.key, store.revision().unwrap())
        .unwrap();
    for snapshot in store.snapshot_list().unwrap() {
        store.snapshot_delete(&snapshot.id).unwrap();
    }
    store.asset_residency_release_unused(|| Ok(())).unwrap();
    assert!(Residency::open(store.repository_root())
        .unwrap()
        .object(&hash, None)
        .unwrap()
        .is_none());
}

#[test]
fn simultaneous_remote_media_grants_do_not_exhaust_device_request_slots() {
    let fixture = Fixture::new();
    let (_root, mut store) = prepared();
    fixture.bind(&mut store);
    let alias = put(&mut store, "assets/burst.png", b"synthetic burst");
    store
        .asset_residency_set_policy(AssetPolicy::Remote, || Ok(()))
        .unwrap();
    store.asset_residency_evict(|| Ok(())).unwrap();
    let provider = Arc::new(
        crate::server_sync::media::MediaProvider::new(
            store.repository_root().to_owned(),
            "http://127.0.0.1:8123".into(),
        )
        .unwrap(),
    );
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let tasks = (0..8)
        .map(|i| {
            let provider = provider.clone();
            let barrier = barrier.clone();
            let object = risunest_sync_connect::media::MediaObject {
                hash: alias.object_hash.clone().unwrap(),
                size: (alias.size as u64).into(),
                mime: format!("image/png; variant={i}"),
            };
            std::thread::spawn(move || {
                barrier.wait();
                provider.url(&object, false)
            })
        })
        .collect::<Vec<_>>();
    for task in tasks {
        if let Err(error) = task.join().unwrap() {
            panic!(
                "visible media burst must not receive device-busy: code={} status={}",
                error.code, error.status
            );
        }
    }
}

#[test]
fn owner_manifests_stay_local_while_the_owner_binary_stays_remote() {
    use crate::asset_repository::owner_manifest_codec::OwnerManifestEntry;
    let fixture = Fixture::new();
    let (_first, mut first) = prepared();
    let (_second, mut second) = prepared();
    fixture.bind(&mut first);
    fixture.bind(&mut second);
    assert_eq!(settle(&mut first).phase, "idle");
    assert_eq!(settle(&mut second).phase, "idle");
    second
        .asset_residency_set_policy(AssetPolicy::Remote, || Ok(()))
        .unwrap();
    let binary = PayloadCas::new(first.repository_root())
        .unwrap()
        .prepare_bytes(b"synthetic owner-only binary")
        .unwrap();
    let mut entries = vec![OwnerManifestEntry {
        tuple: [
            "synthetic".into(),
            "assets/owner-only.png".into(),
            "png".into(),
        ],
        payload_hash: Some(
            hex::decode(&binary.content_hash)
                .unwrap()
                .try_into()
                .unwrap(),
        ),
    }];
    let manifest = commit_owner(&mut first, &entries);
    assert_eq!(settle(&mut first).phase, "idle");
    assert_eq!(settle(&mut second).phase, "idle");
    let cas = PayloadCas::new(second.repository_root()).unwrap();
    assert!(cas.stat_object(&manifest).unwrap().is_some());
    assert_eq!(cas.stat_object(&binary.content_hash).unwrap(), None);
    // Editing the owning record must not pull all of its media back to disk.
    entries[0].tuple[0] = "edited synthetic".into();
    let edited = commit_owner(&mut second, &entries);
    assert_eq!(settle(&mut second).phase, "idle");
    assert_eq!(cas.stat_object(&binary.content_hash).unwrap(), None);
    second.asset_residency_evict(|| Ok(())).unwrap();
    assert!(cas.stat_object(&edited).unwrap().is_some());
}

#[test]
fn preparation_roots_survive_reopen_and_assets_never_enter_the_derived_cache() {
    let fixture = Fixture::new();
    let (root, mut first) = prepared();
    let bytes = vec![37; 300 * 1024];
    let alias = put(&mut first, "assets/synthetic-preparation.png", &bytes);
    let hash = alias.object_hash.as_ref().unwrap();
    put(&mut first, "assets/synthetic-shared.png", &bytes);
    fixture.bind(&mut first);
    let ready = first.server_prepare_cycle(&CycleOptions::default()).unwrap();
    assert!(matches!(ready, crate::persistent_store::server_sync_engine::Preparation::Ready(_)));
    let cache = first.server_cache().unwrap();
    assert!(cache.cas.stat_object(hash).unwrap().is_none());
    assert!(first.collect_labelled_asset_gc_roots(false, true).unwrap().iter()
        .any(|(label, roots)| *label == "server-sync" && roots.object_hashes.contains(hash)));
    drop(ready);
    drop(first);
    let mut first = PersistentStore::open(root.path()).unwrap();
    assert!(first.connection.query_row("SELECT count(*) FROM server_sync_prepared", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
    assert!(first.collect_labelled_asset_gc_roots(false, true).unwrap().iter()
        .any(|(label, roots)| *label == "server-sync" && roots.object_hashes.contains(hash)));
    assert_eq!(settle(&mut first).phase, "idle");
    // Projection rows outlive publication, so a later retry re-projects only the
    // keys edited since. They remain cache roots and still hold no asset body.
    assert!(first.connection.query_row("SELECT count(*) FROM server_sync_prepared", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
    assert!(first.server_cache().unwrap().cas.stat_object(hash).unwrap().is_none());
    let target = tempfile::tempdir().unwrap();
    let mut second = PersistentStore::open(target.path()).unwrap();
    fixture.bind(&mut second);
    let ready = second.server_prepare_cycle(&CycleOptions::default()).unwrap();
    assert!(matches!(ready, crate::persistent_store::server_sync_engine::Preparation::Ready(_)));
    assert_eq!(PayloadCas::new(target.path()).unwrap().read_object(hash).unwrap().unwrap(), bytes);
    assert!(second.server_cache().unwrap().cas.stat_object(hash).unwrap().is_none());
    let root_count: i64 = second.connection.query_row("SELECT count(*) FROM server_sync_objects WHERE hash=?1", [hash], |r| r.get(0)).unwrap();
    assert_eq!(root_count, 1);
    drop(ready);
    drop(second);
    let mut second = PersistentStore::open(target.path()).unwrap();
    assert_eq!(settle(&mut second).phase, "idle");
}

#[test]
#[ignore = "Explicit synthetic full-cycle release measurement"]
fn preparation_full_cycle_measurement() {
    fn disk(path: &std::path::Path) -> u64 {
        std::fs::read_dir(path).unwrap().map(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() { disk(&entry.path()) } else { entry.metadata().unwrap().len() }
        }).sum()
    }
    for (kind, count, size) in [("assets", 256, 4 * 1024 * 1024), ("small-assets", 1024, 1024), ("text", 64, 16 * 1024 * 1024), ("conversation", 1, 64 * 1024 * 1024)] {
        let first_upload = Arc::new(std::sync::Mutex::new(None));
        let fixture = Fixture::measured(Some(first_upload.clone()));
        let (_root, mut store) = prepared();
        let mut state = 97u64;
        for index in 0..count {
            let bytes = (0..size).map(|_| {
                state ^= state << 13; state ^= state >> 7; state ^= state << 17;
                b'a' + (state % 26) as u8
            }).collect::<Vec<_>>();
            if kind.ends_with("assets") {
                put(&mut store, &format!("assets/synthetic-{index}.bin"), &bytes);
            } else {
                let id = format!("synthetic-{index}");
                store.commit(&WorkingSetCommit {
                    conversations: Some(vec![ConversationMutation::ReplaceRange {
                        character_id: "char-a".into(), conversation_id: id.clone(), start: 0, delete_count: 0,
                        messages: vec![json!({"role":"user","data":String::from_utf8(bytes).unwrap(),"chatId":id})],
                        conversation: Some(json!({"id":id,"name":"synthetic"})), configured_index: None,
                    }]),
                    ..empty_working_set_commit(store.revision().unwrap())
                }).unwrap();
            }
        }
        fixture.bind(&mut store);
        let counter = Arc::new(crate::persistent_store::server_sync_engine::CycleItemCounter::default());
        let options = CycleOptions { cycle_items: Some(counter.clone()), ..Default::default() };
        let started = std::time::Instant::now();
        let crate::persistent_store::server_sync_engine::Preparation::Ready(mut ready) = store.server_prepare_cycle(&options).unwrap() else { panic!("expected preparation"); };
        let preparation = started.elapsed().as_millis();
        store.server_activate_cycle(&mut ready).unwrap();
        let upload_barrier = started.elapsed().as_millis();
        store.server_publish_cycle(&ready).unwrap();
        let operation_count: i64 = store.connection.query_row("SELECT count(*) FROM server_sync_operation_records", [], |r| r.get(0)).unwrap();
        assert_eq!(counter.total.load(AtomicOrdering::Relaxed), operation_count as u64);
        assert_eq!(counter.done.load(AtomicOrdering::Relaxed), operation_count as u64);
        if kind == "small-assets" {
            let pages: i64 = store.connection.query_row("SELECT count(*) FROM server_sync_operation_pages", [], |r| r.get(0)).unwrap();
            assert!(pages > 1);
        }
        assert_eq!(settle(&mut store).phase, "idle");
        let total = started.elapsed().as_millis();
        let cache = store.server_cache().unwrap();
        let closure = store.server_cache_references(&cache).unwrap();
        let derived_bytes: u64 = closure.iter().map(|hash| cache.stat_derived(hash).unwrap().unwrap_or(0)).sum();
        let derived_disk = disk(cache.cas.repository_root());
        let first_upload = first_upload.lock().unwrap().unwrap().duration_since(started).as_millis();
        for hash in &closure { assert_eq!(risunest_sync_wire::hash(&fixture.server.get_object(hash).unwrap()), *hash); }
        let unchanged = std::time::Instant::now();
        assert_eq!(settle(&mut store).phase, "idle");
        eprintln!("kind={kind} count={count} logical_bytes={} prepare_ms={preparation} upload_barrier_ms={upload_barrier} first_upload_ms={first_upload} terminal_ms={total} derived_reachable_bytes={derived_bytes} derived_disk_bytes={derived_disk} reachable_objects={} no_change_ms={}",count * size,closure.len(),unchanged.elapsed().as_millis());
    }
}
