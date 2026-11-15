use super::*;
use super::super::tests::{frame_sets, fixture_text};
use risunest_ir::Limits;
use serde_json::{json, Value};
use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}};
use tauri::{Listener, test::MockRuntime};

pub(super) fn budget(owned: usize, work: usize, cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: owned, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: work }, cancel).unwrap()
}
pub(super) fn ample() -> Budget { budget(32 << 20, 100_000_000, Arc::new(AtomicBool::new(false))) }
pub(super) fn text(value: &str, b: &Budget) -> Text { Text::from_str(value, b).unwrap() }
pub(super) fn target(b: &Budget) -> EmbeddingTarget {
    EmbeddingTarget::capture(&text("custom", b), None, &text("https://cache.invalid/v1", b), &text("synthetic", b), b).unwrap()
}
pub(super) fn key(value: u8) -> CacheKey { CacheKey([value; 32]) }
pub(super) fn key_name(key: CacheKey) -> String { String::from_utf8(key.hex().to_vec()).unwrap() }
pub(super) fn writes(keys: &[Option<CacheKey>], b: &Budget) -> Vec<CacheWrite> {
    keys.iter().map(|key| { let mut bits = OwnedVec::new(b).unwrap(); bits.extend(&[1f32.to_bits(), 0x80000000, 0x7fc00001]).unwrap(); CacheWrite { key: *key, bits } }).collect()
}
pub(super) fn cache_ok<T>(result: Result<T, CacheError>) -> T {
    match result { Ok(value) => value, Err(CacheError::Unavailable) => panic!("cache unavailable"),
        Err(CacheError::Terminal(error)) => panic!("terminal cache error: {error:?}") }
}
pub(super) struct Fixture {
    pub directory: tempfile::TempDir,
    pub state: Arc<PersistentStoreState>,
    pub notifications: Arc<AtomicUsize>,
    _app: tauri::App<MockRuntime>,
    notify: Arc<dyn Fn() + Send + Sync>,
}
impl Fixture {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(PersistentStoreState::with_test_store(PersistentStore::open(directory.path()).unwrap()));
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        let notifications = Arc::new(AtomicUsize::new(0));
        let count = notifications.clone();
        let observed_state = state.clone();
        app.listen(crate::server_sync::events::DEVICE_CHANGED_EVENT, move |_| {
            assert_eq!(observed_state.renderer_gate.state.lock().unwrap().operations, 0, "notify retained store admission");
            let operation = observed_state.admit_native_job_operation().unwrap();
            let store = observed_state.open_admitted_native_job_store(&operation).unwrap();
            assert!(store.device_store().unwrap().revision().unwrap() > 0);
            count.fetch_add(1, Ordering::SeqCst);
        });
        let handle = app.handle().clone();
        let notify = Arc::new(move || crate::server_sync::events::notify_device_changed(&handle));
        Self { directory, state, notifications, _app: app, notify }
    }
    pub fn cache(&self) -> NativeCache { NativeCache::new(self.state.clone(), self.notify.clone()) }
    pub fn store<T>(&self, work: impl FnOnce(&mut PersistentStore) -> T) -> T {
        let operation = self.state.admit_native_job_operation().unwrap();
        let mut store = self.state.open_admitted_native_job_store(&operation).unwrap();
        work(&mut store)
    }
    pub fn raw(&self, key: CacheKey, dimensions: i64, vector: &[u8], tombstone: bool) {
        self.store(|store| store.device_store().unwrap().connection().execute(
            "INSERT INTO hypa_embeddings(cache_key,producer,model,preprocess_version,dimensions,vector,tombstone,write_clock,writer_id)
             VALUES(?1,'hypa-v1-text','synthetic',1,?2,?3,?4,'0','') ON CONFLICT(cache_key) DO UPDATE SET dimensions=?2,vector=?3,tombstone=?4",
            rusqlite::params![key_name(key), dimensions, vector, tombstone],
        ).unwrap());
    }
    pub fn snapshot(&self) -> Value {
        self.store(|store| {
            let device = store.device_store().unwrap();
            let rows = |sql| device.connection().prepare(sql).unwrap().query_map([], |row| row.get::<_, String>(0)).unwrap()
                .collect::<Result<Vec<_>, _>>().unwrap();
            json!({
                "revision": device.revision().unwrap(),
                "rows": rows("SELECT json_array(cache_key,dimensions,hex(vector),write_clock,writer_id,tombstone,published_clock,first_published_generation,first_published_at_ms) FROM hypa_embeddings ORDER BY cache_key"),
                "changes": rows("SELECT json_array(section,key1,key2,key3,revision) FROM device_changes ORDER BY revision,key1"),
                "units": rows("SELECT json_array(key,stamp,value,version,identity) FROM lww_units ORDER BY key"),
                "outbox": rows("SELECT json_array(key,stamp,value,version,identity,authority) FROM lww_outbox ORDER BY key"),
            })
        })
    }
}

fn fixture_key(value: &Value) -> CacheKey { CacheKey(hex::decode(value.as_str().unwrap()).unwrap().try_into().unwrap()) }

#[test]
fn native_reads_match_actual_ts_frames_and_preserve_ordered_optional_rows() {
    let (cases, expected) = frame_sets();
    for (case, observed) in cases["cases"].as_array().unwrap().iter().zip(expected["cases"].as_array().unwrap()) {
        assert_eq!(case["id"], observed["id"]);
        let input = &case["input"];
        if input["mode"] != "read" { continue; }
        let fixture = Fixture::new();
        for row in input["rows"].as_array().unwrap() {
            fixture.raw(fixture_key(&row["key"]), row["dimensions"].as_i64().unwrap(), &hex::decode(row["bytesHex"].as_str().unwrap()).unwrap(), false);
        }
        let keys = input["keys"].as_array().unwrap().iter().map(fixture_key).collect::<Vec<_>>();
        let result = fixture.cache().read_batch(&keys, &ample());
        let result = if observed["result"]["readStatus"] == "unavailable" {
            assert!(matches!(result, Err(CacheError::Unavailable)), "{}", case["id"]);
            Vec::new()
        } else {
            let rows = cache_ok(result);
            assert_eq!(rows.as_slice().len(), keys.len());
            let mut unique = Vec::<Value>::new();
            for (key, row) in keys.iter().zip(rows.as_slice()) {
                if let Some(row) = row {
                    assert!(row.key == *key);
                    let value = json!({"key":key_name(row.key),"dimensions":row.dimensions,"bits":row.bits.as_slice()});
                    if let Some(old) = unique.iter_mut().find(|v| v["key"] == value["key"]) { *old = value; }
                    else { unique.push(value); }
                }
            }
            unique
        };
        assert_eq!(json!(result), observed["result"]["hits"], "{}", case["id"]);
        assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn tombstones_null_empty_and_unusual_old_dimensions_keep_native_read_contract() {
    let fixture = Fixture::new();
    fixture.raw(key(1), 1, &[0, 0, 0, 128], false);
    fixture.raw(key(2), 1, &[0, 0, 128, 63], true);
    fixture.raw(key(3), 1, &[], false);
    fixture.raw(key(4), 16_385, &[255], false);
    fixture.raw(key(5), 1, &[1], false);
    fixture.store(|store| { store.device_store().unwrap().connection().execute(
        "UPDATE hypa_embeddings SET vector=NULL WHERE cache_key=?1", [key_name(key(5))]).unwrap(); });
    let b = ample();
    let before = b.owned_bytes();
    let rows = cache_ok(fixture.cache().read_batch(&[key(1), key(2), key(3), key(4), key(5), key(0), key(1)], &b));
    assert_eq!(rows.as_slice().len(), 7);
    assert_eq!(rows.as_slice()[0].as_ref().unwrap().bits.as_slice(), &[0x80000000]);
    for index in [1, 2, 4, 5] { assert!(rows.as_slice()[index].is_none()); }
    let large = rows.as_slice()[3].as_ref().unwrap();
    assert_eq!(large.bits.as_slice().len(), 16_385);
    assert_eq!(large.bits.as_slice()[0], 255);
    assert!(large.bits.as_slice()[1..].iter().all(|v| *v == 0));
    assert_eq!(rows.as_slice()[6].as_ref().unwrap().bits.as_slice(), &[0x80000000]);
    assert!(b.owned_bytes() > before);
    drop(rows);
    assert_eq!(b.owned_bytes(), before);
}

#[test]
fn read_budget_denial_precedes_blob_copy_and_releases_outputs_on_error() {
    let fixture = Fixture::new();
    let size = 256 * 1024;
    fixture.raw(key(1), (size / 4) as i64, &vec![0x33; size], false);
    let b = budget(Budget::context_bytes() + 1024, 10_000_000, Arc::new(AtomicBool::new(false)));
    let before = b.owned_bytes();
    let (result, allocations) = allocation_observer::allocations_at_least(size, || fixture.cache().read_batch(&[key(1)], &b));
    assert!(matches!(result, Err(CacheError::Terminal(StageError::Limit(_)))));
    assert_eq!(allocations, 0, "a full blob/output allocation preceded admission");
    assert_eq!(b.owned_bytes(), before);
    let b = ample();
    let (rows, allocations) = allocation_observer::allocations_at_least(size, || cache_ok(fixture.cache().read_batch(&[key(1)], &b)));
    assert!(allocations > 0, "the allocation observer must see the admitted output");
    assert_eq!(rows.as_slice()[0].as_ref().unwrap().bits.as_slice()[0], 0x33333333);
    drop(rows);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
    fixture.raw(key(2), i64::MAX, &[1], false);
    assert!(matches!(fixture.cache().read_batch(&[key(1), key(2)], &b), Err(CacheError::Terminal(StageError::Limit(_)))));
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
}

#[test]
fn native_writes_preserve_bits_revision_lww_publication_and_reopen_durability() {
    let fixture = Fixture::new();
    let b = ample();
    let target = target(&b);
    let entries = writes(&[Some(key(1)), Some(key(2))], &b);
    cache_ok(fixture.cache().write_batch(&target, &entries, &b));
    let first = fixture.snapshot();
    assert_eq!(first["revision"], 1);
    assert_eq!(first["units"].as_array().unwrap().len(), 2);
    let unit0: Value = serde_json::from_str(first["units"][0].as_str().unwrap()).unwrap();
    let unit1: Value = serde_json::from_str(first["units"][1].as_str().unwrap()).unwrap();
    assert_eq!(unit0[1], unit1[1]);
    for row in first["rows"].as_array().unwrap() {
        let row: Value = serde_json::from_str(row.as_str().unwrap()).unwrap();
        assert_eq!(row[2], "0000803F000000800100C07F");
        assert_eq!(row[3], "1");
    }
    fixture.store(|store| { store.device_store().unwrap().connection().execute_batch(
        "UPDATE hypa_embeddings SET published_clock='1',first_published_generation='1',first_published_at_ms=1").unwrap(); });
    cache_ok(fixture.cache().write_batch(&target, &entries, &b));
    let equal = fixture.snapshot();
    assert_eq!(equal["revision"], 2);
    assert_eq!(equal["units"], first["units"]);
    assert_eq!(equal["outbox"], first["outbox"]);
    for row in equal["rows"].as_array().unwrap() {
        let row: Value = serde_json::from_str(row.as_str().unwrap()).unwrap();
        assert_eq!(row[3], "2");
        assert!(row[6].is_null() && row[7].is_null() && row[8].is_null());
    }
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 2);
    let reopened = PersistentStore::open(fixture.directory.path()).unwrap();
    let rows = reopened.device_store().unwrap().read_hypa_embeddings(&[key_name(key(2)), key_name(key(1))]).unwrap();
    assert_eq!(rows[0].vector, rows[1].vector);
    assert_eq!(rows[0].vector.as_deref(), Some(&[0,0,128,63,0,0,0,128,1,0,192,127][..]));
}

#[test]
fn invalid_batches_and_budget_denial_do_not_mutate_or_notify() {
    let fixture = Fixture::new();
    let b = ample();
    let target = target(&b);
    let good = writes(&[Some(key(1))], &b);
    cache_ok(fixture.cache().write_batch(&target, &good, &b));
    let before = fixture.snapshot();
    for entries in [
        writes(&[Some(key(2)), None], &b),
        vec![CacheWrite { key:Some(key(2)), bits: OwnedVec::new(&b).unwrap() }],
    ] {
        assert!(matches!(fixture.cache().write_batch(&target, &entries, &b), Err(CacheError::Unavailable)));
    }
    let mut oversized = OwnedVec::new(&b).unwrap();
    for _ in 0..=MAX_DIMENSIONS { oversized.push(1).unwrap(); }
    assert!(matches!(fixture.cache().write_batch(&target, &[CacheWrite { key:Some(key(2)), bits:oversized }], &b), Err(CacheError::Unavailable)));
    cache_ok(fixture.cache().write_batch(&target, &[], &b));
    let low = budget(Budget::context_bytes() + 8, 100_000, Arc::new(AtomicBool::new(false)));
    assert!(matches!(fixture.cache().write_batch(&target, &good, &low), Err(CacheError::Terminal(StageError::Limit(_)))));
    assert_eq!(low.owned_bytes(), Budget::context_bytes());
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
}

#[test]
fn sqlite_failure_rolls_back_the_current_batch_and_preserves_earlier_commits() {
    let fixture = Fixture::new();
    let b = ample();
    let target = target(&b);
    cache_ok(fixture.cache().write_batch(&target, &writes(&[Some(key(1))], &b), &b));
    let mut cache = fixture.cache();
    cache.opened = Some(Arc::new(|store| { store.device_store().unwrap().connection().execute_batch(&format!(
        "CREATE TEMP TRIGGER synthetic_hypa_failure BEFORE INSERT ON main.hypa_embeddings WHEN NEW.cache_key='{}' BEGIN SELECT RAISE(ABORT,'synthetic cache write failure'); END;",
        key_name(key(3)))).unwrap(); }));
    let before = fixture.snapshot();
    assert!(matches!(cache.write_batch(&target, &writes(&[Some(key(2)), Some(key(3))], &b), &b), Err(CacheError::Unavailable)));
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_before_mutation_and_after_commit_have_distinct_durable_outcomes() {
    let fixture = Fixture::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let b = budget(32 << 20, 10_000_000, cancel.clone());
    let target = target(&b);
    let entries = writes(&[Some(key(1))], &b);
    let initial = fixture.snapshot();
    cancel.store(true, Ordering::SeqCst);
    assert!(matches!(fixture.cache().write_batch(&target, &entries, &b), Err(CacheError::Terminal(StageError::Cancelled))));
    assert_eq!(fixture.snapshot(), initial);
    cancel.store(false, Ordering::SeqCst);
    let mut cache = fixture.cache();
    cache.committed = Some(Arc::new(move || cancel.store(true, Ordering::SeqCst)));
    assert!(matches!(cache.write_batch(&target, &entries, &b), Err(CacheError::Terminal(StageError::Cancelled))));
    assert_eq!(fixture.snapshot()["revision"], 1);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
}

#[test]
fn strict_metadata_and_exact_frame_size_rejections_are_atomic() {
    let fixture = Fixture::new();
    let b = ample();
    let entries = writes(&[Some(key(1))], &b);
    for units in [&[0xd800][..], &[0xdc00][..]] {
        let invalid = Text::from_units(units, &b).unwrap();
        for endpoint_invalid in [false, true] {
            let ordinary_endpoint = text("https://cache.invalid", &b);
            let ordinary_model = text("synthetic", &b);
            let target = EmbeddingTarget::capture(&text("custom", &b), None,
                if endpoint_invalid { &invalid } else { &ordinary_endpoint },
                if endpoint_invalid { &ordinary_model } else { &invalid }, &b).unwrap();
            assert!(matches!(fixture.cache().write_batch(&target, &entries, &b), Err(CacheError::Unavailable)));
        }
    }
    let target = target(&b);
    let (model, endpoint) = target.identity();
    let exact = write_frame_length(model.as_unicode().unwrap(), endpoint.as_unicode(), &entries, &b).unwrap_or_else(|_| panic!());
    let mut cache = fixture.cache();
    cache.frame_limit = exact - 1;
    assert!(matches!(cache.write_batch(&target, &entries, &b), Err(CacheError::Unavailable)));
    assert_eq!(fixture.snapshot()["revision"], 0);
    cache.frame_limit = exact;
    cache_ok(cache.write_batch(&target, &entries, &b));
    assert_eq!(fixture.snapshot()["revision"], 1);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
}

#[test]
fn counted_header_sizes_match_real_ts_encoder_without_materializing_frames() {
    let (cases, expected) = frame_sets();
    let b = ample();
    let mut checked = 0;
    for (case, observed) in cases["cases"].as_array().unwrap().iter().zip(expected["cases"].as_array().unwrap()) {
        if case["input"]["mode"] != "write" { continue; }
        let entries = case["input"]["entries"].as_array().unwrap();
        let Some(first) = entries.first() else { continue; };
        if !entries.iter().all(|entry| entry["model"] == first["model"] && entry["endpoint"] == first["endpoint"]) { continue; }
        let Ok(model) = fixture_text(&first["model"]) else { continue; };
        let endpoint = if first["endpoint"].is_null() { None } else {
            let Ok(value) = fixture_text(&first["endpoint"]) else { continue; };
            Some(value)
        };
        let entries = entries.iter().map(|entry| {
            let mut bits = OwnedVec::new(&b).unwrap();
            for value in entry["bits"].as_array().unwrap() { bits.push(value.as_u64().unwrap() as u32).unwrap(); }
            CacheWrite { key: Some(fixture_key(&entry["key"])), bits }
        }).collect::<Vec<_>>();
        assert_eq!(write_frame_length(&model, endpoint.as_deref(), &entries, &b).unwrap_or_else(|_| panic!()) as u64,
            observed["result"]["frameBytes"].as_u64().unwrap(), "{}", case["id"]);
        checked += 1;
    }
    assert!(checked >= 5);
}

#[test]
fn native_dependency_graph_keeps_frozen_unicode16_key_observations() {
    use sha2::{Digest, Sha256};
    let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/memory/hypa-v1/observations.cases.json"));
    let cases: Value = serde_json::from_slice(bytes).unwrap();
    let expected: Value = serde_json::from_slice(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/memory/hypa-v1/observations.expected.json"))).unwrap();
    assert_eq!(expected["fixtureSha256"], hex::encode(Sha256::digest(bytes)));
    let b = ample();
    for id in ["unicode-nfc-key-conformance", "unicode17-combining-class-barrier-ts16"] {
        let case = cases["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap();
        let expected = expected["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap();
        let input = &case["input"];
        let config = &input["target"];
        let target = EmbeddingTarget::capture(&text(config["model"].as_str().unwrap(), &b), None,
            &text(config["endpoint"].as_str().unwrap_or_default(), &b), &text(config["customModel"].as_str().unwrap_or_default(), &b), &b).unwrap();
        let keys = input["documents"].as_array().unwrap().iter().map(|value| {
            let document = if let Some(value) = value.as_str() { text(value, &b) } else {
                Text::from_units(&value["u"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u16).collect::<Vec<_>>(), &b).unwrap()
            };
            key_name(target.cache_key(&document, &b).unwrap())
        }).collect::<Vec<_>>();
        assert_eq!(json!(keys), expected["result"]["outcome"]["keys"], "{id}");
    }
}

#[test]
fn read_and_write_work_limits_reach_vector_conversion_and_remain_terminal() {
    let fixture = Fixture::new();
    let dimensions = 256usize;
    fixture.raw(key(1), dimensions as i64, &vec![0; dimensions * 4], false);
    let read_work = 64 + 1 + dimensions * 5;
    let low = budget(32 << 20, read_work - 1, Arc::new(AtomicBool::new(false)));
    let before = fixture.snapshot();
    assert!(matches!(fixture.cache().read_batch(&[key(1)], &low), Err(CacheError::Terminal(StageError::Limit(kind))) if kind == "work"));
    assert_eq!(low.owned_bytes(), Budget::context_bytes());
    let high = budget(32 << 20, read_work, Arc::new(AtomicBool::new(false)));
    drop(cache_ok(fixture.cache().read_batch(&[key(1)], &high)));
    assert_eq!(high.owned_bytes(), Budget::context_bytes());

    let source = ample();
    let target = target(&source);
    let mut bits = OwnedVec::new(&source).unwrap();
    for _ in 0..dimensions { bits.push(1f32.to_bits()).unwrap(); }
    let entries = [CacheWrite { key: Some(key(2)), bits }];
    let (model, endpoint) = target.identity();
    let prefix = 2 + 4 * model.as_unicode().unwrap().len() + 4 * endpoint.as_unicode().unwrap().len() + 64 + b"hypa-v1-text".len();
    let low = budget(32 << 20, prefix + dimensions * 4 - 1, Arc::new(AtomicBool::new(false)));
    assert!(matches!(fixture.cache().write_batch(&target, &entries, &low), Err(CacheError::Terminal(StageError::Limit(kind))) if kind == "work"));
    assert_eq!(low.owned_bytes(), Budget::context_bytes());
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
    let high = budget(32 << 20, prefix + dimensions * 4, Arc::new(AtomicBool::new(false)));
    cache_ok(fixture.cache().write_batch(&target, &entries, &high));
    assert_eq!(high.owned_bytes(), Budget::context_bytes());
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 1);
}

#[test]
fn write_budget_denial_precedes_raw_vector_scratch_allocation() {
    let fixture = Fixture::new();
    let b = budget(100_000, 10_000_000, Arc::new(AtomicBool::new(false)));
    let target = target(&b);
    let mut bits = OwnedVec::new(&b).unwrap();
    for _ in 0..MAX_DIMENSIONS { bits.push(1f32.to_bits()).unwrap(); }
    let entries = [CacheWrite { key: Some(key(1)), bits }];
    let before = b.owned_bytes();
    let snapshot = fixture.snapshot();
    let (result, allocations) = allocation_observer::allocations_at_least(MAX_DIMENSIONS as usize * 4, || fixture.cache().write_batch(&target, &entries, &b));
    assert!(matches!(result, Err(CacheError::Terminal(StageError::Limit(_)))));
    assert_eq!(allocations, 0, "raw write scratch allocated before its budget admission");
    assert_eq!(b.owned_bytes(), before);
    assert_eq!(fixture.snapshot(), snapshot);
    assert_eq!(fixture.notifications.load(Ordering::SeqCst), 0);
    let high = ample();
    let (result, allocations) = allocation_observer::allocations_at_least(MAX_DIMENSIONS as usize * 4, || fixture.cache().write_batch(&target, &entries, &high));
    cache_ok(result);
    assert!(allocations > 0);
    assert_eq!(high.owned_bytes(), Budget::context_bytes());
    drop(entries);
    drop(target);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
}
