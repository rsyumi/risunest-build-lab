use super::*;

#[test]
fn ordering_work_is_scoped_and_preserves_duplicate_and_parent_checks() {
    let root = tempfile::tempdir().unwrap();
    let cache_root = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(root.path()).unwrap();
    let generation = super::super::active_generation(&store.connection).unwrap();
    for (index, character) in ["affected", "unrelated"].into_iter().enumerate() {
        store.connection.execute("INSERT INTO characters(generation,character_id,configured_index,recent_at,trashed,name,conversation_count,type,detail) VALUES(?1,?2,?3,0,0,?2,0,'character','{}')",
            params![generation, character, index as i64]).unwrap();
    }
    store.connection.execute("WITH RECURSIVE indices(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM indices WHERE i<99999) INSERT INTO conversations SELECT ?1,'unrelated','conversation-'||i,i,0,'synthetic',0,'{}' FROM indices", [&generation]).unwrap();
    for index in 0..3 {
        store.connection.execute("INSERT INTO conversations VALUES(?1,'affected',?2,?3,0,'synthetic',0,'{}')",
            params![generation, format!("conversation-{index}"), index]).unwrap();
    }
    store.connection.execute_batch("CREATE TEMP TABLE server_cycle_records(key TEXT PRIMARY KEY,version TEXT NOT NULL,local_hash TEXT,action TEXT NOT NULL,remote TEXT NOT NULL)").unwrap();
    let key = |kind: &str, key1: &str, key2: &str| projection::wire_key(&outbox::ServerDirtyKey {
        kind: kind.into(), key1: key1.into(), key2: key2.into(), revision: 0,
    }).unwrap();
    let absent = json(&RecordVersion::Absent).unwrap();
    store.connection.execute("INSERT INTO server_cycle_records VALUES(?1,?2,NULL,'apply',?2)",
        params![key("conversation", "affected", "conversation-0"), absent]).unwrap();
    let client = ServerClient::new(crate::server_sync::client::ServerConfig {
        directory: None, endpoint: "http://127.0.0.1:1".into(), library_id: "library".into(), device_id: "device".into(), token: "a".repeat(64),
    }).unwrap();
    let cache = Cache::open(cache_root.path()).unwrap();
    let transfer = Transfer::new(&client, &cache).unwrap();
    let started = std::time::Instant::now();
    assert!(store.server_order_conflicts(&cache, &transfer, 0, None).unwrap().is_empty());
    let copied: i64 = store.connection.query_row("SELECT count(*) FROM server_cycle_order", [], |row| row.get(0)).unwrap();
    assert_eq!(copied, 2);
    eprintln!("100000 unrelated conversations, scoped remaining rows={copied}, elapsed_us={}", started.elapsed().as_micros());
    store.connection.execute("UPDATE conversations SET configured_index=1 WHERE character_id='affected' AND conversation_id='conversation-2'", []).unwrap();
    assert!(store.server_order_conflicts(&cache, &transfer, 0, None).unwrap().contains("order:affected"));
    store.connection.execute("INSERT INTO server_cycle_records VALUES(?1,?2,NULL,'apply',?2)",
        params![key("character", "affected", ""), absent]).unwrap();
    assert!(store.server_order_conflicts(&cache, &transfer, 0, None).unwrap().contains("family:affected"));
}
