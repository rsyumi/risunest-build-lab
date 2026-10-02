use super::{message_pages::{accept_copied_database, put_object, verified_object_present}, PersistentStore};
use risunest_external_storage_format::message_pages::{MessageManifest, MANIFEST_SCHEMA};
use risunest_sync_wire::{descriptor::RecordDescriptor, unit::UnitValue};
use rusqlite::params;

fn fixture() -> (tempfile::TempDir, PersistentStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = PersistentStore::open(directory.path()).unwrap();
    (directory, store)
}

#[test]
fn copied_database_rejects_forged_cached_message_identity() {
    let (_directory, mut store) = fixture();
    store.connection.execute("INSERT INTO messages(generation,character_id,conversation_id,message_index,value,canonical_hash,canonical_size)
        VALUES('g','c','chat',0,'{}',?1,2)",["0".repeat(64)]).unwrap();
    let tx = store.connection.transaction().unwrap();
    assert!(accept_copied_database(&tx).unwrap_err().to_string().contains("identity mismatch"));
}

#[test]
fn copied_database_rejects_forged_verified_page_and_index() {
    let (_directory, mut store) = fixture();
    let hash = "0".repeat(64);
    store.connection.execute("INSERT INTO message_page_objects VALUES(?1,?2)",params![hash,b"{}".as_slice()]).unwrap();
    store.connection.execute("INSERT INTO message_page_verified_objects VALUES(?1)",[&hash]).unwrap();
    store.connection.execute("INSERT INTO message_page_proofs VALUES(?1,1,2,2,1,'[]')",[&hash]).unwrap();
    store.connection.execute("INSERT INTO message_page_indexes VALUES('g','c','chat',0,1,?1,2)",[&hash]).unwrap();
    let tx = store.connection.transaction().unwrap();
    assert!(accept_copied_database(&tx).unwrap_err().to_string().contains("no manifest"));
    assert!(!verified_object_present(&tx,&hash).unwrap());
}

#[test]
fn copied_database_does_not_certify_unreferenced_objects() {
    let (_directory, mut store) = fixture();
    let body = b"{}";
    let hash = risunest_sync_wire::hash(body);
    put_object(&store.connection,&hash,body).unwrap();
    let tx = store.connection.transaction().unwrap();
    accept_copied_database(&tx).unwrap();
    assert!(!verified_object_present(&tx,&hash).unwrap());
}

#[test]
fn copied_database_recertifies_known_held_and_deferred_without_changing_receipts() {
    let (_directory, mut store) = fixture();
    let writer = store.lww_clock_state().unwrap().writer_id;
    let stamp = serde_json::to_string(&risunest_sync_wire::stamp::Stamp { physical_ms: 123.into(), logical: 7, writer_id: writer }).unwrap();
    let manifest = MessageManifest {schema: MANIFEST_SCHEMA.into(),message_count:0.into(),pages:Vec::new()};
    let object = manifest.encode().unwrap();
    put_object(&store.connection,&object.hash,&object.bytes).unwrap();
    let descriptor = RecordDescriptor::content(object.hash.clone());
    let descriptor_body = descriptor.bytes().unwrap();
    put_object(&store.connection,&risunest_sync_wire::hash(&descriptor_body),&descriptor_body).unwrap();
    let value = UnitValue::object(descriptor).unwrap();
    for status in ["held","deferred"] {
        let key = risunest_sync_wire::unit::UnitKey::new(&["messages",status,"chat"]).unwrap();
        store.connection.execute("INSERT INTO lww_receive_rows VALUES(?1,?2,?3,?4,?5)",params![status,key.as_str(),stamp,serde_json::to_string(&value).unwrap(),status]).unwrap();
    }
    let before:Vec<(String,String,String)> = store.connection.prepare("SELECT key,stamp,status FROM lww_receive_rows ORDER BY key").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<Result<_,_>>().unwrap();
    let tx = store.connection.transaction().unwrap();
    accept_copied_database(&tx).unwrap();
    assert!(verified_object_present(&tx,&object.hash).unwrap());
    let after:Vec<(String,String,String)> = tx.prepare("SELECT key,stamp,status FROM lww_receive_rows ORDER BY key").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(after,before);
    tx.commit().unwrap();
}
