//! Bounded, provider/OS/PDS-independent byte formats shared by native and WASM.
pub mod catalog;
pub mod content_identity;
pub mod control;
pub mod crypto;
pub mod format;
pub mod logical_records;
pub mod pack;
pub mod section;
pub mod snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatError(pub &'static str);
impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for FormatError {}
impl From<std::io::Error> for FormatError {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        Self(match error.kind() {
            ErrorKind::StorageFull => "object-io-storage-full",
            ErrorKind::PermissionDenied => "object-io-permission-denied",
            ErrorKind::ReadOnlyFilesystem => "object-io-read-only",
            ErrorKind::UnexpectedEof => "object-unexpected-eof",
            _ => "object-io-failed",
        })
    }
}
pub type Result<T> = std::result::Result<T, FormatError>;

impl FormatError {
    pub fn io_kind(self) -> Option<std::io::ErrorKind> {
        use std::io::ErrorKind;
        match self.0 {
            "object-io-storage-full" => Some(ErrorKind::StorageFull),
            "object-io-permission-denied" => Some(ErrorKind::PermissionDenied),
            "object-io-read-only" => Some(ErrorKind::ReadOnlyFilesystem),
            "object-io-failed" => Some(ErrorKind::Other),
            _ => None,
        }
    }
}
