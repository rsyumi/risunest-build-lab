//! The library format level this client writes, and the level a Sync server
//! reports for the library on each reply.
use super::{Result, SyncError};
use risunest_sync_wire::library_format::{parse_header, HEADER, MAX_HEADER};

/// The highest library format level this client reads and writes.
pub(crate) const LEVEL: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LibraryFormat {
    /// The library's level.
    pub level: u64,
    /// The highest level the server accepts.
    pub max: u64,
}

/// Reads the level headers of a reply. A server that predates levels sends
/// neither; a reply with only one of them, or one that does not parse, is
/// refused.
pub(crate) fn read(headers: &reqwest::header::HeaderMap) -> Result<Option<LibraryFormat>> {
    let value = |name| headers.get(name).map(|value| value.to_str().ok().and_then(|value| parse_header(value).ok()));
    match (value(HEADER), value(MAX_HEADER)) {
        (None, None) => Ok(None),
        (Some(Some(level)), Some(Some(max))) => Ok(Some(LibraryFormat { level, max })),
        _ => Err(SyncError::new("invalid-library-format", 502)),
    }
}
