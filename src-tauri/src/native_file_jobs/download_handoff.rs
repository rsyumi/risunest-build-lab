use super::{cleanup_handoff_path, owned_handoff_file, NativeFileJobState, NativeJobError};
use crate::native_log::logged;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use tauri::State;
use uuid::Uuid;

// A frontend download on Android is written here in pieces, then copied to the
// user's destination by the SAF export bridge.
pub(crate) const PREFIX: &str = "risu-download-";
pub(crate) const SUFFIX: &str = ".bin";
/// Raw bytes per append; the base64 text of a full piece and its request
/// envelope stay within the 4 MiB plain invoke budget.
const MAX_PIECE_BYTES: usize = 3 * 1024 * 1024 - 3 * 1024;
const MAX_ENCODED_PIECE_BYTES: usize = MAX_PIECE_BYTES / 3 * 4;
const LABEL: &str = "download";

fn write_error(operation: &str, error: impl std::fmt::Display) -> NativeJobError {
    NativeJobError::new("destination-write-failed", format!("{operation}: {error}"))
}

fn create(root: &Path) -> Result<String, NativeJobError> {
    let directory = root.join("handoffs");
    fs::create_dir_all(&directory)
        .map_err(|error| write_error("create download handoff directory", error))?;
    let path = directory.join(format!("{PREFIX}{}{SUFFIX}", Uuid::new_v4()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| write_error("create download handoff", error))?;
    Ok(path.to_string_lossy().into_owned())
}

fn append(root: &Path, path: &Path, offset: u64, chunk: &str) -> Result<u64, NativeJobError> {
    if chunk.len() > MAX_ENCODED_PIECE_BYTES {
        return Err(NativeJobError::new(
            "invalid-input",
            "download handoff pieces cannot exceed 3 MiB",
        ));
    }
    let bytes = STANDARD.decode(chunk).map_err(|error| {
        NativeJobError::new(
            "invalid-input",
            format!("download handoff piece is not base64: {error}"),
        )
    })?;
    if !owned_handoff_file(root, path, PREFIX, SUFFIX, LABEL, "destination-write-failed")? {
        return Err(NativeJobError::new("invalid-state", "download handoff is gone"));
    }
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| write_error("open download handoff", error))?;
    let length = file
        .metadata()
        .map_err(|error| write_error("read download handoff length", error))?
        .len();
    // A repeated or skipped piece must not shift the rest of the file.
    if length != offset {
        return Err(NativeJobError::new(
            "invalid-state",
            format!("download handoff holds {length} bytes, not {offset}"),
        ));
    }
    file.write_all(&bytes)
        .map_err(|error| write_error("append download handoff", error))?;
    Ok(length + bytes.len() as u64)
}

#[tauri::command(async)]
pub(crate) fn native_download_handoff_create(
    state: State<'_, NativeFileJobState>,
) -> Result<String, NativeJobError> {
    logged("native_download_handoff_create", (|| {
        let _cleanup_operation = state.admit_cleanup_operation()?;
        create(&state.root)
    })())
}

#[tauri::command(async)]
pub(crate) fn native_download_handoff_append(
    state: State<'_, NativeFileJobState>,
    path: String,
    offset: u64,
    chunk: String,
) -> Result<u64, NativeJobError> {
    logged("native_download_handoff_append", (|| {
        let _cleanup_operation = state.admit_cleanup_operation()?;
        append(&state.root, Path::new(&path), offset, &chunk)
    })())
}

#[tauri::command(async)]
pub(crate) fn native_download_handoff_cleanup(
    state: State<'_, NativeFileJobState>,
    path: String,
) -> Result<bool, NativeJobError> {
    logged(
        "native_download_handoff_cleanup",
        cleanup_handoff_path(&state.root, Path::new(&path), PREFIX, SUFFIX, LABEL),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, PathBuf) {
        let directory = TempDir::new().unwrap();
        let root = directory.path().join("native-file-jobs");
        fs::create_dir_all(&root).unwrap();
        (directory, root)
    }

    fn code(result: Result<u64, NativeJobError>) -> String {
        result.unwrap_err().code
    }

    #[test]
    fn appended_pieces_build_the_owned_handoff_in_order() {
        let (_directory, root) = fixture();
        let path = PathBuf::from(create(&root).unwrap());
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(super::super::handoff_name(&path, PREFIX, SUFFIX), "{name}");
        assert_eq!(path.parent().unwrap(), root.join("handoffs"));
        assert_eq!(fs::read(&path).unwrap(), b"");

        assert_eq!(append(&root, &path, 0, &STANDARD.encode(b"first ")).unwrap(), 6);
        assert_eq!(append(&root, &path, 6, &STANDARD.encode(b"second")).unwrap(), 12);
        assert_eq!(append(&root, &path, 12, "").unwrap(), 12);
        assert_eq!(fs::read(&path).unwrap(), b"first second");

        assert!(cleanup_handoff_path(&root, &path, PREFIX, SUFFIX, LABEL).unwrap());
        assert!(!path.exists());
        assert!(!cleanup_handoff_path(&root, &path, PREFIX, SUFFIX, LABEL).unwrap());
    }

    #[test]
    fn a_repeated_or_skipped_piece_is_refused_without_writing() {
        let (_directory, root) = fixture();
        let path = PathBuf::from(create(&root).unwrap());
        append(&root, &path, 0, &STANDARD.encode(b"abc")).unwrap();

        assert_eq!(code(append(&root, &path, 0, &STANDARD.encode(b"abc"))), "invalid-state");
        assert_eq!(code(append(&root, &path, 4, &STANDARD.encode(b"d"))), "invalid-state");
        assert_eq!(fs::read(&path).unwrap(), b"abc");
    }

    #[test]
    fn a_full_piece_fits_and_a_larger_or_malformed_piece_is_refused() {
        let (_directory, root) = fixture();
        let path = PathBuf::from(create(&root).unwrap());
        let full = vec![7u8; MAX_PIECE_BYTES];
        let encoded = STANDARD.encode(&full);
        assert_eq!(encoded.len(), MAX_ENCODED_PIECE_BYTES);
        // Room for the path, offset and the request envelope.
        assert!(MAX_ENCODED_PIECE_BYTES + 3 * 1024 <= 4 * 1024 * 1024);
        assert_eq!(append(&root, &path, 0, &encoded).unwrap(), MAX_PIECE_BYTES as u64);

        let larger = STANDARD.encode(vec![7u8; MAX_PIECE_BYTES + 3]);
        let offset = MAX_PIECE_BYTES as u64;
        assert_eq!(code(append(&root, &path, offset, &larger)), "invalid-input");
        assert_eq!(code(append(&root, &path, offset, "not base64!")), "invalid-input");
        assert_eq!(fs::metadata(&path).unwrap().len(), offset);
    }

    #[test]
    fn appends_reach_only_an_existing_owned_download_handoff() {
        let (directory, root) = fixture();
        let created = PathBuf::from(create(&root).unwrap());
        let handoffs = root.join("handoffs");
        let piece = STANDARD.encode(b"x");

        let other_kind = handoffs.join(format!("risu-backup-{}.bin", Uuid::new_v4()));
        fs::write(&other_kind, b"").unwrap();
        let outside = directory.path().join(format!("{PREFIX}{}{SUFFIX}", Uuid::new_v4()));
        fs::write(&outside, b"").unwrap();
        let relative = PathBuf::from(created.file_name().unwrap());
        for path in [&other_kind, &outside, &relative] {
            assert_eq!(code(append(&root, path, 0, &piece)), "invalid-input", "{path:?}");
        }
        assert_eq!(fs::read(&other_kind).unwrap(), b"");
        assert_eq!(fs::read(&outside).unwrap(), b"");

        fs::remove_file(&created).unwrap();
        assert_eq!(code(append(&root, &created, 0, &piece)), "invalid-state");
        assert!(!created.exists());
    }
}
