pub mod config;
pub mod connection;
pub mod http;
pub mod management;
#[cfg(test)]
mod management_tests;
pub mod publication;
pub mod runtime;
pub mod store;
pub mod tunnel;
#[cfg(windows)]
mod tunnel_job;
pub mod workload;

pub use risunest_sync_wire::PROTOCOL_ID;
pub const STORE_FORMAT_ID: &str = "risunest-sync-store/v1";

pub fn resolve_data_root(path: &std::path::Path) -> Result<std::path::PathBuf> {
    if !path.is_absolute() {
        return Err(Error::new("absolute-data-dir-required", 400));
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            let linked = {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let linked = meta.file_type().is_symlink();
            if linked {
                return Err(Error::new("unsafe-storage-path", 400));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let parent = path.parent().ok_or(Error::new("invalid-data-dir", 400))?;
    let name = path
        .file_name()
        .ok_or(Error::new("invalid-data-dir", 400))?;
    Ok(std::fs::canonicalize(parent)?.join(name))
}

#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub status: u16,
    /// The record a page or seal rejection failed on. A locator, not content,
    /// so the client can name the change without another round of guessing.
    pub key: Option<String>,
}
impl Error {
    pub fn new(code: &'static str, status: u16) -> Self {
        Self {
            code,
            status,
            key: None,
        }
    }
    pub fn for_key(mut self, key: &str) -> Self {
        self.key = Some(key.to_owned());
        self
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::new("storage-io", 503)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::new("metadata-storage", 503)
    }
}
impl From<risunest_sync_wire::WireError> for Error {
    fn from(value: risunest_sync_wire::WireError) -> Self {
        let status = match value.0 {
            "metadata-too-large" | "batch-too-large" | "frame-too-large" => 413,
            _ => 400,
        };
        Self::new(value.0, status)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

impl From<risunest_sync_connect::ConnectError> for Error {
    fn from(value: risunest_sync_connect::ConnectError) -> Self {
        Self::new(value.0, 400)
    }
}
