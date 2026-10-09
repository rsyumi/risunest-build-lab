use super::*;

#[test]
fn a_plain_path_is_opened_as_given() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("plain.sqlite");
    assert_eq!(sqlite_path(&path), path.as_path());
    let opened = open(&path).unwrap();
    #[cfg(windows)]
    assert_eq!(opened.path().unwrap(), path.to_str().unwrap());
    drop(opened);
    assert!(path.is_file());
}

#[cfg(windows)]
#[test]
fn a_verbatim_path_opens_by_its_drive_path_before_and_after_the_file_exists() {
    let directory = tempfile::tempdir().unwrap();
    let verbatim = std::fs::canonicalize(directory.path()).unwrap().join("store.sqlite");
    assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));
    let plain = Path::new(&verbatim.to_str().unwrap()[4..]);
    let creating = open(&verbatim).unwrap();
    assert_eq!(creating.path().unwrap(), plain.to_str().unwrap());
    let reading = open_with_flags(&verbatim, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(reading.path().unwrap(), plain.to_str().unwrap());
}

#[cfg(windows)]
#[test]
fn a_verbatim_path_that_win32_would_rename_is_kept() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let dotted = root.join("trailing.");
    std::fs::write(&dotted, []).unwrap();
    assert_eq!(sqlite_path(&dotted), dotted.as_path());
    let missing = root.join("missing.");
    assert_eq!(sqlite_path(&missing), missing.as_path());
}

#[cfg(windows)]
#[test]
fn only_a_short_drive_path_loses_its_prefix() {
    let long = format!(r"\\?\C:\{}\store.sqlite", "d".repeat(250));
    assert_eq!(sqlite_path(Path::new(&long)), Path::new(&long));
    for kept in [r"\\?\UNC\server\share\store.sqlite", r"\\server\share\store.sqlite", r"\\.\C:\store.sqlite"] {
        assert_eq!(sqlite_path(Path::new(kept)), Path::new(kept));
    }
}
