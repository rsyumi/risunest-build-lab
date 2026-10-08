//! A sync receive that archives or restores a character on a device where the stored body it
//! reads is held by the Sync server, an external storage, or nobody.
use super::*;
use crate::asset_repository::PayloadCas;
use crate::external_storage::{
    connection_store::ConnectionStore,
    contract::{Cancellation, ErrorKind, ProviderError},
    lww_residency,
    lww_tests::CycleFixture,
    previous_storage_tests::receiver_connection,
};
use crate::persistent_store::archive::ArchivedObject;
use crate::server_sync::{
    lww_client::LwwClient,
    lww_tests::{drain_publications, header, local, receive_available, LocalServerFixture},
    SyncError,
};
use risunest_sync_wire::stamp::DecimalU64;

const CHARACTER: &str = "char-receive";
const ASSET_KEY: &str = "assets/receive-extra.png";

/// Message text that does not compress, so the archive stays above the size a receive brings in
/// with its records.
fn incompressible() -> String {
    let mut seed = Sha256::digest(b"synthetic receive archive").to_vec();
    (0..8192)
        .map(|_| {
            seed = Sha256::digest(&seed).to_vec();
            hex::encode(&seed)
        })
        .collect()
}

fn detail() -> Value {
    json!({
        "chaId": CHARACTER, "type": "character", "name": "Synthetic receive",
        "additionalAssets": [["extra", ASSET_KEY, "png"]],
        "chats": [{"id": "receive-chat", "name": "Chat", "message": [
            {"role": "user", "data": incompressible(), "chatId": "synthetic-receive-message"}
        ]}],
    })
}

fn add_character(store: &mut PersistentStore) {
    store
        .commit(&WorkingSetCommit {
            add_character: Some(detail()),
            ..empty_working_set_commit(store.revision().expect("read revision"))
        })
        .expect("add the character");
}

/// The owner manifest of an imported character and the one asset only it lists.
struct Owned {
    manifest: String,
    manifest_bytes: Vec<u8>,
    asset: String,
}

/// Adds the character as an import stores it: its additional asset has no alias and only the
/// owner manifest lists it.
fn add_owned_character(store: &mut PersistentStore) -> Owned {
    use crate::asset_repository::{
        job_pins::{CasJobKind, CasJobOwner, CasObjectRole, CasReleaseOutcome, DurableCasJob},
        owner_manifest_codec::{encode_owner_manifest, OwnerManifestEntry},
    };
    let cas = PayloadCas::new(store.repository_root()).expect("open the CAS");
    let mut job = DurableCasJob::begin(
        store.repository_root(),
        "receive-owner",
        CasJobKind::CardOrModuleContentImport,
        CasJobOwner::for_test(),
        1,
    )
    .expect("begin the import");
    let asset = job
        .prepare_reader(&cas, &mut &b"synthetic receive asset"[..], CasObjectRole::DirectObject)
        .expect("store the asset")
        .content_hash;
    let manifest_bytes = encode_owner_manifest(&[OwnerManifestEntry {
        tuple: ["extra".into(), ASSET_KEY.into(), "png".into()],
        payload_hash: Some(hex::decode(&asset).unwrap().try_into().unwrap()),
    }])
    .expect("encode the owner manifest");
    let manifest = job
        .prepare_reader(&cas, &mut manifest_bytes.as_slice(), CasObjectRole::OwnerManifest)
        .expect("store the owner manifest")
        .content_hash;
    job.seal(store, 2).expect("seal the import");
    store
        .commit(&WorkingSetCommit {
            add_character: Some(detail()),
            asset_owner_heads: Some(vec![AssetOwnerHead::present(
                AssetOwnerLocator::CharacterAdditionalAssets { character_id: CHARACTER.into() },
                manifest.clone(),
                1,
            )]),
            ..empty_working_set_commit(store.revision().expect("read revision"))
        })
        .expect("add the character");
    job.release(CasReleaseOutcome::Committed).expect("release the import");
    Owned { manifest, manifest_bytes, asset }
}

fn character(store: &PersistentStore) -> Option<Value> {
    store.materialize(None).expect("materialize the library")["characters"]
        .as_array()
        .expect("characters")
        .iter()
        .find(|character| character["chaId"] == CHARACTER)
        .cloned()
}

/// The character without the fields each device keeps for itself.
fn synced(mut character: Value) -> Value {
    let fields = character.as_object_mut().expect("a character object");
    fields.remove("chatPage");
    fields.remove("lastInteraction");
    character
}

fn archived(store: &PersistentStore) -> Option<ArchivedObject> {
    let generation = active_generation(&store.connection).expect("read active generation");
    archive::read_archived_object(&store.connection, &generation, CHARACTER)
        .expect("read the archived character")
}

fn archive(store: &mut PersistentStore) -> ArchivedObject {
    store
        .archive_character(CHARACTER, store.revision().expect("read revision"), 10)
        .expect("archive the character");
    archived(store).expect("the character is archived")
}

fn restore(store: &mut PersistentStore) {
    store
        .restore_character(CHARACTER, store.revision().expect("read revision"))
        .expect("restore the character");
}

fn is_local(store: &PersistentStore, hash: &str) -> bool {
    PayloadCas::new(store.repository_root())
        .expect("open the CAS")
        .stat_object(hash)
        .expect("stat the body")
        .is_some()
}

/// Removes a body from this device and returns its bytes.
fn take_local_body(store: &PersistentStore, hash: &str) -> Vec<u8> {
    let cas = PayloadCas::new(store.repository_root()).expect("open the CAS");
    let bytes = cas.read_object(hash).expect("read the body").expect("the body is here");
    fs::remove_file(cas.object_path(hash).expect("object path").expect("object path"))
        .expect("remove the body");
    bytes
}

fn put_local_body(store: &PersistentStore, bytes: &[u8]) {
    PayloadCas::new(store.repository_root())
        .expect("open the CAS")
        .prepare_bytes(bytes)
        .expect("put the body back");
}

/// Everything an archive or a restore changes, what a receive stages, and the intents left open.
fn state(store: &PersistentStore) -> (i64, Option<String>, i64, i64, i64, i64) {
    let count = |sql: &str| -> i64 {
        store.connection.query_row(sql, [CHARACTER], |row| row.get(0)).expect("count rows")
    };
    (
        store.revision().expect("read revision"),
        store
            .connection
            .query_row(
                "SELECT archived_object FROM characters WHERE character_id = ?1",
                [CHARACTER],
                |row| row.get(0),
            )
            .expect("read the character row"),
        count("SELECT count(*) FROM conversations WHERE character_id = ?1"),
        count("SELECT count(*) FROM messages WHERE character_id = ?1"),
        store
            .connection
            .query_row("SELECT count(*) FROM lww_receive_rows WHERE status = 'staged'", [], |row| row.get(0))
            .expect("count staged rows"),
        store
            .device_store()
            .expect("open the device store")
            .connection()
            .query_row("SELECT count(*) FROM lww_intents WHERE complete = 0", [], |row| row.get(0))
            .expect("count open intents"),
    )
}

fn assert_clean(store: &mut PersistentStore, device: &str) {
    assert_eq!(
        data_health_tests::blocking(&data_health_tests::scan(store)),
        Vec::<&crate::data_health::Finding>::new(),
        "the data check on {device} is clean"
    );
}

/// The receiving device ends where the sending one is: the same character with its
/// conversations, or the same archive with the same asset references.
fn assert_converged(a: &mut PersistentStore, b: &mut PersistentStore) {
    match (archived(a), archived(b)) {
        (None, None) => {
            let restored = character(b).expect("the character is restored");
            assert_eq!(synced(restored), synced(character(a).expect("the character is on A")));
        }
        (Some(sender), Some(receiver)) => {
            assert_eq!(receiver.shared_object_hash, sender.shared_object_hash);
            assert_eq!(receiver.shared_asset_hashes, sender.shared_asset_hashes);
            assert_eq!(receiver.conversation_count, sender.conversation_count);
            assert_eq!(receiver.message_count, sender.message_count);
            assert!(character(b).is_none());
        }
        (sender, receiver) => panic!("archived on A {}, on B {}", sender.is_some(), receiver.is_some()),
    }
    assert_clean(a, "A");
    assert_clean(b, "B");
}

// ---- Sync server ----

struct Server {
    _server: LocalServerFixture,
    _ra: tempfile::TempDir,
    a: PersistentStore,
    ca: LwwClient,
    _rb: tempfile::TempDir,
    b: PersistentStore,
    cb: LwwClient,
}

impl Server {
    fn new() -> Self {
        Self::on(LocalServerFixture::new())
    }
    fn on(server: LocalServerFixture) -> Self {
        let (_ra, a) = local();
        let (_rb, b) = local();
        let ca = server.client(&a);
        let cb = server.client(&b);
        Self { _server: server, _ra, a, ca, _rb, b, cb }
    }
    fn publish_a(&mut self) {
        drain_publications(&self.ca, &mut self.a, &[]).expect("publish from A");
    }
    fn receive_b(&mut self) -> Result<(), SyncError> {
        receive_available(&self.cb, &mut self.b, &[])
    }
}

/// B holds the archive only as custody on the server: the character and its archive reached B
/// in one receive.
fn server_held_archive(f: &mut Server) -> ArchivedObject {
    add_character(&mut f.a);
    archive(&mut f.a);
    f.publish_a();
    f.receive_b().expect("receive the archive");
    let held = archived(&f.b).expect("the character is archived on B");
    assert!(!is_local(&f.b, &held.object_hash), "the archive stays on the server");
    held
}

/// B archived its own copy, so the archive it holds is one only it has.
fn own_archive(f: &mut Server) -> ArchivedObject {
    add_character(&mut f.a);
    f.publish_a();
    f.receive_b().expect("receive the character");
    archive(&mut f.a);
    f.publish_a();
    f.receive_b().expect("receive the archive");
    let own = archived(&f.b).expect("the character is archived on B");
    assert_ne!(own.object_hash, own.shared_object_hash, "B's archive is its own");
    assert!(is_local(&f.b, &own.object_hash));
    own
}

#[test]
fn a_server_receive_restores_an_archive_only_the_server_holds() {
    let mut f = Server::new();
    let held = server_held_archive(&mut f);
    restore(&mut f.a);
    f.publish_a();
    f.receive_b().expect("receive the restore");
    assert!(archived(&f.b).is_none());
    assert!(is_local(&f.b, &held.object_hash), "the restore read the archive from the server");
    assert_eq!(state(&f.b).5, 0);
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn a_stored_server_page_brings_the_archive_again_when_it_left_before_the_apply() {
    let mut f = Server::new();
    let held = server_held_archive(&mut f);
    restore(&mut f.a);
    f.publish_a();
    let header = header(&f.b);
    f.cb.receive_page(&mut f.b, &header).expect("receive the page");
    take_local_body(&f.b, &held.object_hash);
    f.receive_b().expect("apply the stored page");
    assert!(archived(&f.b).is_none());
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn a_server_receive_restores_an_archive_kept_here() {
    let mut f = Server::new();
    own_archive(&mut f);
    assert_converged(&mut f.a, &mut f.b);
    restore(&mut f.a);
    f.publish_a();
    f.receive_b().expect("receive the restore");
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn a_server_receive_stops_before_anything_changes_when_no_storage_holds_the_archive() {
    let mut f = Server::new();
    let own = own_archive(&mut f);
    let bytes = take_local_body(&f.b, &own.object_hash);
    restore(&mut f.a);
    f.publish_a();
    let unchanged = state(&f.b);
    let error = f.receive_b().expect_err("nothing holds the archive");
    assert_eq!(error.code, "required-asset-unavailable");
    assert_eq!(state(&f.b), unchanged);

    // The receive goes on once the archive is back.
    put_local_body(&f.b, &bytes);
    f.receive_b().expect("receive the restore");
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn a_server_receive_archives_with_the_assets_a_manifest_only_the_server_holds_lists() {
    let mut f = Server::new();
    let owned = add_owned_character(&mut f.a);
    assert_eq!(add_owned_character(&mut f.b).manifest, owned.manifest);
    f.publish_a();
    f.receive_b().expect("receive the character");
    take_local_body(&f.b, &owned.manifest);
    take_local_body(&f.b, &owned.asset);
    let sender = archive(&mut f.a);
    assert!(sender.asset_hashes.contains(&owned.asset));
    f.publish_a();
    f.receive_b().expect("receive the archive");
    let receiver = archived(&f.b).expect("the character is archived on B");
    assert_eq!(receiver.asset_hashes, sender.asset_hashes);
    assert!(is_local(&f.b, &owned.manifest), "the archive read the manifest from the server");
    assert!(!is_local(&f.b, &owned.asset), "the asset stays where it is held");
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn a_server_receive_stops_before_anything_changes_when_no_storage_holds_the_manifest() {
    let mut f = Server::new();
    add_character(&mut f.a);
    let owned = add_owned_character(&mut f.b);
    f.publish_a();
    f.receive_b().expect("receive the character");
    take_local_body(&f.b, &owned.manifest);
    archive(&mut f.a);
    f.publish_a();
    let unchanged = state(&f.b);
    let error = f.receive_b().expect_err("nothing holds the manifest");
    assert_eq!(error.code, "required-asset-unavailable");
    assert_eq!(state(&f.b), unchanged);

    put_local_body(&f.b, &owned.manifest_bytes);
    f.receive_b().expect("receive the archive");
    let receiver = archived(&f.b).expect("the character is archived on B");
    assert!(receiver.asset_hashes.contains(&owned.asset));
    assert!(receiver.asset_hashes.contains(&owned.manifest));
}

#[test]
fn cancelling_a_server_receive_while_it_fetches_the_archive_changes_nothing() {
    use std::sync::Mutex;
    let target = Arc::new(Mutex::new(None::<String>));
    let cancelled = Arc::new(AtomicBool::new(false));
    let server = {
        let (target, cancelled) = (target.clone(), cancelled.clone());
        LocalServerFixture::with_router(move |router| {
            router.layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let (target, cancelled) = (target.clone(), cancelled.clone());
                    async move {
                        let (parts, body) = request.into_parts();
                        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                        let fetching = target.lock().unwrap().as_ref().is_some_and(|hash| {
                            parts.uri.path().contains(hash.as_str())
                                || bytes.windows(hash.len()).any(|window| window == hash.as_bytes())
                        });
                        if fetching {
                            cancelled.store(true, AtomicOrdering::Release);
                        }
                        next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes)))
                            .await
                    }
                },
            ))
        })
    };
    let mut f = Server::on(server);
    let held = server_held_archive(&mut f);
    restore(&mut f.a);
    f.publish_a();
    let stored = f.b.server_stored_config().unwrap().expect("B is configured");
    let mut cancellable = LwwClient::with_cancellation(
        f.b.repository_root(),
        stored.resolve(f.b.repository_root()).unwrap(),
        Some(cancelled.clone()),
    )
    .unwrap();
    cancellable.access = Some(stored);
    let unchanged = state(&f.b);
    *target.lock().unwrap() = Some(held.object_hash.clone());
    let error = receive_available(&cancellable, &mut f.b, &[]).expect_err("the fetch is cancelled");
    assert_eq!(error.code, "cancelled");
    assert!(cancelled.load(AtomicOrdering::Acquire), "the cancellation came during the fetch");
    assert_eq!(state(&f.b), unchanged);

    *target.lock().unwrap() = None;
    cancelled.store(false, AtomicOrdering::Release);
    receive_available(&cancellable, &mut f.b, &[]).expect("receive once nothing cancels it");
    assert_converged(&mut f.a, &mut f.b);
}

// ---- External storage ----

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a runtime")
        .block_on(future)
}

/// Lets B reach the synthetic external storage to fetch what it holds.
fn reach_external_storage(f: &CycleFixture) -> lww_residency::TestSourceConnection {
    let connection = receiver_connection(f);
    ConnectionStore::open(f.directory_b.path())
        .expect("open the connection store")
        .insert(&connection.stored)
        .expect("store the connection");
    lww_residency::install_test_source_connection(f.directory_b.path(), Arc::new(connection))
        .expect("install the connection")
}

fn receive_external(f: &mut CycleFixture, cancel: &Cancellation) -> Result<usize, ProviderError> {
    run(f.receiver.receive_and_apply(&mut f.b, DecimalU64(0), &[], cancel))
}

fn external_held_archive(f: &mut CycleFixture) -> ArchivedObject {
    add_character(&mut f.a);
    archive(&mut f.a);
    run(f.publish_a());
    run(f.receive_b());
    let held = archived(&f.b).expect("the character is archived on B");
    assert!(!is_local(&f.b, &held.object_hash), "the archive stays in the external storage");
    held
}

fn external_own_archive(f: &mut CycleFixture) -> ArchivedObject {
    add_character(&mut f.a);
    run(f.publish_a());
    run(f.receive_b());
    archive(&mut f.a);
    run(f.publish_a());
    run(f.receive_b());
    let own = archived(&f.b).expect("the character is archived on B");
    assert_ne!(own.object_hash, own.shared_object_hash, "B's archive is its own");
    assert!(is_local(&f.b, &own.object_hash));
    own
}

#[test]
fn an_external_receive_restores_an_archive_only_the_external_storage_holds() {
    let mut f = CycleFixture::new();
    let _reach = reach_external_storage(&f);
    let held = external_held_archive(&mut f);
    restore(&mut f.a);
    run(f.publish_a());
    receive_external(&mut f, &Cancellation::default()).expect("receive the restore");
    assert!(archived(&f.b).is_none());
    assert!(is_local(&f.b, &held.object_hash), "the restore read the archive from the external storage");
    assert_eq!(state(&f.b).5, 0);
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn an_external_receive_restores_an_archive_kept_here() {
    let mut f = CycleFixture::new();
    let _reach = reach_external_storage(&f);
    external_own_archive(&mut f);
    assert_converged(&mut f.a, &mut f.b);
    restore(&mut f.a);
    run(f.publish_a());
    receive_external(&mut f, &Cancellation::default()).expect("receive the restore");
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn an_external_receive_stops_before_anything_changes_when_no_storage_holds_the_archive() {
    let mut f = CycleFixture::new();
    let _reach = reach_external_storage(&f);
    let own = external_own_archive(&mut f);
    let bytes = take_local_body(&f.b, &own.object_hash);
    restore(&mut f.a);
    run(f.publish_a());
    let unchanged = state(&f.b);
    let error = receive_external(&mut f, &Cancellation::default()).expect_err("nothing holds the archive");
    assert_eq!(error.kind, ErrorKind::PreviousStorageUnavailable);
    assert_eq!(state(&f.b), unchanged);

    put_local_body(&f.b, &bytes);
    receive_external(&mut f, &Cancellation::default()).expect("receive the restore");
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn an_external_receive_archives_with_the_assets_a_manifest_only_the_external_storage_holds_lists() {
    let mut f = CycleFixture::new();
    let _reach = reach_external_storage(&f);
    let owned = add_owned_character(&mut f.a);
    assert_eq!(add_owned_character(&mut f.b).manifest, owned.manifest);
    run(f.publish_a());
    run(f.receive_b());
    take_local_body(&f.b, &owned.manifest);
    take_local_body(&f.b, &owned.asset);
    let sender = archive(&mut f.a);
    assert!(sender.asset_hashes.contains(&owned.asset));
    run(f.publish_a());
    receive_external(&mut f, &Cancellation::default()).expect("receive the archive");
    let receiver = archived(&f.b).expect("the character is archived on B");
    assert_eq!(receiver.asset_hashes, sender.asset_hashes);
    assert!(is_local(&f.b, &owned.manifest), "the archive read the manifest from the external storage");
    assert!(!is_local(&f.b, &owned.asset), "the asset stays where it is held");
    assert_converged(&mut f.a, &mut f.b);
}

#[test]
fn an_external_receive_stops_before_anything_changes_when_no_storage_holds_the_manifest() {
    let mut f = CycleFixture::new();
    let _reach = reach_external_storage(&f);
    add_character(&mut f.a);
    let owned = add_owned_character(&mut f.b);
    run(f.publish_a());
    run(f.receive_b());
    take_local_body(&f.b, &owned.manifest);
    archive(&mut f.a);
    run(f.publish_a());
    let unchanged = state(&f.b);
    let error = receive_external(&mut f, &Cancellation::default()).expect_err("nothing holds the manifest");
    assert_eq!(error.kind, ErrorKind::PreviousStorageUnavailable);
    assert_eq!(state(&f.b), unchanged);

    put_local_body(&f.b, &owned.manifest_bytes);
    receive_external(&mut f, &Cancellation::default()).expect("receive the archive");
    let receiver = archived(&f.b).expect("the character is archived on B");
    assert!(receiver.asset_hashes.contains(&owned.asset));
    assert!(receiver.asset_hashes.contains(&owned.manifest));
}

#[test]
fn cancelling_an_external_receive_while_it_fetches_the_archive_changes_nothing() {
    let mut f = CycleFixture::new();
    let _reach = reach_external_storage(&f);
    let held = external_held_archive(&mut f);
    restore(&mut f.a);
    run(f.publish_a());
    let source = lww_residency::freeze_remote_body(f.directory_b.path(), &held.object_hash)
        .expect("read the registered source")
        .expect("the external storage holds the archive");
    let object = match source {
        lww_residency::FrozenBodySource::Standalone(source) => {
            source.body.locator.map_or(source.body.object_id, |locator| locator.object)
        }
        lww_residency::FrozenBodySource::Packed(source) => source.packs[0].locator.object.clone(),
    };
    let cancel = Cancellation::default();
    f.provider.cancel_after_read(&object, f.provider.read_attempts(&object) + 1, &cancel);
    let unchanged = state(&f.b);
    let error = receive_external(&mut f, &cancel).expect_err("the fetch is cancelled");
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert!(f.provider.read_attempts(&object) > 0, "the cancellation came during the fetch");
    assert_eq!(state(&f.b), unchanged);

    receive_external(&mut f, &Cancellation::default()).expect("receive once nothing cancels it");
    assert_converged(&mut f.a, &mut f.b);
}
