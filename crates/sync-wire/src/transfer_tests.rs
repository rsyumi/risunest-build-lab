use crate::{
    delta::source_test_observer,
    transfer::{encode, encoded_len, Frame},
};
use std::sync::{Arc, Mutex};

/// Sizing a reply reads no body, so a full frame is hashed only when the
/// reply is encoded for sending.
#[test]
fn sizing_does_not_hash_and_send_still_validates() {
    let thread = std::thread::current().id();
    let hashed = Arc::new(Mutex::new(Vec::new()));
    let seen = hashed.clone();
    source_test_observer::install(Arc::new(move |event: source_test_observer::Event| {
        if std::thread::current().id() == thread {
            seen.lock().unwrap().push((event.domain, event.calls, event.bytes));
        }
    }));
    let body = b"synthetic sized reply body".to_vec();
    let frames = [Frame::Full(body.clone())];
    for _ in 0..3 {
        assert_eq!(encoded_len(&frames).unwrap(), body.len() + 53);
    }
    assert!(hashed.lock().unwrap().is_empty());
    assert_eq!(encode(&frames).unwrap().len(), body.len() + 53);
    assert_eq!(
        *hashed.lock().unwrap(),
        [("transfer-full-encode-sha256", 1, body.len() as u64)]
    );
}
