use super::*;
use crate::persistent_store::device_store::plugin_values::PluginDeviceMutation;
use crate::persistent_store::server_sync_sections as server;
use risunest_sync_wire::Domain;

fn plugin(clock: &str, writer: &str) -> SectionRow {
    SectionRow {
        key1: "plugin".into(), key2: "string".into(), key3: "key".into(),
        value: SectionValueRow::Plugin { space: "string".into(), value: "value".into() },
        write_clock: Sequence::try_from(clock.to_owned()).unwrap(), writer_id: writer.into(),
    }
}

fn local_entry(row: &SectionRow) -> LocalSectionEntry {
    LocalSectionEntry {
        version: row.version(), entry: row.to_entry(SectionKind::LocalPlugins, true).unwrap(),
        object: None, published: true,
    }
}

#[test]
fn pending_plugin_keys_page_by_tuple_with_escaped_identifiers_and_indexed_status() {
    let directory = tempfile::tempdir().unwrap();
    let mut device = DeviceStore::open(directory.path()).unwrap();
    let keys = ["a", "a\"", "a\\", "a/", "b"];
    for key in keys {
        device.write_plugin_device_values("plugin", &[PluginDeviceMutation::Set {
            space: "string".into(), key: key.into(), value: "synthetic".into(),
        }]).unwrap();
    }
    let mut after = String::new();
    let mut collected = std::collections::BTreeSet::new();
    loop {
        let page = device.pending_section_entry_keys(Section::LocalPlugins, &after, 2).unwrap();
        if page.is_empty() { break; }
        for key in &page { assert!(collected.insert(key.clone()), "duplicate pending key"); }
        after = page.last().unwrap().clone();
    }
    assert_eq!(collected.len(), keys.len());
    for (table, index) in [("plugin_device_storage", "plugin_device_storage_pending"), ("hypa_embeddings", "hypa_embeddings_pending")] {
        let plan: Vec<String> = device.connection.prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT EXISTS(SELECT 1 FROM {table} WHERE published_clock IS NULL OR published_clock<>write_clock)"
        )).unwrap().query_map([], |row| row.get(3)).unwrap().collect::<Result<_,_>>().unwrap();
        assert!(plan.iter().any(|line| line.contains(index)), "{plan:?}");
    }
}

#[test]
#[ignore = "synthetic 20k embedding pending-status measurement"]
fn pending_status_vm_steps_stay_constant_with_20k_published_embeddings() {
    use crate::persistent_store::device_store::hypa::HypaEmbeddingWrite;
    use risunest_external_storage_format::section::hypa_entry_key;
    use rusqlite::StatementStatus;

    let directory = tempfile::tempdir().unwrap();
    let mut device = DeviceStore::open(directory.path()).unwrap();
    device.set_section_participating(Section::Hypa, true).unwrap();
    let sql = "SELECT EXISTS(SELECT 1 FROM hypa_embeddings WHERE published_clock IS NULL OR published_clock<>write_clock)";
    let measure = |device: &DeviceStore| {
        let mut statement = device.connection.prepare(sql).unwrap();
        let pending: bool = statement.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(pending, device.has_pending_section_entries(Section::Hypa).unwrap());
        let steps = statement.get_status(StatementStatus::VmStep);
        assert!(steps > 0);
        (pending, steps)
    };
    let baseline = measure(&device);
    assert!(!baseline.0);
    for start in (0..20_000).step_by(512) {
        let end = (start + 512).min(20_000);
        let entries: Vec<_> = (start..end).map(|index| HypaEmbeddingWrite {
            cache_key: format!("{index:064x}"), producer: "synthetic".into(),
            model: "synthetic-1536".into(), endpoint: None, preprocess_version: 1,
            dimensions: 1536, vector: 1.0f32.to_le_bytes().repeat(1536), metadata: None,
        }).collect();
        device.write_hypa_embeddings(&entries).unwrap();
        let published: Vec<_> = entries.iter().map(|entry| server::SectionWrite::Mark {
            domain: Domain::Hypa, key: hypa_entry_key(&entry.cache_key).unwrap(),
        }).collect();
        server::write_sections(&mut device, &published).unwrap();
    }
    let shape: (i64, i64, i64) = device.connection.query_row(
        "SELECT count(*),min(dimensions),sum(length(vector)) FROM hypa_embeddings",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(shape, (20_000, 1536, 20_000 * 1536 * 4));
    assert!(!server::has_pending(&device).unwrap());
    assert!(device.pending_section_entry_keys(Section::Hypa, "", 256).unwrap().is_empty());
    let full = measure(&device);
    assert!(!full.0);
    assert_eq!(full.1, baseline.1, "all-published status must not visit embedding rows");
    let plan: Vec<String> = device.connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap().query_map([], |row| row.get(3)).unwrap().collect::<Result<_,_>>().unwrap();
    assert!(plan.iter().any(|line| line.contains("hypa_embeddings_pending")), "{plan:?}");
    eprintln!("synthetic pending-status: rows=20000 dimensions=1536 baseline_vm_steps={} full_vm_steps={} plan={plan:?}", baseline.1, full.1);
}

#[test]
#[ignore = "synthetic 10k plugin pending-page and publication measurement"]
fn pending_plugin_scale_pages_complete_once_and_keep_writes_after_the_cursor() {
    use risunest_external_storage_format::section::{decode_local_plugin_entry_key, local_plugin_entry_key};
    use rusqlite::StatementStatus;
    use std::collections::BTreeSet;

    let directory = tempfile::tempdir().unwrap();
    let mut device = DeviceStore::open(directory.path()).unwrap();
    device.set_section_participating(Section::LocalPlugins, true).unwrap();
    let raw_keys: Vec<_> = (0..10_000).map(|index| format!("key-{index:05}{}",
        match index % 4 { 0 => "\"", 1 => "\\", 2 => "/", _ => "plain" })).collect();
    for batch in raw_keys.chunks(512) {
        let writes: Vec<_> = batch.iter().map(|key| PluginDeviceMutation::Set {
            space: "string".into(), key: key.clone(), value: "synthetic".into(),
        }).collect();
        device.write_plugin_device_values("plugin", &writes).unwrap();
    }
    let expected: BTreeSet<_> = raw_keys.iter().map(|key|
        local_plugin_entry_key("plugin", "string", key).unwrap()).collect();
    assert_eq!(expected.len(), 10_000);
    let first_sql = "SELECT owner,space,key FROM plugin_device_storage WHERE (published_clock IS NULL OR published_clock<>write_clock) ORDER BY owner,space,key LIMIT ?4";
    let next_sql = "SELECT owner,space,key FROM plugin_device_storage WHERE (published_clock IS NULL OR published_clock<>write_clock) AND (owner,space,key)>(?1,?2,?3) ORDER BY owner,space,key LIMIT ?4";
    let plan: Vec<String> = device.connection.prepare(&format!("EXPLAIN QUERY PLAN {next_sql}"))
        .unwrap().query_map(rusqlite::params!["plugin", "string", "", 256], |row| row.get(3))
        .unwrap().collect::<Result<_,_>>().unwrap();
    assert!(plan.iter().any(|line| line.contains("plugin_device_storage_pending")), "{plan:?}");
    let mut after = String::new();
    let mut collected = BTreeSet::new();
    let mut pages = 0;
    let mut total_steps = 0;
    let mut max_steps = 0;
    let mut rewritten = None;
    loop {
        let page = device.pending_section_entry_keys(Section::LocalPlugins, &after, 256).unwrap();
        let lower = if after.is_empty() { (String::new(), String::new(), String::new()) }
            else { decode_local_plugin_entry_key(&after).unwrap() };
        let mut statement = device.connection.prepare(if after.is_empty() { first_sql } else { next_sql }).unwrap();
        let measured: Vec<_> = statement.query_map(rusqlite::params![lower.0, lower.1, lower.2, 256], |row|
            Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?)))
            .unwrap().collect::<Result<Vec<_>,_>>().unwrap().iter().map(|(owner, space, key)|
                local_plugin_entry_key(owner, space, key).unwrap()).collect();
        assert_eq!(measured, page);
        let steps = statement.get_status(StatementStatus::VmStep);
        total_steps += steps;
        max_steps = max_steps.max(steps);
        drop(statement);
        if page.is_empty() { break; }
        pages += 1;
        assert!(page.len() <= 256);
        assert!(pages <= 40, "pending pages did not advance");
        for key in &page { assert!(collected.insert(key.clone()), "duplicate pending key"); }
        let versions: Vec<_> = page.iter().map(|key| server::SectionWrite::MarkVersion {
            domain: Domain::LocalPlugins, key: key.clone(),
            version: device.read_section_entry(Section::LocalPlugins, key).unwrap().unwrap().version,
        }).collect();
        if rewritten.is_none() {
            let key = page.first().unwrap().clone();
            let (_, space, raw) = decode_local_plugin_entry_key(&key).unwrap();
            device.write_plugin_device_values("plugin", &[PluginDeviceMutation::Set {
                space, key: raw, value: "synthetic-newer".into(),
            }]).unwrap();
            rewritten = Some(key);
        }
        server::write_sections(&mut device, &versions).unwrap();
        after = page.last().unwrap().clone();
    }
    assert_eq!(collected, expected);
    assert_eq!(pages, 40);
    assert!(max_steps < 256 * 64 + 128, "a page must not scan the pending tail: {max_steps}");
    assert!(total_steps < 10_000 * 64 + 41 * 128, "page work must remain bounded by visited keys: {total_steps}");
    let rewritten = rewritten.unwrap();
    assert!(server::has_pending(&device).unwrap());
    assert_eq!(device.pending_section_entry_keys(Section::LocalPlugins, "", 256).unwrap(), vec![rewritten.clone()]);
    let latest = device.read_section_entry(Section::LocalPlugins, &rewritten).unwrap().unwrap();
    assert!(!latest.published);
    server::write_sections(&mut device, &[server::SectionWrite::MarkVersion {
        domain: Domain::LocalPlugins, key: rewritten, version: latest.version,
    }]).unwrap();
    assert!(!server::has_pending(&device).unwrap());
    assert!(device.pending_section_entry_keys(Section::LocalPlugins, "", 256).unwrap().is_empty());
    eprintln!("synthetic plugin pending-pages: keys=10000 pages={pages} total_vm_steps={total_steps} max_page_vm_steps={max_steps} plan={plan:?}");
}

fn assert_both(local: &SectionRow, incoming: &SectionRow, decision: SectionMergeDecision, outcome: server::Outcome) {
    assert_eq!(resolve_section_row(Some(local), incoming).unwrap(), decision);
    assert_eq!(server::resolve(Some(&local_entry(local)), &local_entry(incoming).entry).unwrap(), outcome);
}

#[test]
fn e2_both_adapters_compare_large_clocks_and_writer_ties_exactly() {
    let local = plugin("9007199254740992", "writer-z");
    let newer = plugin("9007199254740993", "writer-a");
    assert_both(&local, &newer, SectionMergeDecision::ApplyIncoming, server::Outcome::Apply);
    assert_both(&newer, &local, SectionMergeDecision::PublishLocal, server::Outcome::Publish);
    let same_clock = plugin("9007199254740993", "writer-b");
    assert_both(&newer, &same_clock, SectionMergeDecision::ApplyIncoming, server::Outcome::Apply);
    assert_both(&same_clock, &same_clock, SectionMergeDecision::Settled, server::Outcome::Settled);
}

#[test]
fn e2_a_newer_version_never_excuses_a_different_key_or_kind() {
    let local = plugin("1", "writer");
    let mut incoming = plugin("2", "writer");
    incoming.key3 = "other".into();
    assert!(resolve_section_row(Some(&local), &incoming).is_err());
    assert!(server::resolve(Some(&local_entry(&local)), &local_entry(&incoming).entry).is_err());
    let mut incoming = local_entry(&local).entry;
    incoming.kind = SectionKind::Hypa;
    assert!(server::resolve(None, &incoming).is_err());
    incoming = local_entry(&local).entry;
    incoming.version = None;
    assert!(server::resolve(None, &incoming).is_err());
}

#[test]
fn e2_same_version_different_values_are_rejected_by_both_adapters() {
    let local = plugin("7", "writer");
    let mut incoming = local.clone();
    incoming.value = SectionValueRow::Plugin { space: "string".into(), value: "other".into() };
    assert!(resolve_section_row(Some(&local), &incoming).is_err());
    assert!(server::resolve(Some(&local_entry(&local)), &local_entry(&incoming).entry).is_err());
}

#[test]
fn e2_removal_markers_converge_for_both_adapters() {
    for (generation, at_ms) in [(3, 500), (7, 99), (7, 100), (9, 1)] {
        let mut local = plugin("42", "writer");
        local.value = SectionValueRow::Tombstone { first_published: Some(TombstonePublication {
            generation: Sequence::from(7u64), at_ms: 100,
        }) };
        let mut incoming = local.clone();
        incoming.value = SectionValueRow::Tombstone { first_published: Some(TombstonePublication {
            generation: Sequence::from(generation as u64), at_ms,
        }) };
        let expected = match (generation, at_ms).cmp(&(7, 100)) {
            std::cmp::Ordering::Less => server::Outcome::Apply,
            std::cmp::Ordering::Equal => server::Outcome::Settled,
            std::cmp::Ordering::Greater => server::Outcome::Publish,
        };
        assert_eq!(server::resolve(Some(&local_entry(&local)), &local_entry(&incoming).entry).unwrap(), expected);
        let directory = tempfile::tempdir().unwrap();
        let mut device = DeviceStore::open(directory.path()).unwrap();
        device.apply_section_rows(Section::LocalPlugins, &[local.clone()]).unwrap();
        device.apply_section_rows(Section::LocalPlugins, &[incoming.clone()]).unwrap();
        let held = device.read_section_rows(Section::LocalPlugins).unwrap().remove(0);
        let expected_marker = if (generation, at_ms) < (7, 100) { incoming.value.first_published() } else { local.value.first_published() };
        assert_eq!(held.value.first_published(), expected_marker);
    }
    let mut bare = plugin("42", "writer");
    bare.value = SectionValueRow::Tombstone { first_published: None };
    let marker = TombstonePublication { generation: Sequence::from(7u64), at_ms: 100 };
    let mut marked = bare.clone();
    marked.value = SectionValueRow::Tombstone { first_published: Some(marker.clone()) };
    assert_eq!(resolve_section_row(Some(&bare), &marked).unwrap(), SectionMergeDecision::MergeRemovalMarker {
        marker: Some(marker.clone()), write_local: true, publish: false,
    });
    assert_eq!(resolve_section_row(Some(&marked), &bare).unwrap(), SectionMergeDecision::MergeRemovalMarker {
        marker: Some(marker), write_local: false, publish: true,
    });
}

#[test]
fn e2_json_and_vector_representations_have_one_content_rule() {
    let mut local = plugin("5", "writer");
    local.key2 = "json".into();
    local.value = SectionValueRow::Plugin { space: "json".into(), value: "{\"b\":2, \"a\":1}".into() };
    let mut incoming = local.clone();
    incoming.value = SectionValueRow::Plugin { space: "json".into(), value: "{\"a\":1,\"b\":2}".into() };
    assert_both(&local, &incoming, SectionMergeDecision::Settled, server::Outcome::Settled);

    let row = SectionRow {
        key1: "a".repeat(64), key2: String::new(), key3: String::new(),
        value: SectionValueRow::Hypa {
            producer: "producer".into(), model: "model".into(), endpoint: None,
            preprocess_version: 1, dimensions: 4, vector: vec![7; 16], metadata: None,
        },
        write_clock: Sequence::from(1u64), writer_id: "writer".into(),
    };
    let entry = row.to_entry(SectionKind::Hypa, true).unwrap();
    let mut object_entry = entry.clone();
    let SectionValue::Hypa(value) = &mut object_entry.value else { unreachable!() };
    value.vector = InlineOrObject::Object(ObjectReference {
        content_sha256: risunest_external_storage_format::content_identity::hash(&[7; 16]), byte_length: 16,
    });
    assert_eq!(resolve_section_row(Some(&entry), &object_entry).unwrap(), SectionMergeDecision::Settled);
}

#[test]
fn e2_server_apply_rechecks_the_local_version_and_rolls_back_conflicting_batches() {
    let directory = tempfile::tempdir().unwrap();
    let mut device = DeviceStore::open(directory.path()).unwrap();
    let local = plugin("9", "writer");
    device.apply_section_rows(Section::LocalPlugins, &[local.clone()]).unwrap();
    server::write_sections(&mut device, &[server::SectionWrite::Apply {
        domain: Domain::LocalPlugins, entry: local_entry(&plugin("8", "writer")).entry, object: None,
    }]).unwrap();
    assert_eq!(device.read_section_rows(Section::LocalPlugins).unwrap(), vec![local.clone()]);
    assert!(!device.read_section_entry(Section::LocalPlugins, &local_entry(&local).entry.key).unwrap().unwrap().published);

    let mut conflict = local.clone();
    conflict.value = SectionValueRow::Plugin { space: "string".into(), value: "conflict".into() };
    let mut other = plugin("10", "writer");
    other.key3 = "another".into();
    let before = device.section_state(Section::LocalPlugins).unwrap();
    assert!(server::write_sections(&mut device, &[
        server::SectionWrite::Apply { domain: Domain::LocalPlugins, entry: local_entry(&other).entry, object: None },
        server::SectionWrite::Apply { domain: Domain::LocalPlugins, entry: local_entry(&conflict).entry, object: None },
    ]).is_err());
    assert_eq!(device.read_section_rows(Section::LocalPlugins).unwrap(), vec![local.clone()]);
    assert_eq!(device.section_state(Section::LocalPlugins).unwrap(), before);

    server::write_sections(&mut device, &[server::SectionWrite::MarkVersion {
        domain: Domain::LocalPlugins, key: local_entry(&local).entry.key,
        version: plugin("8", "writer").version(),
    }]).unwrap();
    assert!(!device.read_section_entry(Section::LocalPlugins, &local_entry(&local).entry.key).unwrap().unwrap().published);
}

#[test]
fn server_completion_preserves_a_device_tail_until_its_exact_version_is_published() {
    let directory = tempfile::tempdir().unwrap();
    let mut device = DeviceStore::open(directory.path()).unwrap();
    device.set_section_participating(Section::LocalPlugins, false).unwrap();
    device.write_plugin_device_values("plugin", &[PluginDeviceMutation::Set {
        space: "string".into(), key: "key".into(), value: "first".into(),
    }]).unwrap();
    let first = device.read_section_rows(Section::LocalPlugins).unwrap().remove(0);
    assert!(!server::has_pending(&device).unwrap());
    device.set_section_participating(Section::LocalPlugins, true).unwrap();
    assert!(server::has_pending(&device).unwrap());

    device.write_plugin_device_values("plugin", &[PluginDeviceMutation::Set {
        space: "string".into(), key: "key".into(), value: "newer".into(),
    }]).unwrap();
    let newer = device.read_section_rows(Section::LocalPlugins).unwrap().remove(0);
    server::write_sections(&mut device, &[server::SectionWrite::MarkVersion {
        domain: Domain::LocalPlugins, key: local_entry(&first).entry.key,
        version: first.version(),
    }]).unwrap();
    assert!(server::has_pending(&device).unwrap());
    server::write_sections(&mut device, &[server::SectionWrite::MarkVersion {
        domain: Domain::LocalPlugins, key: local_entry(&newer).entry.key,
        version: newer.version(),
    }]).unwrap();
    assert!(!server::has_pending(&device).unwrap());
}

#[test]
fn e3_spools_deduplicate_vectors_are_read_only_and_clean_up_after_use() {
    use risunest_external_storage_format::{content_identity::hash, format::FingerprintBuilder};
    let mut spool = SectionSpoolBuilder::new(Section::Hypa).unwrap();
    let mut fingerprint = FingerprintBuilder::new(&SectionKind::Hypa.fingerprint_domain());
    for key in ["a", "b"] {
        let row = SectionRow {
            key1: key.repeat(64), key2: String::new(), key3: String::new(),
            value: SectionValueRow::Hypa {
                producer: "producer".into(), model: "model".into(), endpoint: None,
                preprocess_version: 1, dimensions: 2048, vector: vec![7; 8192], metadata: None,
            },
            write_clock: Sequence::from(1u64), writer_id: "writer".into(),
        };
        let entry = row.to_entry(SectionKind::Hypa, true).unwrap();
        let digest = hash(&entry.encode().unwrap());
        fingerprint.push(&entry.key, &digest).unwrap();
        spool.push(row, &entry.key, &digest).unwrap();
    }
    let prepared = spool.finish(&fingerprint.finish()).unwrap();
    let objects: i64 = prepared.connection.query_row("SELECT count(*) FROM vector_values", [], |row| row.get(0)).unwrap();
    assert_eq!(objects, 1);
    assert!(prepared.connection.execute("DELETE FROM row_index", []).is_err());
    let mut count = 0;
    prepared.visit(|row| {
        let SectionValueRow::Hypa { vector, .. } = row.value else { panic!("expected a vector") };
        assert_eq!(vector, vec![7; 8192]);
        count += 1;
        Ok(())
    }).unwrap();
    assert_eq!(count, 2);
    let path = prepared._directory.path().to_path_buf();
    drop(prepared);
    assert!(!path.exists());
}

#[test]
fn e3_spool_key_pages_limit_bytes_as_well_as_rows() {
    use risunest_external_storage_format::{content_identity::hash, format::FingerprintBuilder};
    let mut spool = SectionSpoolBuilder::new(Section::LocalPlugins).unwrap();
    let mut fingerprint = FingerprintBuilder::new(&SectionKind::LocalPlugins.fingerprint_domain());
    for index in 0..100 {
        let mut row = plugin("1", "writer");
        row.key3 = format!("{index:04}-{}", "x".repeat(48 * 1024));
        let entry = row.to_entry(SectionKind::LocalPlugins, true).unwrap();
        let digest = hash(&entry.encode().unwrap());
        fingerprint.push(&entry.key, &digest).unwrap();
        spool.push(row, &entry.key, &digest).unwrap();
    }
    let prepared = spool.finish(&fingerprint.finish()).unwrap();
    let first = {
        let mut statement = prepared.connection.prepare("SELECT key1,key2,key3 FROM row_index ORDER BY key1,key2,key3").unwrap();
        let mut rows = statement.query([]).unwrap();
        read_key_page(&mut rows).unwrap()
    };
    assert!(!first.is_empty() && first.len() < 100);
    let bytes: usize = first.iter().map(|key| key.0.len() + key.1.len() + key.2.len() + std::mem::size_of::<SectionKey>()).sum();
    assert!(bytes <= SECTION_PAGE_BYTES);
    drop(first);
    let mut visited = 0;
    prepared.visit(|_| { visited += 1; Ok(()) }).unwrap();
    assert_eq!(visited, 100);
}

fn set_plugin(store: &mut DeviceStore, key: &str, value: &str) {
    store.write_plugin_device_values("plugin", &[PluginDeviceMutation::Set {
        space: "string".into(), key: key.into(), value: value.into(),
    }]).unwrap();
}

#[test]
fn e4_captures_selected_sections_from_one_read_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = DeviceStore::open(directory.path()).unwrap();
    set_plugin(&mut store, "value", "before");
    store.write_setting("dosync", &serde_json::json!("before")).unwrap();
    let snapshot = Connection::open_with_flags(
        directory.path().join(super::super::DEVICE_DATABASE_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).unwrap();
    snapshot.execute_batch("PRAGMA query_only=ON; BEGIN;").unwrap();
    let mut plugins = SectionSpoolBuilder::new_backup(SectionKind::LocalPlugins).unwrap();
    capture_backup_rows(&snapshot, SectionKind::LocalPlugins, &mut plugins).unwrap();
    set_plugin(&mut store, "value", "after");
    store.write_setting("dosync", &serde_json::json!("after")).unwrap();
    let mut settings = SectionSpoolBuilder::new_backup(SectionKind::LocalSettings).unwrap();
    capture_backup_rows(&snapshot, SectionKind::LocalSettings, &mut settings).unwrap();
    snapshot.execute_batch("COMMIT;").unwrap();
    let plugins = plugins.finish_captured().unwrap();
    let settings = settings.finish_captured().unwrap();
    let mut plugin_value = None;
    plugins.visit_entries(|entry, _| {
        let SectionValue::LocalPlugin(value) = &entry.value else { unreachable!() };
        plugin_value = value.value.as_str().map(str::to_owned);
        Ok(())
    }).unwrap();
    let mut setting_value = None;
    settings.visit_entries(|entry, _| {
        if let SectionValue::LocalSetting(value) = &entry.value {
            setting_value = value.value.as_str().map(str::to_owned);
        }
        Ok(())
    }).unwrap();
    assert_eq!(plugin_value.as_deref(), Some("before"));
    assert_eq!(setting_value.as_deref(), Some("before"));
}

#[test]
fn e4_backup_entries_are_visited_in_canonical_key_order() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = DeviceStore::open(directory.path()).unwrap();
    store.write_setting("dosync", &serde_json::json!(true)).unwrap();
    for code_hash in ["a".repeat(64), "f".repeat(64)] {
        store.write_plugin_permission(&code_hash, "network", true).unwrap();
    }
    let prepared = store.capture_backup_sections(&[SectionKind::LocalSettings])
        .unwrap().pop().unwrap();
    let mut keys = Vec::new();
    prepared.visit_entries(|entry, _| { keys.push(entry.key.clone()); Ok(()) }).unwrap();
    let mut sorted = keys.clone(); sorted.sort(); assert_eq!(keys, sorted);

    let mut plugin_spool = SectionSpoolBuilder::new_backup(SectionKind::LocalPlugins).unwrap();
    for (owner, key) in [
        ("owner-z".to_owned(), "short".to_owned()),
        ("소유자".to_owned(), "인용-\"-키".to_owned()),
        ("owner-a".to_owned(), "x".repeat(4096)),
    ] {
        let mut row = plugin("0", ""); row.key1 = owner; row.key3 = key;
        plugin_spool.push_backup_row(row).unwrap();
    }
    let prepared = plugin_spool.finish_captured().unwrap();
    let mut keys = Vec::new();
    prepared.visit_entries(|entry, _| { keys.push(entry.key.clone()); Ok(()) }).unwrap();
    let mut sorted = keys.clone(); sorted.sort(); assert_eq!(keys, sorted);
}

#[test]
fn e4_prepared_restore_is_bounded_empty_aware_and_idempotent() {
    let source_directory = tempfile::tempdir().unwrap();
    let mut source = DeviceStore::open(source_directory.path()).unwrap();
    set_plugin(&mut source, "alpha", "from-backup");
    let prepared = source.capture_backup_sections(&[SectionKind::LocalPlugins])
        .unwrap().pop().unwrap();
    assert_eq!(prepared.kind(), SectionKind::LocalPlugins);
    assert_eq!(prepared.len(), 1);
    assert!(!prepared.is_empty());
    let target_directory = tempfile::tempdir().unwrap();
    let mut target = DeviceStore::open(target_directory.path()).unwrap();
    set_plugin(&mut target, "dropped", "before");
    target.restore_prepared_backup_section(&prepared).unwrap();
    let first_clock = target.section_state(Section::LocalPlugins).unwrap().max_write_clock;
    let first_rows = target.read_section_rows(Section::LocalPlugins).unwrap();
    target.restore_prepared_backup_section(&prepared).unwrap();
    assert_eq!(target.section_state(Section::LocalPlugins).unwrap().max_write_clock, first_clock);
    assert_eq!(target.read_section_rows(Section::LocalPlugins).unwrap(), first_rows);
    let empty_directory = tempfile::tempdir().unwrap();
    let mut empty = DeviceStore::open(empty_directory.path()).unwrap();
    let empty = empty.capture_backup_sections(&[SectionKind::LocalPlugins])
        .unwrap().pop().unwrap();
    assert!(empty.is_empty());
    target.restore_prepared_backup_section(&empty).unwrap();
    assert!(target.read_backup_section_rows(Section::LocalPlugins).unwrap().is_empty());
}
