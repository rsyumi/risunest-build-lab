use super::*;

/// Native jobs read through stores of their own while the renderer's store
/// commits. The log must fold back afterwards whichever form names the root.
#[test]
fn concurrent_job_store_reads_leave_the_log_foldable() {
    let directory = tempfile::tempdir().unwrap();
    let canonical = fs::canonicalize(directory.path()).unwrap();
    for root in [directory.path().join("plain"), canonical.join("canonical")] {
        fs::create_dir_all(&root).unwrap();
        let mut store = PersistentStore::open(&root).unwrap();
        let readers = (0..4)
            .map(|_| store.open_native_job_store().unwrap())
            .collect::<Vec<_>>();
        let readers = thread::scope(|scope| {
            let running = readers
                .into_iter()
                .map(|reader| {
                    scope.spawn(move || {
                        for _ in 0..300 {
                            reader.read_root(None).unwrap();
                        }
                        reader
                    })
                })
                .collect::<Vec<_>>();
            for index in 0..60 {
                let commit: WorkingSetCommit = serde_json::from_value(json!({
                    "expectedRevision": store.revision().unwrap(),
                    "rootMutations": [
                        { "type": "set", "key": "username", "value": format!("writer-{index}") }
                    ]
                }))
                .unwrap();
                store.commit(&commit).unwrap();
            }
            running
                .into_iter()
                .map(|reader| reader.join().unwrap())
                .collect::<Vec<_>>()
        });
        let (busy, log, checkpointed): (i64, i64, i64) = store
            .connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(
            busy,
            0,
            "{}: log={log} checkpointed={checkpointed}",
            root.display()
        );
        drop(readers);
    }
}
