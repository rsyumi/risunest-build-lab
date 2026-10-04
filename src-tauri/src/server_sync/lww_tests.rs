use super::lww_client::*;
use crate::persistent_store::{
    lww::{Header, UnitMutation},
    PersistentStore, WorkingSetCommit,
};
use risunest_sync_server::{http, store::Store};
use risunest_sync_wire::{
    lww::{OperationReceipt, PushReceipt, PushRequest, UnitChange},
    stamp::Stamp,
    unit::{UnitKey, UnitValue},
    MAX_METADATA_BYTES,
};
use std::sync::Arc;

pub(crate) struct LocalServerFixture {
    pub server: Arc<Store>,
    pub endpoint: String,
    _root: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
    runtime: Option<tokio::runtime::Runtime>,
}
impl LocalServerFixture {
    pub(crate) fn new() -> Self {
        Self::with_router(|router| router)
    }
    pub(crate) fn with_router(wrap: impl FnOnce(axum::Router) -> axum::Router) -> Self {
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
        let app = {
            let _runtime = runtime.enter();
            wrap(http::router(server.clone()))
        };
        let task = runtime.spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            server,
            endpoint,
            _root: root,
            task,
            runtime: Some(runtime),
        }
    }
    pub(crate) fn client(&self, store: &PersistentStore) -> LwwClient {
        let (config, stored) = self.candidate(store);
        store.server_save_config(&stored).unwrap();
        let mut client = LwwClient::new(store.repository_root(), config).unwrap();
        client.client.test_io = Some(Arc::new(super::client::TestIoCounters::default()));
        client
    }
    pub(crate) fn candidate(
        &self,
        store: &PersistentStore,
    ) -> (
        super::client::ServerConfig,
        super::credentials::StoredConfig,
    ) {
        let credential = self.server.add_device().unwrap();
        let config = super::client::ServerConfig {
            directory: None,
            endpoint: self.endpoint.clone(),
            library_id: credential.library_id,
            device_id: credential.device_id,
            token: credential.token,
        };
        let stored =
            super::credentials::StoredConfig::persist(store.repository_root(), &config).unwrap();
        (config, stored)
    }
    pub(crate) fn prepare_binding_candidate(&self, store: &PersistentStore) {
        let (_, stored) = self.candidate(store);
        OperationLog::open(store.repository_root()).unwrap().save_config("candidate", &stored).unwrap();
    }
    pub(crate) fn reopen_client(store: &PersistentStore, counters: Arc<super::client::TestIoCounters>) -> super::Result<LwwClient> {
        let stored = store.server_stored_config()?.ok_or_else(|| super::SyncError::new("server-unconfigured",409))?;
        let mut client = LwwClient::new(store.repository_root(), stored.resolve(store.repository_root())?)?;
        client.access = Some(stored);
        client.client.test_io = Some(counters);
        Ok(client)
    }
}
impl Drop for LocalServerFixture {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(2));
        }
    }
}
pub(crate) fn local() -> (tempfile::TempDir, PersistentStore) {
    let root = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(root.path()).unwrap();
    (root, store)
}
pub(crate) fn put_asset(
    store: &mut PersistentStore,
    key: &str,
    bytes: &[u8],
) -> crate::persistent_store::AssetAlias {
    let object = crate::asset_repository::PayloadCas::new(store.repository_root())
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
    let alias = crate::persistent_store::AssetAlias {
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
        metadata: serde_json::json!({}),
    };
    store
        .commit_asset_alias(&alias, store.revision().unwrap())
        .unwrap();
    alias
}
pub(crate) fn header(store: &PersistentStore) -> Header {
    Header {
        binding_authority: store.lww_binding_authority().unwrap(),
        request_id: uuid::Uuid::new_v4().to_string(),
    }
}
pub(crate) fn save(store: &mut PersistentStore, parts: &[&str], value: serde_json::Value) {
    store
        .commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            unit_mutations: Some(vec![UnitMutation::Set {
                key: UnitKey::new(parts).unwrap(),
                value,
            }]),
            ..Default::default()
        })
        .unwrap();
}
fn push(client: &LwwClient, store: &mut PersistentStore) {
    assert!(publish_cycle(client, store, &[]).unwrap().is_some());
}
fn receive(client: &LwwClient, store: &mut PersistentStore) {
    receive_available(client, store, &[]).unwrap();
}
pub(crate) fn publish_cycle(
    client: &LwwClient,
    store: &mut PersistentStore,
    generating: &[crate::persistent_store::lww::MessageLocator],
) -> super::Result<Option<risunest_sync_wire::lww::PushReceipt>> {
    client.push(store, &header(store), generating)
}
pub(crate) fn receive_cycle(
    client: &LwwClient,
    store: &mut PersistentStore,
    generating: &[crate::persistent_store::lww::MessageLocator],
) -> super::Result<super::lww_client::NativeReceiveCompletion> {
    client.receive_native_complete(store, &header(store), generating)
}
pub(crate) fn drain_publications(
    client: &LwwClient,
    store: &mut PersistentStore,
    generating: &[crate::persistent_store::lww::MessageLocator],
) -> super::Result<()> {
    while publish_cycle(client, store, generating)?.is_some() {}
    Ok(())
}
pub(crate) fn receive_available(
    client: &LwwClient,
    store: &mut PersistentStore,
    generating: &[crate::persistent_store::lww::MessageLocator],
) -> super::Result<()> {
    loop {
        if receive_cycle(client, store, generating)?.received_units == 0 {
            return Ok(());
        }
    }
}

#[test]
fn two_native_stores_converge_independent_units_in_both_push_orders() {
    for reverse in [false, true] {
        let server = LocalServerFixture::new();
        let (_a, mut a) = local();
        let (_b, mut b) = local();
        let ca = server.client(&a);
        let cb = server.client(&b);
        save(&mut a, &["root", "language"], serde_json::json!("ja"));
        save(&mut b, &["root", "askRemoval"], serde_json::json!(false));
        if reverse {
            push(&cb, &mut b);
            push(&ca, &mut a);
        } else {
            push(&ca, &mut a);
            push(&cb, &mut b);
        }
        receive(&ca, &mut a);
        receive(&cb, &mut b);
        assert_eq!(
            a.read_root(None).unwrap().value,
            b.read_root(None).unwrap().value
        );
        assert_eq!(a.read_root(None).unwrap().value["language"], "ja");
        assert_eq!(a.read_root(None).unwrap().value["askRemoval"], false);
        assert!(a.lww_read_outbox(0.into(), 256).unwrap().entries.is_empty());
        assert!(b.lww_read_outbox(0.into(), 256).unwrap().entries.is_empty());
    }
}
#[test]
fn greater_remote_unit_dominates_an_unsent_local_change() {
    let server = LocalServerFixture::new();
    let (_a, mut a) = local();
    let (_b, mut b) = local();
    let ca = server.client(&a);
    let cb = server.client(&b);
    save(&mut a, &["root", "language"], serde_json::json!("ja"));
    push(&ca, &mut a);
    receive(&cb, &mut b);
    save(&mut a, &["root", "language"], serde_json::json!("ko"));
    receive(&cb, &mut b);
    save(&mut b, &["root", "askRemoval"], serde_json::json!(true));
    save(&mut b, &["root", "language"], serde_json::json!("de"));
    push(&cb, &mut b);
    receive(&ca, &mut a);
    assert_eq!(a.read_root(None).unwrap().value["language"], "de");
    assert!(a.lww_read_outbox(0.into(), 256).unwrap().entries.is_empty());
}
#[test]
fn absent_operation_is_cancelled_and_delayed_original_post_cannot_publish() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let client = server.client(&store);
    save(&mut store, &["root", "language"], serde_json::json!("ja"));
    let entries = store.lww_read_outbox(0.into(), 256).unwrap().entries;
    let publication = Publication {
        authority: 0.into(),
        request: PushRequest {
            library_id: client.client.config().library_id,
            writer_id: store.lww_clock_state().unwrap().writer_id,
            operation_id: uuid::Uuid::new_v4().to_string(),
            changes: entries
                .iter()
                .map(|e| UnitChange {
                    key: e.key.clone(),
                    stamp: e.stamp.clone(),
                    value: e.value.clone(),
                })
                .collect(),
        },
        entries,
        config: store.server_stored_config().unwrap().unwrap(),
    };
    let bytes = client.log.prepare(&publication).unwrap();
    let receipt = client.settle(&publication).unwrap();
    assert!(
        matches!(receipt,OperationReceipt::Rejected{ref error,..} if error=="operation-cancelled")
    );
    let reply = client
        .client
        .request(
            reqwest::Method::POST,
            "push",
            &[],
            Some(bytes),
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(reply.status, 409);
    assert_eq!(
        store.lww_read_outbox(0.into(), 256).unwrap().entries.len(),
        1
    );
}
#[test]
fn equal_stamp_different_value_stops_sync_after_original_is_superseded() {
    let server = LocalServerFixture::new();
    let (_root, store) = local();
    let client = server.client(&store);
    let writer = store.lww_clock_state().unwrap().writer_id;
    let first = PushRequest {
        library_id: client.client.config().library_id,
        writer_id: writer.clone(),
        operation_id: "first".into(),
        changes: vec![UnitChange {
            key: UnitKey::new(&["root", "language"]).unwrap(),
            stamp: Stamp {
                physical_ms: 1.into(),
                logical: 0,
                writer_id: writer,
            },
            value: UnitValue::inline(br#""en""#).unwrap(),
        }],
    };
    let (_, _): (_, PushReceipt) = client
        .client
        .json(reqwest::Method::POST, "push", &[], Some(&first), &[])
        .unwrap();
    let mut newer = first.clone();
    newer.operation_id = "newer".into();
    newer.changes[0].stamp.physical_ms = 2.into();
    newer.changes[0].value = UnitValue::inline(br#""ko""#).unwrap();
    client
        .client
        .json::<PushReceipt>(reqwest::Method::POST, "push", &[], Some(&newer), &[])
        .unwrap();
    let mut collision = first;
    collision.operation_id = "collision".into();
    collision.changes[0].value = UnitValue::inline(br#""de""#).unwrap();
    let error = client
        .client
        .json::<PushReceipt>(reqwest::Method::POST, "push", &[], Some(&collision), &[])
        .unwrap_err();
    assert_eq!(error.code, "equal-stamp-integrity");
}

#[test]
fn lost_accepted_response_is_recovered_after_reopen_and_keeps_a_later_edit_pending() {
    let server = LocalServerFixture::new();
    let (root, mut store) = local();
    let core = server.client(&store);
    save(&mut store, &["root", "language"], serde_json::json!("ja"));
    let entries = store.lww_read_outbox(0.into(), 256).unwrap().entries;
    let publication = Publication {
        authority: 0.into(),
        request: PushRequest {
            library_id: core.client.config().library_id,
            writer_id: store.lww_clock_state().unwrap().writer_id,
            operation_id: uuid::Uuid::new_v4().to_string(),
            changes: entries
                .iter()
                .map(|e| UnitChange {
                    key: e.key.clone(),
                    stamp: e.stamp.clone(),
                    value: e.value.clone(),
                })
                .collect(),
        },
        entries,
        config: store.server_stored_config().unwrap().unwrap(),
    };
    let body = core.log.prepare(&publication).unwrap();
    let reply = core
        .client
        .request(
            reqwest::Method::POST,
            "push",
            &[],
            Some(body),
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(reply.status, 200);
    save(&mut store, &["root", "language"], serde_json::json!("ko"));
    let config = core.client.config();
    drop(core);
    drop(store);
    let mut store = PersistentStore::open(root.path()).unwrap();
    let core = LwwClient::new(root.path(), config).unwrap();
    core.fence(&mut store).unwrap();
    assert_eq!(
        store.lww_read_outbox(0.into(), 256).unwrap().entries.len(),
        1
    );
    assert_eq!(store.read_root(None).unwrap().value["language"], "ko");
    push(&core, &mut store);
    assert!(store
        .lww_read_outbox(0.into(), 256)
        .unwrap()
        .entries
        .is_empty());
}
#[test]
fn receive_crash_replays_exact_page_and_refuses_ack_before_native_durability() {
    let server = LocalServerFixture::new();
    let (_a, mut source) = local();
    let (root, mut target) = local();
    let a = server.client(&source);
    let b = server.client(&target);
    save(&mut source, &["root", "language"], serde_json::json!("ja"));
    push(&a, &mut source);
    let request = header(&target);
    let page = b.receive_page(&mut target, &request).unwrap();
    assert_eq!(
        b.finish_receive(&target, &page.header).unwrap_err().code,
        "receive-not-durable"
    );
    let config = b.client.config();
    drop(b);
    drop(target);
    let mut target = PersistentStore::open(root.path()).unwrap();
    let b = LwwClient::new(root.path(), config).unwrap();
    let request = header(&target);
    let replay = b.receive_page(&mut target, &request).unwrap();
    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        serde_json::to_value(&replay).unwrap()
    );
    target.lww_stage_receive(&replay).unwrap();
    target
        .lww_apply_receive(&crate::persistent_store::lww::ApplyReceive {
            header: replay.header.clone(),
            generating: vec![],
        })
        .unwrap();
    target.lww_finish_receive(&replay.header).unwrap();
    b.finish_receive(&target, &replay.header).unwrap();
    assert_eq!(target.read_root(None).unwrap().value["language"], "ja");
}
#[test]
fn messages_converge_as_a_whole_list_without_merging_concurrent_appends() {
    use crate::persistent_store::ConversationMutation;
    let server = LocalServerFixture::new();
    let (_a, mut a) = local();
    let (_b, mut b) = local();
    let ca = server.client(&a);
    let cb = server.client(&b);
    save(
        &mut a,
        &["exists", "character", "char"],
        serde_json::json!({"type":"character"}),
    );
    save(
        &mut a,
        &["exists", "conversation", "char", "chat"],
        serde_json::json!(true),
    );
    a.commit(&WorkingSetCommit {
        expected_revision: a.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char".into(),
            conversation_id: "chat".into(),
            start: 0,
            delete_count: 0,
            messages: vec![serde_json::json!({"role":"user","data":"base","chatId":"preserved"})],
            conversation: None,
            configured_index: None,
        }]),
        ..Default::default()
    })
    .unwrap();
    push(&ca, &mut a);
    receive(&cb, &mut b);
    for (store, text) in [(&mut a, "append-a"), (&mut b, "append-b")] {
        store
            .commit(&WorkingSetCommit {
                expected_revision: store.revision().unwrap(),
                conversations: Some(vec![ConversationMutation::ReplaceRange {
                    character_id: "char".into(),
                    conversation_id: "chat".into(),
                    start: 1,
                    delete_count: 0,
                    messages: vec![serde_json::json!({"role":"user","data":text})],
                    conversation: None,
                    configured_index: None,
                }]),
                ..Default::default()
            })
            .unwrap();
    }
    let key = UnitKey::new(&["messages", "char", "chat"]).unwrap();
    let ea = a
        .lww_read_outbox(0.into(), 256)
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.key == key)
        .unwrap();
    let eb = b
        .lww_read_outbox(0.into(), 256)
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.key == key)
        .unwrap();
    let expected = if ea.stamp > eb.stamp {
        "append-a"
    } else {
        "append-b"
    };
    push(&ca, &mut a);
    push(&cb, &mut b);
    receive(&ca, &mut a);
    receive(&cb, &mut b);
    let av = a
        .read_conversation("char", "chat", None)
        .unwrap()
        .unwrap()
        .value;
    let bv = b
        .read_conversation("char", "chat", None)
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(av, bv);
    assert_eq!(av["message"].as_array().unwrap().len(), 2);
    assert_eq!(av["message"][0]["chatId"], "preserved");
    assert_eq!(av["message"][1]["data"], expected);
}
#[test]
fn asset_alias_receive_retains_remote_custody_before_apply_and_hydrates_only_missing_bodies() {
    let server = LocalServerFixture::new();
    let (_a, mut a) = local();
    let (_b, mut b) = local();
    let ca = server.client(&a);
    let cb = server.client(&b);
    let body = vec![42; 128 * 1024 + 17];
    let alias = put_asset(&mut a, "assets/synthetic.png", &body);
    push(&ca, &mut a);
    let hash = alias.object_hash.unwrap();
    let cas = crate::asset_repository::PayloadCas::new(b.repository_root()).unwrap();
    let request = header(&b);
    let page = cb.receive_page(&mut b, &request).unwrap();
    assert!(super::residency::Residency::open(b.repository_root())
        .unwrap()
        .object(&hash, None)
        .unwrap()
        .is_some());
    assert!(cas.stat_object(&hash).unwrap().is_none());
    b.lww_stage_receive(&page).unwrap();
    b.lww_apply_receive(&crate::persistent_store::lww::ApplyReceive {
        header: page.header.clone(),
        generating: vec![],
    })
    .unwrap();
    b.lww_finish_receive(&page.header).unwrap();
    cb.finish_receive(&b, &page.header).unwrap();
    assert!(cas.stat_object(&hash).unwrap().is_none());
    b.hydrate_registered_remote_assets(|| Ok(())).unwrap();
    assert_eq!(cas.read_object(&hash).unwrap().unwrap(), body);
    let before = cb
        .client
        .test_io
        .as_ref()
        .unwrap()
        .requests
        .load(std::sync::atomic::Ordering::Relaxed);
    cb.prepare_bodies(&mut b, &page.changes, page.admitted_time_upper_ms)
        .unwrap();
    assert_eq!(
        before,
        cb.client
            .test_io
            .as_ref()
            .unwrap()
            .requests
            .load(std::sync::atomic::Ordering::Relaxed)
    );
}
#[test]
fn no_edit_push_does_not_make_a_request() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let client = server.client(&store);
    let before = client
        .client
        .test_io
        .as_ref()
        .unwrap()
        .requests
        .load(std::sync::atomic::Ordering::Relaxed);
    let request = header(&store);
    assert!(client.push(&mut store, &request, &[]).unwrap().is_none());
    assert_eq!(
        before,
        client
            .client
            .test_io
            .as_ref()
            .unwrap()
            .requests
            .load(std::sync::atomic::Ordering::Relaxed)
    );
}

fn publication(client: &LwwClient, store: &PersistentStore) -> Publication {
    let entries = store
        .lww_read_outbox(store.lww_binding_authority().unwrap(), 256)
        .unwrap()
        .entries;
    Publication {
        authority: store.lww_binding_authority().unwrap(),
        request: PushRequest {
            library_id: client.client.config().library_id,
            writer_id: store.lww_clock_state().unwrap().writer_id,
            operation_id: uuid::Uuid::new_v4().to_string(),
            changes: entries
                .iter()
                .map(|entry| UnitChange {
                    key: entry.key.clone(),
                    stamp: entry.stamp.clone(),
                    value: entry.value.clone(),
                })
                .collect(),
        },
        entries,
        config: store.server_stored_config().unwrap().unwrap(),
    }
}

#[test]
fn initial_state_retry_settles_original_acceptance_without_republishing_acked_units() {
    let server = LocalServerFixture::new();
    let (root, mut store) = local();
    let client = server.client(&store);
    save(&mut store, &["root", "language"], serde_json::json!("ja"));
    let operation = publication(&client, &store);
    let body = client.log.prepare(&operation).unwrap();
    let reply = client
        .client
        .request(
            reqwest::Method::POST,
            "push",
            &[],
            Some(body),
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(reply.status, 200);
    let config = client.client.config();
    drop(client);
    drop(store);
    let mut store = PersistentStore::open(root.path()).unwrap();
    let mut client = LwwClient::new(root.path(), config).unwrap();
    let io = std::sync::Arc::new(super::client::TestIoCounters::default());
    client.client.test_io = Some(io.clone());
    save(
        &mut store,
        &["root", "askRemoval"],
        serde_json::json!(false),
    );
    for attempt in 0..2 {
        let request = header(&store);
        store
            .lww_queue_unit_state_page(&request, None, 256)
            .unwrap();
        io.reset();
        let receipt = client.push(&mut store, &request, &[]).unwrap();
        let counters = io.snapshot();
        if attempt == 1 {
            assert!(receipt.is_none());
            assert_eq!(counters[0], 0);
        } else {
            assert!(receipt.is_some());
            assert!(counters[0] > 0);
            let intent: String = client
                .log
                .0
                .query_row(
                    "SELECT intent FROM publications WHERE id<>?1",
                    [&operation.request.operation_id],
                    |row| row.get(0),
                )
                .unwrap();
            let next: Publication = serde_json::from_str(&intent).unwrap();
            assert_eq!(next.request.changes.len(), 1);
            assert_eq!(
                next.request.changes[0].key,
                UnitKey::new(&["root", "askRemoval"]).unwrap()
            );
            let original: Vec<u8> = client
                .log
                .0
                .query_row(
                    "SELECT body FROM publications WHERE id=?1",
                    [&operation.request.operation_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                original,
                risunest_sync_wire::canonical::encode(&operation.request).unwrap()
            );
        }
        assert!(store
            .lww_read_outbox(0.into(), 256)
            .unwrap()
            .entries
            .is_empty());
        assert_eq!(
            client
                .log
                .0
                .query_row("SELECT COUNT(*) FROM publications", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            // The next fence drops both acknowledged publications and the
            // empty outbox adds none.
            if attempt == 0 { 2 } else { 0 }
        );
    }
}

#[test]
fn terminal_receipt_saved_before_ack_recovers_exact_versions_after_reopen() {
    let server = LocalServerFixture::new();
    let (root, mut store) = local();
    let client = server.client(&store);
    save(&mut store, &["root", "language"], serde_json::json!("ja"));
    let operation = publication(&client, &store);
    let body = client.log.prepare(&operation).unwrap();
    let reply = client
        .client
        .request(
            reqwest::Method::POST,
            "push",
            &[],
            Some(body),
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(reply.status, 200);
    assert!(matches!(
        client.settle(&operation).unwrap(),
        OperationReceipt::Accepted { .. }
    ));
    save(
        &mut store,
        &["root", "askRemoval"],
        serde_json::json!(false),
    );
    let config = client.client.config();
    drop(client);
    drop(store);
    let mut store = PersistentStore::open(root.path()).unwrap();
    let client = LwwClient::new(root.path(), config).unwrap();
    client.fence(&mut store).unwrap();
    let pending = store.lww_read_outbox(0.into(), 256).unwrap().entries;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].key,
        UnitKey::new(&["root", "askRemoval"]).unwrap()
    );
    client.fence(&mut store).unwrap();
    assert_eq!(
        pending,
        store.lww_read_outbox(0.into(), 256).unwrap().entries
    );
}

#[test]
fn expired_bootstrap_pin_restarts_and_preserves_unsent_units() {
    let server = LocalServerFixture::new();
    let (_source_root, mut source) = local();
    let (_target_root, mut target) = local();
    let source_client = server.client(&source);
    let target_client = server.client(&target);
    save(&mut source, &["root", "language"], serde_json::json!("ja"));
    push(&source_client, &mut source);
    save(
        &mut target,
        &["root", "askRemoval"],
        serde_json::json!(false),
    );
    let (_, pin): (_, risunest_sync_wire::lww::StatePin) = target_client
        .client
        .json(
            reqwest::Method::POST,
            "state/pins",
            &[],
            None::<&risunest_sync_wire::lww::AckRequest>,
            &[],
        )
        .unwrap();
    target_client
        .log
        .0
        .execute(
            "INSERT INTO bootstrap VALUES('0',?1,NULL,'0')",
            [serde_json::to_string(&pin).unwrap()],
        )
        .unwrap();
    let reply = target_client
        .client
        .request(
            reqwest::Method::DELETE,
            &format!("state/pins/{}", pin.pin_id),
            &[],
            None,
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(reply.status, 204);
    receive(&target_client, &mut target);
    assert_eq!(target.read_root(None).unwrap().value["language"], "ja");
    assert_eq!(target.read_root(None).unwrap().value["askRemoval"], false);
    assert_eq!(
        target.lww_read_outbox(0.into(), 256).unwrap().entries.len(),
        1
    );
    let active_pins: i64 = target_client
        .log
        .0
        .query_row("SELECT count(*) FROM bootstrap", [], |row| row.get(0))
        .unwrap();
    assert_eq!(active_pins, 0);
}

#[test]
fn unpublished_future_operation_requires_exact_server_rejection_before_restamping() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let client = server.client(&store);
    let writer = store.lww_clock_state().unwrap().writer_id;
    let future = Stamp {
        physical_ms: ((std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64)
            + 1_000_000)
            .into(),
        logical: 0,
        writer_id: writer,
    };
    store
        .device_store()
        .unwrap()
        .connection()
        .execute(
            "UPDATE lww_clock SET issued=?1",
            [serde_json::to_string(&future).unwrap()],
        )
        .unwrap();
    save(&mut store, &["root", "language"], serde_json::json!("ja"));
    let operation = publication(&client, &store);
    let body = client.log.prepare(&operation).unwrap();
    let reply = client
        .client
        .request(
            reqwest::Method::POST,
            "push",
            &[],
            Some(body),
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(reply.status, 409);
    let request = header(&store);
    let corrected = client.retry_unpublished(&mut store, &request).unwrap();
    assert_eq!(corrected.affected_keys.len(), 1);
    let pending = store.lww_read_outbox(0.into(), 256).unwrap().entries;
    assert!(pending[0].stamp < operation.entries[0].stamp);
    assert_eq!(pending[0].value, operation.entries[0].value);
    let terminal = client.settle(&operation).unwrap();
    assert!(matches!(terminal, OperationReceipt::Rejected { .. }));
    let delayed = client
        .client
        .request(
            reqwest::Method::POST,
            "push",
            &[],
            Some(client.log.prepare(&operation).unwrap()),
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(delayed.status, 409);
    push(&client, &mut store);
    assert!(store
        .lww_read_outbox(0.into(), 256)
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn real_journal_floor_bootstrap_receives_state_without_replacing_pending_local_edits() {
    let server = LocalServerFixture::new();
    let (_a, mut source) = local();
    let (_b, mut target) = local();
    let a = server.client(&source);
    let b = server.client(&target);
    save(&mut source, &["root", "language"], serde_json::json!("ja"));
    push(&a, &mut source);
    save(
        &mut target,
        &["root", "askRemoval"],
        serde_json::json!(false),
    );
    let staging = target.replace_begin().unwrap();
    target
        .replace_put_upstream_root(&staging.staging_id, &serde_json::json!({"language":"ko"}))
        .unwrap();
    let staged_before = target.materialize_staging(&staging.staging_id).unwrap();
    let db = rusqlite::Connection::open(server._root.path().join("metadata.sqlite")).unwrap();
    db.execute("UPDATE journal SET created=0", []).unwrap();
    drop(db);
    let floor = server.server.maintain().unwrap().min_retained_seq;
    assert_ne!(floor.as_str(), "0");
    let reply = b
        .client
        .request(
            reqwest::Method::GET,
            "changes",
            &[("after", "0".into())],
            None,
            &[],
            MAX_METADATA_BYTES,
        )
        .unwrap();
    assert_eq!(reply.status, 410);
    receive(&b, &mut target);
    let staged_after = target.materialize_staging(&staging.staging_id).unwrap();
    assert_eq!(staged_after, staged_before);
    assert_eq!(target.read_root(None).unwrap().value["language"], "ja");
    assert_eq!(target.read_root(None).unwrap().value["askRemoval"], false);
    assert_eq!(
        target.lww_read_outbox(0.into(), 256).unwrap().entries.len(),
        1
    );
    assert_eq!(
        target.lww_receive_progress(0.into()).unwrap()[0]
            .cursor
            .0
            .to_string(),
        floor.as_str()
    );
}

#[test]
fn production_cycle_counters_record_actual_hash_inputs_and_completed_native_receive() {
    let server = LocalServerFixture::new();
    let (_a, mut source) = local();
    let (_b, mut target) = local();
    let a = server.client(&source);
    let b = server.client(&target);
    save(&mut source, &["root", "language"], serde_json::json!("ja"));
    a.client.test_io.as_ref().unwrap().reset();
    super::hash_metrics::reset_hash_metrics();
    push(&a, &mut source);
    let sent = super::hash_metrics::take_hash_metrics();
    assert!(a.client.test_io.as_ref().unwrap().snapshot()[0] > 0);
    assert!(!sent.incomplete);
    assert!(sent.domains["c_operation_identity"].calls >= 3);
    assert!(sent.domains["c_wire_identity"].bytes > 0);
    assert!(!sent.domains.contains_key("c_cache_identity"));
    super::hash_metrics::reset_hash_metrics();
    receive(&b, &mut target);
    let received = super::hash_metrics::take_hash_metrics();
    assert!(!received.incomplete);
    assert_eq!(target.read_root(None).unwrap().value["language"], "ja");
    assert!(
        b.client
            .test_io
            .as_ref()
            .unwrap()
            .requests
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    );
}

#[test]
fn actual_append_cycle_tracks_control_files_without_opening_known_asset_bodies() {
    use crate::{
        asset_repository::body_io::{
            register_object_purpose, reset_body_io, take_body_io, BodyPurpose,
        },
        persistent_store::ConversationMutation,
    };
    let server = LocalServerFixture::new();
    let (_a, mut source) = local();
    let (_b, mut target) = local();
    let a = server.client(&source);
    let b = server.client(&target);
    save(
        &mut source,
        &["exists", "character", "char"],
        serde_json::json!({"type":"character"}),
    );
    save(
        &mut source,
        &["exists", "conversation", "char", "chat"],
        serde_json::json!(true),
    );
    let asset = put_asset(
        &mut source,
        "assets/known-body.png",
        &vec![42; 128 * 1024 + 17],
    )
    .object_hash
    .unwrap();
    source
        .commit(&WorkingSetCommit {
            expected_revision: source.revision().unwrap(),
            conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char".into(), conversation_id: "chat".into(),
            start: 0, delete_count: 0,
            messages: (0..64).map(|index| serde_json::json!({
                "role":"user", "data":"s".repeat(9000), "chatId":format!("message-{index}")
            })).collect(),
            conversation: None, configured_index: None,
        }]),
            ..Default::default()
        })
        .unwrap();
    drain_publications(&a, &mut source, &[]).unwrap();
    receive_available(&b, &mut target, &[]).unwrap();
    target.hydrate_registered_remote_assets(|| Ok(())).unwrap();

    reset_body_io();
    register_object_purpose(&asset, BodyPurpose::Asset);
    source.commit(&WorkingSetCommit {
        expected_revision: source.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char".into(), conversation_id: "chat".into(),
            start: 64, delete_count: 0,
            messages: vec![serde_json::json!({"role":"user","data":"appended".repeat(9 * 1024),"chatId":"preserved-new-id"})],
            conversation: None, configured_index: None,
        }]),
        ..Default::default()
    }).unwrap();
    assert!(publish_cycle(&a, &mut source, &[]).unwrap().is_some());
    let received = receive_cycle(&b, &mut target, &[]).unwrap();
    let observed = take_body_io();
    assert!(observed.complete(), "{observed:#?}");
    let controls = observed
        .objects
        .values()
        .filter(|object| {
            object.purposes.contains(&BodyPurpose::Control)
                && !object.purposes.contains(&BodyPurpose::Asset)
        })
        .collect::<Vec<_>>();
    assert!(controls.iter().map(|object| object.work.opens).sum::<u64>() > 0);
    assert!(
        controls
            .iter()
            .map(|object| object.work.read_bytes)
            .sum::<u64>()
            > super::cache::SMALL_OBJECT_BYTES as u64
    );
    let known_asset = observed.objects.get(&asset).unwrap();
    assert!(known_asset.purposes.contains(&BodyPurpose::Asset));
    assert_eq!(known_asset.work.open_attempts, 0);
    assert_eq!(known_asset.work.read_bytes, 0);
    assert!(received
        .result
        .affected_keys
        .contains(&UnitKey::new(&["messages", "char", "chat"]).unwrap()));
    let value = target
        .read_conversation("char", "chat", None)
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(value["message"].as_array().unwrap().len(), 65);
    assert_eq!(value["message"][64]["chatId"], "preserved-new-id");
}

#[test]
fn verified_native_controls_skip_cached_file_reads_and_unverified_corruption_fails_closed() {
    use crate::{
        asset_repository::body_io::{reset_body_io, take_body_io, BodyPurpose},
        persistent_store::{lww::Change, ConversationMutation},
    };
    use risunest_sync_wire::lww::{AckRequest, ChangesPage};

    let server = LocalServerFixture::new();
    let (_a, mut source) = local();
    let (_b, mut target) = local();
    let a = server.client(&source);
    let b = server.client(&target);
    save(
        &mut source,
        &["exists", "character", "char"],
        serde_json::json!({"type":"character"}),
    );
    save(
        &mut source,
        &["exists", "conversation", "char", "chat"],
        serde_json::json!(true),
    );
    source
        .commit(&WorkingSetCommit {
            expected_revision: source.revision().unwrap(),
            conversations: Some(vec![ConversationMutation::ReplaceRange {
                character_id: "char".into(),
                conversation_id: "chat".into(),
                start: 0,
                delete_count: 0,
                messages: vec![serde_json::json!({
                    "role":"user", "data":"p".repeat(90 * 1024), "chatId":"preserved-id"
                })],
                conversation: None,
                configured_index: None,
            }]),
            ..Default::default()
        })
        .unwrap();
    drain_publications(&a, &mut source, &[]).unwrap();
    let (_, page): (_, ChangesPage) = b
        .client
        .json(
            reqwest::Method::GET,
            "changes",
            &[("after", "0".into()), ("limit", "256".into())],
            None::<&AckRequest>,
            &[],
        )
        .unwrap();
    let changes = page
        .items
        .into_iter()
        .map(|item| Change {
            key: item.key,
            stamp: item.stamp,
            value: item.value,
        })
        .collect::<Vec<_>>();
    let page_hash = changes
        .iter()
        .find_map(|change| {
            if change.key.components()[0] == "messages" {
                match &change.value {
                    UnitValue::Object { descriptor, .. } => {
                        assert_eq!(descriptor.dependencies.len(), 1);
                        Some(descriptor.dependencies[0].clone())
                    }
                    _ => None,
                }
            } else {
                None
            }
        })
        .unwrap();
    assert!(!target.lww_verified_object_present(&page_hash).unwrap());

    let cache = super::cache::Cache::open(
        &target.repository_root().join("server-sync/lww-cache"),
    )
    .unwrap()
    .with_library(target.repository_root())
    .unwrap();
    super::transfer::Transfer::new(&b.client, &cache)
        .unwrap()
        .download(&[page_hash.clone()], &[])
        .unwrap();
    let original = cache.read(&page_hash, MAX_METADATA_BYTES).unwrap();
    assert!(original.len() > super::cache::SMALL_OBJECT_BYTES);
    let path = cache.cas.object_path(&page_hash).unwrap().unwrap();
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    std::fs::write(&path, &corrupt).unwrap();
    let upper = b.admission().unwrap();
    assert_eq!(
        b.prepare_bodies(&mut target, &changes, upper)
            .unwrap_err()
            .code,
        "cached-object-corrupt"
    );
    assert!(!target.lww_verified_object_present(&page_hash).unwrap());

    std::fs::write(&path, &original).unwrap();
    b.prepare_bodies(&mut target, &changes, upper).unwrap();
    assert!(target.lww_verified_object_present(&page_hash).unwrap());
    std::fs::write(&path, &corrupt).unwrap();
    reset_body_io();
    b.client.test_io.as_ref().unwrap().reset();
    b.prepare_bodies(&mut target, &changes, upper).unwrap();
    let observed = take_body_io();
    assert!(observed.complete(), "{observed:#?}");
    let cached_page = observed.objects.get(&page_hash).unwrap();
    assert!(cached_page.purposes.contains(&BodyPurpose::Control));
    assert_eq!(cached_page.work.open_attempts, 0);
    assert_eq!(cached_page.work.read_bytes, 0);
    assert_eq!(b.client.test_io.as_ref().unwrap().snapshot()[0], 0);

    receive_available(&b, &mut target, &[]).unwrap();
    let value = target
        .read_conversation("char", "chat", None)
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(value["message"][0]["chatId"], "preserved-id");
    assert_eq!(value["message"][0]["data"], "p".repeat(90 * 1024));
}

#[test]
fn a_new_target_initialization_preserves_received_issuers_under_its_authenticated_publisher() {
    let old_target = LocalServerFixture::new();
    let new_target = LocalServerFixture::new();
    let (_a, mut source) = local();
    let (_b, mut target) = local();
    let a = old_target.client(&source);
    let b = old_target.client(&target);
    let original_issuer = source.lww_clock_state().unwrap().writer_id;
    save(&mut source, &["root", "language"], serde_json::json!("ja"));
    push(&a, &mut source);
    receive(&b, &mut target);
    save(
        &mut target,
        &["root", "askRemoval"],
        serde_json::json!(false),
    );
    let publisher = target.lww_clock_state().unwrap().writer_id;
    assert_ne!(publisher, original_issuer);
    let c = new_target.client(&target);
    let original = target.lww_binding_state().unwrap();
    let inspected = super::binding::inspect(&target, &header(&target)).unwrap();
    let request = header(&target);
    target
        .switch_lww_binding(
            &crate::persistent_store::sync_selection::SwitchBindingRequest {
                header: request,
                expected_selection_epoch: original.selection_epoch,
                target: crate::persistent_store::sync_selection::SyncTarget::Server(
                    "server".into(),
                ),
                inspection_id: Some(inspected.inspection_id),
            },
        )
        .unwrap();
    let request = header(&target);
    super::binding::activate(&mut target, &request, None).unwrap();
    let authority = target.lww_binding_authority().unwrap();
    let mut after = None;
    loop {
        let request = header(&target);
        let page = target
            .lww_queue_unit_state_page(&request, after.as_ref(), 256)
            .unwrap();
        if !page.has_more {
            break;
        }
        after = page.after_key;
    }
    let entries = target.server_outbox_for_repair(authority).unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry.stamp.writer_id == original_issuer));
    assert!(entries
        .iter()
        .any(|entry| entry.stamp.writer_id == publisher));
    drain_publications(&c, &mut target, &[]).unwrap();
    let (_d, mut peer) = local();
    let d = new_target.client(&peer);
    receive(&d, &mut peer);
    assert_eq!(peer.read_root(None).unwrap().value["language"], "ja");
    assert_eq!(peer.read_root(None).unwrap().value["askRemoval"], false);
}

/// One synthetic issuer on the server side of a test.
struct ServerWriter {
    server: Arc<Store>,
    device: risunest_sync_server::store::Device,
    library_id: String,
}
impl ServerWriter {
    const WRITER: &'static str = "00000000-0000-4000-8000-0000000000aa";
    fn new(server: &Arc<Store>) -> Self {
        let credential = server.add_device().unwrap();
        Self {
            server: server.clone(),
            device: server
                .authenticate(&credential.library_id, &credential.token)
                .unwrap(),
            library_id: credential.library_id,
        }
    }
    fn push(&self, operation: &str, key: &UnitKey, physical: u64, value: UnitValue) {
        self.server
            .push(
                &self.device,
                &PushRequest {
                    library_id: self.library_id.clone(),
                    writer_id: Self::WRITER.into(),
                    operation_id: operation.into(),
                    changes: vec![UnitChange {
                        key: key.clone(),
                        stamp: Stamp {
                            physical_ms: physical.into(),
                            logical: 0,
                            writer_id: Self::WRITER.into(),
                        },
                        value,
                    }],
                },
            )
            .unwrap();
    }
}

#[test]
fn bootstrap_keeps_a_lower_stamped_retirement_from_the_journal_tail() {
    let key = UnitKey::new(&["exists", "character", "synthetic"]).unwrap();
    let writer: Arc<std::sync::OnceLock<ServerWriter>> = Arc::default();
    let retire = (writer.clone(), key.clone());
    let pinned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = LocalServerFixture::with_router(move |router| {
        router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let (writer, key) = retire.clone();
                let pinned = pinned.clone();
                async move {
                    let pin = request.method() == axum::http::Method::POST
                        && request.uri().path() == "/state/pins";
                    let response = next.run(request).await;
                    // The retirement lands after the pin copied the live
                    // existence, so only the journal tail carries it.
                    if pin && !pinned.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        tokio::task::spawn_blocking(move || {
                            writer
                                .get()
                                .unwrap()
                                .push("retire", &key, 20, UnitValue::Deleted)
                        })
                        .await
                        .unwrap();
                    }
                    response
                }
            },
        ))
    });
    assert!(writer.set(ServerWriter::new(&server.server)).is_ok());
    writer.get().unwrap().push(
        "create",
        &key,
        100,
        UnitValue::inline(br#"{"type":"character"}"#).unwrap(),
    );
    let (_root, mut store) = local();
    let client = server.client(&store);
    let upper = client.admission().unwrap();
    let (_, changes) = client.state(&mut store, upper).unwrap();
    let existence = changes.iter().find(|change| change.key == key).unwrap();
    assert_eq!(existence.value, UnitValue::Deleted);
    assert_eq!(existence.stamp.physical_ms.0, 20);
}

fn publication_ids(client: &LwwClient) -> Vec<String> {
    let mut query = client
        .log
        .0
        .prepare("SELECT id FROM publications ORDER BY rowid")
        .unwrap();
    query
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn settled_publications_leave_the_log_at_the_next_fence_and_detached_ones_stay() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let client = server.client(&store);
    let digest = "0".repeat(64);
    let detached = serde_json::json!({"kind":"auth-inactive-detached","authorizationId":"former","bodyDigest":digest,"historicalAcceptance":"unknown"});
    let rejected = OperationReceipt::Rejected {
        operation_id: "rejected".into(),
        body_digest: digest.clone(),
        error: "clock-skew".into(),
        server_time_ms: 1.into(),
    };
    for (id, receipt) in [
        ("detached", detached.to_string()),
        ("rejected", serde_json::to_string(&rejected).unwrap()),
    ] {
        client
            .log
            .0
            .execute(
                "INSERT INTO publications VALUES(?1,?2,x'7b7d','{}',?3,1)",
                rusqlite::params![id, digest, receipt],
            )
            .unwrap();
    }
    for index in 0..4 {
        save(
            &mut store,
            &["root", "language"],
            serde_json::json!(format!("synthetic-{index}")),
        );
        let receipt = publish_cycle(&client, &mut store, &[]).unwrap().unwrap();
        assert_eq!(
            publication_ids(&client),
            ["detached".to_string(), receipt.operation_id]
        );
    }
}

#[test]
fn clock_retry_does_not_decode_publications_of_another_authority() {
    let server = LocalServerFixture::new();
    let (_root, mut store) = local();
    let client = server.client(&store);
    let other = store.lww_binding_authority().unwrap().0 + 1;
    client
        .log
        .0
        .execute(
            "INSERT INTO publications VALUES('other',?1,x'7b7d',?2,'{}',1)",
            rusqlite::params![
                "0".repeat(64),
                serde_json::json!({"authority": other.to_string()}).to_string()
            ],
        )
        .unwrap();
    let request = header(&store);
    let result = client.retry_unpublished(&mut store, &request).unwrap();
    assert!(result.affected_keys.is_empty());
    assert_eq!(publication_ids(&client), ["other"]);
}
