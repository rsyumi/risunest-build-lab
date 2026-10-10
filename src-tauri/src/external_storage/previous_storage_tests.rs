use super::{
    connection_commands::ConnectedRepository,
    connection_store::{ConnectionStore, StoredConnection},
    contract::*,
    fake,
    lww_engine::ExternalLwwEngine,
    lww_residency,
    lww_tests::{small_asset, CycleFixture},
};
use crate::{
    asset_repository::PayloadCas,
    persistent_store::{
        lww::Header,
        sync_selection::{SwitchBindingRequest, SyncTarget},
        AssetAlias, PersistentStore, WorkingSetCommit,
    },
    server_sync::{
        lww_tests::{drain_publications, LocalServerFixture},
        previous_storage_tests::{bind_server, switchable_server},
        residency::{AssetPolicy, Residency},
    },
};
use risunest_sync_wire::stamp::DecimalU64;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
}

/// The connection the fixture's receiving store would hold for the repository.
pub(crate) fn receiver_connection(f: &CycleFixture) -> ConnectedRepository {
    ConnectedRepository {
        stored: StoredConnection {
            id: "receiver".into(),
            config: ConnectionConfig { provider: "synthetic".into(), profile: None, endpoint: "https://synthetic.invalid".into(),
                account_id: "fixture".into(), location: Default::default(), oauth_profile: None },
            descriptor: f.receiver.descriptor.clone(),
            descriptor_locator: RemoteLocator { connection_identity: f.receiver.repository.connection_identity.clone(), collection: None, object: "descriptor".into() },
            provider_repository_id: f.receiver.repository.repository_id.clone(),
            credential_ref: "credential".into(), root_key_ref: "key".into(), recovery_key_ref: "recovery".into(),
            retention_policy: None, transfer_concurrency: None, capabilities: f.receiver.capabilities.clone(),
            created_at_ms: 1, verified_at_ms: 1, last_sync_at_ms: None, last_backup_at_ms: None,
        },
        provider: f.provider.clone(),
        handle: fake::repository(),
        dependencies: fake::loopback_dependencies(fake::MemoryVault::default(), 1).dependencies,
        root_key: zeroize::Zeroizing::new([7; 32]),
    }
}

/// Binds the store to the fixture's repository and queues its whole state, as
/// the first publication after a switch does.
fn bind_external(store: &mut PersistentStore, engine: &ExternalLwwEngine) -> DecimalU64 {
    let before = store.lww_binding_state().unwrap();
    let target = SyncTarget::External("repository".into());
    let inspection = store.register_lww_binding_inspection(before.target_authority, &target, &engine.repository.connection_identity, &engine.library).unwrap();
    let authority = store.switch_lww_binding(&SwitchBindingRequest {
        header: Header { binding_authority: before.target_authority, request_id: uuid::Uuid::new_v4().to_string() },
        expected_selection_epoch: before.selection_epoch, target, inspection_id: Some(inspection), initial_publication: false,
    }).unwrap().target_authority;
    let mut after = None;
    loop {
        let header = Header { binding_authority: authority, request_id: uuid::Uuid::new_v4().to_string() };
        let page = store.lww_queue_unit_state_page(&header, after.as_ref(), 4096).unwrap();
        if !page.has_more {
            break;
        }
        after = page.after_key;
    }
    authority
}

fn segments(f: &CycleFixture) -> usize {
    f.provider.uploaded_ids().iter().filter(|id| parse_segment_object_id(id).is_ok()).count()
}

#[test]
fn a_previous_server_body_that_cannot_be_fetched_publishes_no_segment_that_references_it() {
    let offline = Arc::new(AtomicBool::new(false));
    let server = switchable_server(offline.clone());
    let mut f = CycleFixture::new();
    let core = server.client(&f.a);
    let missing = small_asset(&mut f.a, "z-previous-only", &[71; 96 * 1024]);
    drain_publications(&core, &mut f.a, &[]).unwrap();
    f.a.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    f.a.asset_residency_evict(|| Ok(())).unwrap();
    assert!(!f.a.external_lww_object_is_local(&missing).unwrap());
    let shared_body = b"synthetic shared local body";
    let shared = small_asset(&mut f.a, "a-shared", shared_body);
    let template = AssetAlias { key: String::new(), object_hash: Some(shared), kind: "asset".into(), size: shared_body.len() as i64,
        mime: "application/octet-stream".into(), name: "synthetic".into(), ext: "bin".into(),
        inlay_type: None, width: None, height: None, metadata: serde_json::json!({}) };
    let fillers = (0..4100).map(|index| AssetAlias { key: format!("a-{index:04}"), ..template.clone() }).collect::<Vec<_>>();
    f.a.commit_with_asset_aliases(&WorkingSetCommit { expected_revision: f.a.revision().unwrap(), ..Default::default() }, &fillers).unwrap();
    let authority = bind_external(&mut f.a, &f.sender);
    let missing_key = risunest_sync_wire::unit::UnitKey::new(&["asset", "z-previous-only"]).unwrap();
    assert!(f.a.lww_read_outbox(authority, 4096).unwrap().entries.iter().all(|entry| entry.key != missing_key));
    offline.store(true, Ordering::SeqCst);
    let error = run(f.sender.publish(&mut f.a, authority, &[], &Cancellation::default())).err().unwrap();
    assert_eq!(error.kind, ErrorKind::PreviousStorageUnavailable);
    assert_eq!(segments(&f), 1, "the segment before the failing one stays published");
    assert!(f.a.lww_read_outbox(authority, 4096).unwrap().entries.iter().any(|entry| entry.key == missing_key));
    run(f.receive_b());
    assert!(f.b.read_asset_alias("asset", "a-0000", None).unwrap().is_some());
    assert!(f.b.read_asset_alias("asset", "z-previous-only", None).unwrap().is_none());
    offline.store(false, Ordering::SeqCst);
    run(f.sender.publish(&mut f.a, authority, &[], &Cancellation::default())).unwrap();
    assert_eq!(segments(&f), 2);
    assert!(f.a.lww_read_outbox(authority, 16).unwrap().entries.is_empty());
    run(f.receive_b());
    assert!(f.b.read_asset_alias("asset", "z-previous-only", None).unwrap().is_some());
    assert!(!f.a.external_lww_object_is_local(&missing).unwrap());
}

#[test]
fn first_publication_to_a_server_copies_bodies_only_an_external_storage_holds() {
    let mut f = CycleFixture::new();
    let body = vec![73; 96 * 1024];
    let hash = small_asset(&mut f.a, "external-only", &body);
    run(f.publish_a());
    run(f.receive_b());
    assert!(!f.b.external_lww_object_is_local(&hash).unwrap());
    let connection = receiver_connection(&f);
    ConnectionStore::open(f.directory_b.path()).unwrap().insert(&connection.stored).unwrap();
    let resolver = lww_residency::install_test_source_connection(f.directory_b.path(), Arc::new(connection)).unwrap();
    let packs = lww_residency::packed_source(f.directory_b.path(), &hash).unwrap().unwrap().packs;
    let server = LocalServerFixture::new();
    let client = bind_server(&mut f.b, &server);
    drain_publications(&client, &mut f.b, &[]).unwrap();
    assert!(PayloadCas::new(f.directory_b.path()).unwrap().stat_object(&hash).unwrap().is_none());
    assert!(packs.iter().all(|pack| f.provider.holds(&pack.header.object_id)), "the external storage keeps its copy");
    let routed = Residency::open(f.directory_b.path()).unwrap().object(&hash, None).unwrap().unwrap();
    assert_eq!(routed.config.library_id, client.client.config().library_id);
    drop(resolver);
    f.b.asset_residency_set_policy(AssetPolicy::Full, || Ok(())).unwrap();
    assert_eq!(PayloadCas::new(f.directory_b.path()).unwrap().read_object(&hash).unwrap().unwrap(), body, "later downloads come from the server");
}

#[test]
fn a_previous_external_body_that_cannot_be_fetched_publishes_no_segment_to_the_next_storage() {
    let mut f = CycleFixture::new();
    let body = vec![79; 96 * 1024];
    let hash = small_asset(&mut f.a, "external-previous", &body);
    run(f.publish_a());
    run(f.receive_b());
    assert!(!f.b.external_lww_object_is_local(&hash).unwrap());
    let next = CycleFixture::new();
    let mut engine = next.sender;
    engine.connection_id = "next".into();
    engine.connection_root = f.directory_b.path().into();
    engine.library = "synthetic-next-library".into();
    let authority = bind_external(&mut f.b, &engine);
    let error = run(engine.publish(&mut f.b, authority, &[], &Cancellation::default())).err().unwrap();
    assert_eq!(error.kind, ErrorKind::PreviousStorageUnavailable);
    assert_eq!(next.provider.uploaded_ids().iter().filter(|id| parse_segment_object_id(id).is_ok()).count(), 0);
    let connection = receiver_connection(&f);
    ConnectionStore::open(f.directory_b.path()).unwrap().insert(&connection.stored).unwrap();
    let _resolver = lww_residency::install_test_source_connection(f.directory_b.path(), Arc::new(connection)).unwrap();
    run(engine.publish(&mut f.b, authority, &[], &Cancellation::default())).unwrap();
    assert_eq!(next.provider.uploaded_ids().iter().filter(|id| parse_segment_object_id(id).is_ok()).count(), 1);
    assert!(f.b.lww_read_outbox(authority, 16).unwrap().entries.is_empty());
    assert!(!f.b.external_lww_object_is_local(&hash).unwrap());
}

/// A store holding one body only on the server and one only in the
/// fixture's repository, under the remote policy.
fn split_holders(f: &mut CycleFixture) -> (LocalServerFixture, String, String) {
    let external = small_asset(&mut f.a, "external-held", &[83; 96 * 1024]);
    run(f.publish_a());
    run(f.receive_b());
    let server = LocalServerFixture::new();
    let core = server.client(&f.b);
    let held = crate::server_sync::lww_tests::put_asset(&mut f.b, "assets/server-held.png", b"synthetic server held body").object_hash.unwrap();
    drain_publications(&core, &mut f.b, &[]).unwrap();
    f.b.asset_residency_set_policy(AssetPolicy::Remote, || Ok(())).unwrap();
    f.b.asset_residency_evict(|| Ok(())).unwrap();
    assert!(!f.b.external_lww_object_is_local(&held).unwrap());
    assert!(!f.b.external_lww_object_is_local(&external).unwrap());
    (server, held, external)
}

fn status(store: &PersistentStore) -> serde_json::Value {
    serde_json::to_value(store.asset_residency_status().unwrap()).unwrap()
}

#[test]
fn status_counts_bodies_by_holder_and_forgets_a_removed_connection() {
    let mut f = CycleFixture::new();
    let (_server, _held, external) = split_holders(&mut f);
    let gone = status(&f.b);
    assert_eq!((gone["serverObjects"].as_u64(), gone["remoteObjects"].as_u64(), gone["unavailableObjects"].as_u64()), (Some(1), Some(1), Some(1)),
        "a body whose connection is gone is unavailable, not remote");
    assert_eq!(gone["serverBytes"].as_u64(), Some(b"synthetic server held body".len() as u64));
    assert_eq!(gone["externalObjects"], serde_json::json!([]));
    ConnectionStore::open(f.directory_b.path()).unwrap().insert(&receiver_connection(&f).stored).unwrap();
    let split = status(&f.b);
    assert_eq!((split["serverObjects"].as_u64(), split["remoteObjects"].as_u64(), split["unavailableObjects"].as_u64()), (Some(1), Some(2), Some(0)));
    assert_eq!(split["externalObjects"], serde_json::json!([{"connectionId":"receiver","objects":1}]));
    f.b.forget_external_connection_bodies("receiver").unwrap();
    assert!(lww_residency::stat(f.directory_b.path(), &external).unwrap().is_none());
    let removed = status(&f.b);
    assert_eq!((removed["serverObjects"].as_u64(), removed["remoteObjects"].as_u64(), removed["unavailableObjects"].as_u64()), (Some(1), Some(1), Some(1)));
    assert_eq!(removed["externalObjects"], serde_json::json!([]));
}

/// The aliases the quick data check finds without a body, and how many findings block.
fn absent_aliases(store: &mut PersistentStore) -> (Vec<String>, u64) {
    let revision = store.revision().unwrap();
    let lease = store.acquire_revision(revision).unwrap().lease;
    let findings = store.data_health_reader(&lease).unwrap().scan(2000, &crate::local_backup::NeverCancelled).unwrap();
    store.release_revision(&lease).unwrap();
    let absent = findings.items.iter()
        .filter(|finding| finding.code == crate::data_health::codes::ALIAS_OBJECT_ABSENT)
        .map(|finding| finding.owner.id.clone())
        .collect();
    (absent, crate::data_health::SeverityCounts::of(&findings.items).blocking)
}

#[test]
fn the_data_check_reports_only_a_body_no_live_storage_holds() {
    let mut f = CycleFixture::new();
    let (_server, _held, _external) = split_holders(&mut f);
    // The connection that holds the external body is not on this device yet.
    let (gone, blocking) = absent_aliases(&mut f.b);
    assert_eq!(gone, ["external-held"], "the server-held body is not reported");
    assert_eq!(blocking, 1);
    ConnectionStore::open(f.directory_b.path()).unwrap().insert(&receiver_connection(&f).stored).unwrap();
    assert_eq!(absent_aliases(&mut f.b), (Vec::new(), 0));
}

/// Two bodies only the fixture's repository holds, the second of them
/// registered as well for a second live connection.
fn shared_holders(f: &mut CycleFixture) -> (ConnectedRepository, String, String) {
    let exclusive = small_asset(&mut f.a, "external-exclusive", &[87; 96 * 1024]);
    let shared = small_asset(&mut f.a, "external-shared", &[89; 96 * 1024]);
    run(f.publish_a());
    run(f.receive_b());
    let root = f.directory_b.path().to_owned();
    let connection = receiver_connection(f);
    let mut second = connection.stored.clone();
    second.id = "second".into();
    second.descriptor_locator.connection_identity = "second-identity".into();
    let mut connections = ConnectionStore::open(&root).unwrap();
    connections.insert(&connection.stored).unwrap();
    connections.insert(&second).unwrap();
    copy_source(&root, &shared, &shared, "second");
    assert!(!f.b.external_lww_object_is_local(&exclusive).unwrap());
    assert!(!f.b.external_lww_object_is_local(&shared).unwrap());
    (connection, exclusive, shared)
}

#[test]
fn binding_membership_includes_shared_target_holders_without_changing_removal_counts() {
    use crate::persistent_store::asset_residency::PreviousStorageTarget;
    let mut f = CycleFixture::new();
    let (connection, exclusive, shared) = shared_holders(&mut f);
    let target = |id: &str| PreviousStorageTarget::External { connection_id: id.into() };
    let remaining = |id: &str| serde_json::to_value(f.b.asset_residency_status_for_target(Some(&target(id))).unwrap()).unwrap()["previousStorageObjects"].as_u64().unwrap();
    assert_eq!(remaining("receiver"), 0);
    assert_eq!(remaining("second"), 1);
    assert_eq!(remaining("other"), 2);
    assert_eq!(status(&f.b)["externalObjects"], serde_json::json!([{"connectionId":"receiver","objects":1}]));
    let _resolver = lww_residency::install_test_source_connection(f.directory_b.path(), Arc::new(connection)).unwrap();
    f.b.asset_residency_download_previous(&target("second"), None, None, || Ok(())).unwrap();
    assert!(f.b.external_lww_object_is_local(&exclusive).unwrap());
    assert!(!f.b.external_lww_object_is_local(&shared).unwrap());
}

#[test]
fn binding_membership_counts_only_nonlocal_files_the_server_target_lacks() {
    let mut f = CycleFixture::new();
    let (server, _held, _external) = split_holders(&mut f);
    ConnectionStore::open(f.directory_b.path()).unwrap().insert(&receiver_connection(&f).stored).unwrap();
    let core = server.client(&f.b);
    let head = core.client.resolve_identity().unwrap();
    let target = crate::persistent_store::asset_residency::PreviousStorageTarget::Server {
        target_id: risunest_sync_wire::hash(format!("{}:{}", head.library_id, head.epoch).as_bytes()), library_id: head.library_id,
    };
    let status = serde_json::to_value(f.b.asset_residency_status_for_target(Some(&target)).unwrap()).unwrap();
    assert_eq!(status["previousStorageObjects"], 1);
    assert_eq!(status["serverObjects"], 1);
}

#[test]
fn a_connection_counts_and_downloads_only_the_bodies_no_other_live_connection_holds() {
    let mut f = CycleFixture::new();
    let (connection, exclusive, shared) = shared_holders(&mut f);
    let counted = status(&f.b);
    assert_eq!((counted["remoteObjects"].as_u64(), counted["unavailableObjects"].as_u64()), (Some(2), Some(0)));
    assert_eq!(counted["externalObjects"], serde_json::json!([{"connectionId":"receiver","objects":1}]),
        "a body two live connections hold counts in neither");
    let _resolver = lww_residency::install_test_source_connection(f.directory_b.path(), Arc::new(connection)).unwrap();
    f.b.asset_residency_download_remote(Some("second"), None, None, || Ok(())).unwrap();
    assert!(!f.b.external_lww_object_is_local(&shared).unwrap());
    let readings = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = readings.clone();
    let progress = tauri::ipc::Channel::new(move |body| { sink.lock().unwrap().push(body.deserialize::<serde_json::Value>().unwrap()); Ok(()) });
    crate::server_sync::asset_download_progress::within(Some(progress), || {
        f.b.asset_residency_download_remote(Some("receiver"), None, None, || Ok(())).unwrap();
    });
    assert_eq!(readings.lock().unwrap().last(), Some(&serde_json::json!({"completedItems":1,"totalItems":1})));
    assert!(f.b.external_lww_object_is_local(&exclusive).unwrap());
    assert!(!f.b.external_lww_object_is_local(&shared).unwrap(), "another connection still holds it");
    assert_eq!(status(&f.b)["externalObjects"], serde_json::json!([]));
    f.b.forget_external_connection_bodies("receiver").unwrap();
    let after = status(&f.b);
    assert_eq!(after["remoteObjects"].as_u64(), Some(1));
    assert_eq!(after["externalObjects"], serde_json::json!([{"connectionId":"second","objects":1}]));
}

#[test]
fn a_status_pass_opens_the_body_registry_once() {
    let mut f = CycleFixture::new();
    let (_connection, _exclusive, _shared) = shared_holders(&mut f);
    let root = f.directory_b.path().to_owned();
    lww_residency::forget_registry_opens(&root);
    status(&f.b);
    assert_eq!(lww_residency::registry_opens(&root), 1);
}

#[test]
fn a_cleanup_preview_opens_the_body_registry_once() {
    let mut f = CycleFixture::new();
    let (_connection, _exclusive, _shared) = shared_holders(&mut f);
    let root = f.directory_b.path().to_owned();
    lww_residency::forget_registry_opens(&root);
    f.b.asset_gc_dry_run(1024, None, super::runtime::now_ms() as i64, 0).unwrap();
    assert_eq!(lww_residency::registry_opens(&root), 1);
}

#[test]
fn download_without_a_policy_change_fetches_one_connection_or_every_holder() {
    let mut f = CycleFixture::new();
    let (_server, held, external) = split_holders(&mut f);
    let connection = receiver_connection(&f);
    ConnectionStore::open(f.directory_b.path()).unwrap().insert(&connection.stored).unwrap();
    let _resolver = lww_residency::install_test_source_connection(f.directory_b.path(), Arc::new(connection)).unwrap();
    f.b.asset_residency_download_remote(Some("receiver"), None, None, || Ok(())).unwrap();
    assert!(f.b.external_lww_object_is_local(&external).unwrap());
    assert!(!f.b.external_lww_object_is_local(&held).unwrap(), "a connection download leaves server-held bodies remote");
    assert_eq!(status(&f.b)["policy"], "remote");
    f.b.asset_residency_download_remote(None, None, None, || Ok(())).unwrap();
    assert!(f.b.external_lww_object_is_local(&held).unwrap());
    assert_eq!(status(&f.b)["policy"], "remote");
    assert_eq!(status(&f.b)["remoteObjects"].as_u64(), Some(0));
}

#[test]
fn download_without_a_policy_change_skips_bodies_no_storage_holds() {
    let mut f = CycleFixture::new();
    let (_server, held, external) = split_holders(&mut f);
    f.b.asset_residency_download_remote(None, None, None, || Ok(())).unwrap();
    assert!(f.b.external_lww_object_is_local(&held).unwrap());
    assert!(!f.b.external_lww_object_is_local(&external).unwrap());
    assert_eq!(status(&f.b)["unavailableObjects"].as_u64(), Some(1));
}

/// Registers the source of `from` again for `hash`, through `connection`.
fn copy_source(root: &std::path::Path, from: &str, hash: &str, connection: &str) {
    let db = rusqlite::Connection::open(root.join("external-sync").join("remote-bodies.sqlite")).unwrap();
    let copied = ["sources", "packed_sources"].iter().map(|table| db.execute(&format!(
        "INSERT INTO {table}(hash,library,source) SELECT ?2,library||?3,json_set(source,'$.hash',?2,'$.connectionId',?3) FROM {table} WHERE hash=?1"),
        rusqlite::params![from, hash, connection]).unwrap()).sum::<usize>();
    assert_eq!(copied, 1);
}

fn catalog_row(root: &std::path::Path, hash: &str, size: u64) {
    let db = rusqlite::Connection::open(root.join("persistent").join(crate::persistent_store::DATABASE_FILE)).unwrap();
    db.execute("INSERT INTO asset_objects(object_hash,byte_size,created_at_ms) VALUES(?1,?2,0)", rusqlite::params![hash, size as i64]).unwrap();
}

#[test]
fn removing_a_connection_forgets_catalog_rows_only_it_held() {
    let mut f = CycleFixture::new();
    let (_server, held, external) = split_holders(&mut f);
    let root = f.directory_b.path().to_owned();
    let connection = receiver_connection(&f);
    ConnectionStore::open(&root).unwrap().insert(&connection.stored).unwrap();
    let _resolver = lww_residency::install_test_source_connection(&root, Arc::new(connection)).unwrap();
    f.b.asset_residency_download_remote(Some("receiver"), None, None, || Ok(())).unwrap();
    let size = f.b.asset_object_byte_size(&external).unwrap().expect("the received body has a catalog row");
    // Rows registered before their bodies arrived, as admission and restore leave them.
    let (orphan, shared) = (format!("{:064x}", 1), format!("{:064x}", 2));
    for hash in [&orphan, &shared] {
        catalog_row(&root, hash, size);
        copy_source(&root, &external, hash, "receiver");
    }
    copy_source(&root, &external, &shared, "other");
    copy_source(&root, &external, &held, "receiver");
    f.b.forget_external_connection_bodies("receiver").unwrap();
    assert_eq!(f.b.asset_object_byte_size(&orphan).unwrap(), None, "no file, source or custody holds it");
    assert_eq!(f.b.asset_object_byte_size(&external).unwrap(), Some(size), "a local body keeps its row");
    assert_eq!(f.b.asset_object_byte_size(&shared).unwrap(), Some(size), "another connection still holds it");
    assert!(f.b.asset_object_byte_size(&held).unwrap().is_some(), "server custody still holds it");
    assert!(lww_residency::stat(&root, &orphan).unwrap().is_none());
    assert!(lww_residency::stat(&root, &external).unwrap().is_none());
    assert_eq!(lww_residency::stat(&root, &shared).unwrap(), Some(size));
    f.b.asset_gc_dry_run(1024, None, super::runtime::now_ms() as i64, 0).unwrap();
}

/// The status's count for `id`, or zero when it lists none.
fn status_objects(store: &PersistentStore, id: &str) -> u64 {
    status(store)["externalObjects"].as_array().unwrap().iter()
        .find(|entry| entry["connectionId"] == id)
        .map_or(0, |entry| entry["objects"].as_u64().unwrap())
}

/// The narrow count for each connection, checked against the status.
fn connection_counts(store: &PersistentStore) -> [u64; 3] {
    ["receiver", "second", "other"].map(|id| {
        let counted = store.asset_residency_connection_objects(id).unwrap();
        assert_eq!(counted, status_objects(store, id), "{id} differs from the status");
        counted
    })
}

fn registered_root_collections() -> usize {
    crate::external_storage::capture::REGISTERED_ROOT_COLLECTIONS.with(|count| count.get())
}

#[test]
fn a_connection_count_matches_the_status_for_every_holder() {
    let mut f = CycleFixture::new();
    let (_server, held, external) = split_holders(&mut f);
    let root = f.directory_b.path().to_owned();
    // Registered only through a connection this device does not have.
    assert_eq!(connection_counts(&f.b), [0, 0, 0]);
    let connection = receiver_connection(&f);
    ConnectionStore::open(&root).unwrap().insert(&connection.stored).unwrap();
    assert_eq!(connection_counts(&f.b), [1, 0, 0]);
    // A body the server also holds, and one no record references.
    copy_source(&root, &external, &held, "receiver");
    let unreferenced = format!("{:064x}", 3);
    copy_source(&root, &external, &unreferenced, "receiver");
    assert!(!f.b.external_lww_object_is_local(&unreferenced).unwrap());
    assert_eq!(connection_counts(&f.b), [1, 0, 0]);
    let _resolver = lww_residency::install_test_source_connection(&root, Arc::new(connection)).unwrap();
    f.b.asset_residency_download_remote(Some("receiver"), None, None, || Ok(())).unwrap();
    assert!(f.b.external_lww_object_is_local(&external).unwrap());
    assert_eq!(connection_counts(&f.b), [0, 0, 0]);
    f.b.asset_residency_set_policy(AssetPolicy::Full, || Ok(())).unwrap();
    assert!(f.b.external_lww_object_is_local(&held).unwrap());
    assert_eq!(connection_counts(&f.b), [0, 0, 0]);
}

#[test]
fn a_connection_count_reads_the_library_only_for_a_body_that_connection_alone_holds() {
    let mut f = CycleFixture::new();
    let (_connection, _exclusive, _shared) = shared_holders(&mut f);
    let before = registered_root_collections();
    assert_eq!(f.b.asset_residency_connection_objects("second").unwrap(), 0);
    assert_eq!(f.b.asset_residency_connection_objects("other").unwrap(), 0);
    assert_eq!(registered_root_collections(), before, "no body qualified, so the library was not read");
    assert_eq!(f.b.asset_residency_connection_objects("receiver").unwrap(), 1);
    assert_eq!(registered_root_collections(), before + 1);
}

#[test]
fn a_status_collects_the_registered_capture_roots_once() {
    let mut f = CycleFixture::new();
    let (_connection, _exclusive, _shared) = shared_holders(&mut f);
    let before = registered_root_collections();
    status(&f.b);
    assert_eq!(registered_root_collections(), before + 1);
}
