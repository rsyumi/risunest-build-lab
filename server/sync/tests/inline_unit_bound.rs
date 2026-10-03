mod common;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::*;
use risunest_sync_server::store::Store;
use risunest_sync_wire::{
    lww::UnitChange,
    unit::{UnitValue, MAX_INLINE_UNIT_BYTES},
};

const FIELDS: [&str; 4] = [
    "additionalPrompt",
    "NAIImgUrl",
    "autofillRequestUrl",
    "language",
];

// A JSON string whose canonical form is exactly `len` bytes.
fn text(len: usize, fill: char) -> Vec<u8> {
    format!("\"{}\"", fill.to_string().repeat(len - 2)).into_bytes()
}

#[test]
fn inline_units_at_the_bound_push_pull_and_page_through_state() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let b = device(&store);
    let mut pushed = Vec::new();
    for (i, field) in FIELDS.iter().enumerate() {
        let value =
            UnitValue::inline(&text(MAX_INLINE_UNIT_BYTES, (b'a' + i as u8) as char)).unwrap();
        let change = unit(&["root", field], WRITER_A, 10 + i as u64, value);
        store
            .push(
                &a,
                &request(
                    &store,
                    WRITER_A,
                    &format!("at-bound-{i}"),
                    vec![change.clone()],
                ),
            )
            .unwrap();
        pushed.push(change);
    }

    let mut pulled = Vec::new();
    let mut after = 0.into();
    loop {
        let page = store.changes(&b, after, 1024).unwrap();
        assert!(!page.items.is_empty() || page.next_after == page.through_seq);
        pulled.extend(page.items.into_iter().map(|item| UnitChange {
            key: item.key,
            stamp: item.stamp,
            value: item.value,
        }));
        if page.next_after == page.through_seq {
            break;
        }
        after = page.next_after;
    }
    assert_eq!(pulled, pushed);

    let pin = store.create_state_pin(&b).unwrap();
    let mut state = Vec::new();
    let mut next = None;
    loop {
        let page = store
            .state_page(&b, &pin.pin_id, next.as_ref(), 1024)
            .unwrap();
        assert!(!page.items.is_empty());
        state.extend(page.items);
        next = page.next_key;
        if next.is_none() {
            break;
        }
    }
    store.release_state_pin(&b, &pin.pin_id).unwrap();
    state.sort_by(|left, right| left.stamp.cmp(&right.stamp));
    assert_eq!(state, pushed);
}

#[test]
fn an_inline_unit_above_the_bound_is_rejected_at_push() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let a = device(&store);
    let over = unit(
        &["root", "language"],
        WRITER_A,
        10,
        UnitValue::Inline {
            bytes: URL_SAFE_NO_PAD.encode(text(MAX_INLINE_UNIT_BYTES + 1, 'a')),
        },
    );
    assert_eq!(
        store
            .push(&a, &request(&store, WRITER_A, "above-bound", vec![over]))
            .unwrap_err()
            .code,
        "inline-unit-too-large"
    );
    assert!(store.changes(&a, 0.into(), 1024).unwrap().items.is_empty());
}
