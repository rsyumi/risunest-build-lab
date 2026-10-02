#![allow(dead_code)]
use risunest_sync_server::store::{Device, Store};
use risunest_sync_wire::{
    lww::{PushRequest, UnitChange},
    stamp::Stamp,
    unit::{UnitKey, UnitValue},
};
pub const WRITER_A: &str = "00000000-0000-4000-8000-000000000001";
pub const WRITER_B: &str = "00000000-0000-4000-8000-000000000002";
pub fn device(store: &Store) -> Device {
    let credential = store.add_device().unwrap();
    store
        .authenticate(&credential.library_id, &credential.token)
        .unwrap()
}
pub fn inline(key: &str, writer: &str, physical: u64, value: &str) -> UnitChange {
    unit(
        &["root", key],
        writer,
        physical,
        UnitValue::inline(&serde_json::to_vec(value).unwrap()).unwrap(),
    )
}
pub fn unit(key: &[&str], writer: &str, physical: u64, value: UnitValue) -> UnitChange {
    UnitChange {
        key: UnitKey::new(key).unwrap(),
        stamp: Stamp {
            physical_ms: physical.into(),
            logical: 0,
            writer_id: writer.into(),
        },
        value,
    }
}
pub fn request(
    store: &Store,
    writer: &str,
    operation: &str,
    changes: Vec<UnitChange>,
) -> PushRequest {
    PushRequest {
        library_id: store.head().unwrap().library_id,
        writer_id: writer.into(),
        operation_id: operation.into(),
        changes,
    }
}
