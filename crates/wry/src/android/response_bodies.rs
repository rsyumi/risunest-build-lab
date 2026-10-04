// RisuNest: custom-protocol response bodies above a threshold stay in native
// memory, and `RustResponseStream` copies one piece out per read, so a large
// body never becomes one Java array.

use std::{
  borrow::Cow,
  collections::BTreeMap,
  sync::{
    atomic::{AtomicI64, Ordering},
    Mutex, MutexGuard,
  },
};

/// Bodies up to this size keep the plain `ByteArrayInputStream`.
pub(crate) const STREAM_THRESHOLD: usize = 256 * 1024;

struct Body {
  bytes: Cow<'static, [u8]>,
  position: usize,
}

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);
static BODIES: Mutex<BTreeMap<i64, Body>> = Mutex::new(BTreeMap::new());

fn bodies() -> MutexGuard<'static, BTreeMap<i64, Body>> {
  BODIES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Holds a body until [`release`] and returns its handle.
pub(crate) fn register(bytes: Cow<'static, [u8]>) -> i64 {
  let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
  bodies().insert(handle, Body { bytes, position: 0 });
  handle
}

/// Passes the next piece of at most `max` bytes to `copy`, and moves past it
/// only when the copy succeeds. An empty piece means the body is finished;
/// `None` means the handle is not held.
pub(crate) fn read_with<E>(
  handle: i64,
  max: usize,
  copy: impl FnOnce(&[u8]) -> Result<(), E>,
) -> Option<Result<usize, E>> {
  let mut bodies = bodies();
  let body = bodies.get_mut(&handle)?;
  let end = body.bytes.len().min(body.position.saturating_add(max));
  let result = copy(&body.bytes[body.position..end]);
  Some(result.map(|()| {
    let read = end - body.position;
    body.position = end;
    read
  }))
}

/// Drops a held body. Returns false when the handle was already released.
pub(crate) fn release(handle: i64) -> bool {
  bodies().remove(&handle).is_some()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn read_all(handle: i64, max: usize) -> Vec<Vec<u8>> {
    let mut pieces = Vec::new();
    loop {
      let mut piece = Vec::new();
      let read = read_with(handle, max, |bytes| {
        piece.extend_from_slice(bytes);
        Ok::<(), ()>(())
      })
      .expect("held")
      .expect("copied");
      assert_eq!(read, piece.len());
      if read == 0 {
        return pieces;
      }
      pieces.push(piece);
    }
  }

  #[test]
  fn reads_a_body_in_pieces_of_the_requested_size() {
    let body: Vec<u8> = (0..=255).cycle().take(1000).collect();
    let handle = register(Cow::Owned(body.clone()));
    let pieces = read_all(handle, 300);
    assert_eq!(
      pieces.iter().map(Vec::len).collect::<Vec<_>>(),
      [300, 300, 300, 100]
    );
    assert_eq!(pieces.concat(), body);
    assert!(release(handle));
  }

  #[test]
  fn a_short_final_read_and_later_reads_report_the_end() {
    static BODY: &[u8] = b"synthetic static body";
    let handle = register(Cow::Borrowed(BODY));
    assert_eq!(read_all(handle, 1 << 20), [BODY.to_vec()]);
    assert_eq!(read_with(handle, 8, |_| Ok::<(), ()>(())), Some(Ok(0)));
    assert!(release(handle));
  }

  #[test]
  fn a_failed_copy_does_not_advance() {
    let handle = register(Cow::Owned(b"abcdef".to_vec()));
    assert_eq!(read_with(handle, 4, |_| Err("pending exception")), Some(Err("pending exception")));
    assert_eq!(read_all(handle, 4), [b"abcd".to_vec(), b"ef".to_vec()]);
    assert!(release(handle));
  }

  #[test]
  fn a_second_release_and_reads_after_release_find_nothing() {
    let handle = register(Cow::Owned(vec![1, 2, 3]));
    assert!(release(handle));
    assert!(!release(handle));
    assert!(read_with(handle, 3, |_| Ok::<(), ()>(())).is_none());
  }

  #[test]
  fn releasing_an_unread_body_frees_only_that_body() {
    let unread = register(Cow::Owned(vec![7; 64]));
    let other = register(Cow::Owned(vec![9; 4]));
    assert_ne!(unread, other);
    assert!(release(unread));
    assert_eq!(read_all(other, 64), [vec![9; 4]]);
    assert!(release(other));
  }
}
