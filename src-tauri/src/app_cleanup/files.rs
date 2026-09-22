use std::{fs, io, path::{Component, Path, PathBuf}};

pub(super) type Result<T> = std::result::Result<T, String>;

pub(super) fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    { metadata.file_type().is_symlink() }
}

/// A cleanup root must be an absolute, dedicated path with no redirected ancestors.
pub(super) fn validate(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.parent().and_then(Path::parent).is_none()
        || path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)) {
        return Err("cleanup-path-invalid".into());
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if linked(&metadata) => return Err("cleanup-path-redirected".into()),
            Ok(_) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(_) => return Err("cleanup-path-unavailable".into()),
        }
    }
    Ok(())
}

pub(super) fn remove(path: &Path) -> Result<()> {
    validate(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("cleanup-path-unavailable".into()),
    };
    let result = if metadata.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    result.map_err(|_| "cleanup-files-busy-or-denied".to_owned())
}

pub(super) fn independent_roots(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    let all = paths.clone();
    paths.retain(|path| !all.iter().any(|other| other != path && path.starts_with(other)));
    paths
}

pub(super) fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    validate(path)?;
    let parent = path.parent().ok_or("cleanup-path-invalid")?;
    fs::create_dir_all(parent).map_err(|_| "cleanup-journal-unavailable")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| "cleanup-journal-unavailable")?;
    serde_json::to_writer(file.as_file_mut(), value).map_err(|_| "cleanup-journal-unavailable")?;
    file.as_file().sync_all().map_err(|_| "cleanup-journal-unavailable")?;
    file.persist(path).map_err(|_| "cleanup-journal-unavailable")?;
    #[cfg(unix)]
    fs::File::open(parent).and_then(|directory| directory.sync_all())
        .map_err(|_| "cleanup-journal-unavailable")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_replacement_keeps_a_complete_retry_record() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("control/request.json");
        write_json(&journal, &serde_json::json!({"pending": true, "error": null})).unwrap();
        let retry = serde_json::json!({"pending": true, "error": "cleanup-files-busy-or-denied"});
        write_json(&journal, &retry).unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&fs::read(&journal).unwrap()).unwrap(), retry);
        assert_eq!(fs::read_dir(journal.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn removes_only_selected_roots_and_repeated_cleanup_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let owned = dir.path().join("owned");
        let exported = dir.path().join("exported");
        fs::create_dir_all(owned.join("snapshots")).unwrap();
        fs::write(owned.join("snapshots/data"), b"synthetic").unwrap();
        fs::write(&exported, b"synthetic export").unwrap();
        remove(&owned).unwrap();
        remove(&owned).unwrap();
        assert!(!owned.exists());
        assert_eq!(fs::read(exported).unwrap(), b"synthetic export");
    }

    #[test]
    fn validates_every_root_before_mutation_and_collapses_nested_roots() {
        let dir = tempfile::tempdir().unwrap();
        let owned = dir.path().join("owned");
        assert!(validate(&owned.join(".." )).is_err());
        assert!(validate(Path::new("relative/data")).is_err());
        assert_eq!(independent_roots(vec![owned.join("logs"), owned.clone(), owned.clone()]), vec![owned]);
    }

    #[cfg(unix)]
    #[test]
    fn redirected_root_is_rejected_and_nested_links_do_not_delete_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let owned = dir.path().join("owned");
        std::os::unix::fs::symlink(outside.path(), &owned).unwrap();
        assert!(remove(&owned).is_err());
        fs::remove_file(&owned).unwrap();
        fs::create_dir(&owned).unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), owned.join("link")).unwrap();
        remove(&owned).unwrap();
        assert!(outside.path().join("keep").exists());
    }
}
