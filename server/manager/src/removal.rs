use crate::{client::Client, lifecycle, platform, update, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

const RECORD: &str = "risunest-sync-instance.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Registration {
    schema: u32,
    data: PathBuf,
    install: PathBuf,
    server: PathBuf,
    instance: String,
    linger_changed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalPlan {
    pub data: PathBuf,
    pub install: PathBuf,
    pub delete_data: bool,
    pub files: Vec<PathBuf>,
    pub registrations: Vec<String>,
    pub launcher: Option<PathBuf>,
    pub blockers: Vec<String>,
    pub linger_preserved: bool,
    pub remove_gui_profile: bool,
}

fn registry() -> Result<PathBuf> {
    Ok(platform::default_data_dir()?
        .parent()
        .ok_or("removal-registry-unavailable")?
        .join(".risunest-sync-instances"))
}

pub fn install_directory(server: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Some(bundle) = server
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
    {
        return Ok(bundle.to_owned());
    }
    server
        .parent()
        .map(Path::to_owned)
        .ok_or("managed-install-path-invalid".into())
}

fn marker(install: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        install.join("Contents/Resources/risunest-sync-bundle.json")
    } else {
        install.join("risunest-sync-bundle.json")
    }
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn safe_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("removal-path-invalid".into());
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        if matches!(part, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if linked(&meta) => return Err("removal-linked-path".into()),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("removal-path-unavailable".into()),
        }
    }
    Ok(())
}

fn safe_tree(path: &Path) -> Result<()> {
    safe_path(path)?;
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| "removal-path-unavailable")? {
            safe_tree(&entry.map_err(|_| "removal-path-unavailable")?.path())?;
        }
    }
    Ok(())
}

fn safe_data_tree(root: &Path) -> Result<()> {
    safe_tree(root)?;
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|_| "removal-path-unavailable")? {
        let name = entry.map_err(|_| "removal-path-unavailable")?.file_name();
        if ![
            RECORD,
            "owner.lock",
            "metadata.sqlite",
            "metadata.sqlite-wal",
            "metadata.sqlite-shm",
            "objects",
            "staging",
            "connection-state",
            "management-session",
            "network.json",
            "manager-update",
            "startup-error.txt",
        ]
        .iter()
        .any(|allowed| name == *allowed)
        {
            return Err("removal-unowned-data-files".into());
        }
    }
    Ok(())
}

fn inventory_tree(root: &Path, owned: &[PathBuf]) -> Result<()> {
    safe_path(root)?;
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|_| "removal-path-unavailable")? {
        let path = entry.map_err(|_| "removal-path-unavailable")?.path();
        safe_path(&path)?;
        if path.is_dir() {
            inventory_tree(&path, owned)?;
        } else if !owned.contains(&path)
            && !(cfg!(windows) && path.file_name().is_some_and(|n| n == "uninstall.exe"))
        {
            return Err("removal-unowned-install-files".into());
        }
    }
    Ok(())
}

fn read_record(path: &Path) -> Result<Registration> {
    safe_path(path)?;
    serde_json::from_slice(&fs::read(path).map_err(|_| "removal-registration-missing")?)
        .map_err(|_| "removal-registration-invalid".into())
}

fn record_path(registry: &Path, root: &Path) -> PathBuf {
    registry.join(format!("{}.json", platform::instance_name(root)))
}

pub fn register(root: &Path, server: &Path) -> Result<()> {
    let install = install_directory(server)?;
    if !marker(&install).exists() {
        return Ok(());
    }
    update::managed_bundle_version(&install)?;
    register_at(&registry()?, root, server, &install)
}

fn register_at(registry: &Path, root: &Path, server: &Path, install: &Path) -> Result<()> {
    for path in [registry, root, server, install] {
        safe_path(path)?;
    }
    if root.starts_with(install) || install.starts_with(root) || root.parent().is_none() {
        return Err("removal-overlapping-roots".into());
    }
    let mut record = Registration {
        schema: 1,
        data: root.to_owned(),
        install: install.to_owned(),
        server: server.to_owned(),
        instance: platform::instance_name(root),
        linger_changed: std::env::var("RISUNEST_SYNC_LINGER_CHANGED")
            .is_ok_and(|value| value == "true"),
    };
    let local = root.join(RECORD);
    if local.exists() {
        let mut previous = read_record(&local)?;
        record.linger_changed |= previous.linger_changed;
        previous.linger_changed = record.linger_changed;
        if previous != record {
            return Err("removal-registration-conflict".into());
        }
    }
    update::write_json(&local, &record, "removal-registration-write-failed")?;
    update::write_json(
        &record_path(registry, root),
        &record,
        "removal-registration-write-failed",
    )
}

pub fn plan(root: &Path, server: &Path, delete_data: bool) -> Result<RemovalPlan> {
    plan_at(&registry()?, root, server, delete_data)
}

fn plan_at(registry: &Path, root: &Path, server: &Path, delete_data: bool) -> Result<RemovalPlan> {
    let install = install_directory(server)?;
    for path in [registry, root, server, &install] {
        safe_path(path)?;
    }
    let mut plan = RemovalPlan {
        data: root.to_owned(),
        install: install.clone(),
        delete_data,
        files: vec![],
        registrations: vec![
            platform::instance_name(root),
            format!("{}-update", platform::instance_name(root)),
            format!("{}-gui", platform::instance_name(root)),
        ],
        launcher: None,
        blockers: vec![],
        linger_preserved: cfg!(target_os = "linux"),
        remove_gui_profile: delete_data,
    };
    let record = read_record(&record_path(registry, root))?;
    if record.schema != 1
        || record.data != root
        || record.install != install
        || record.server != server
        || record.instance != platform::instance_name(root)
        || root.starts_with(&install)
        || install.starts_with(root)
    {
        return Err("removal-registration-invalid".into());
    }
    if root.exists() && read_record(&root.join(RECORD))? != record {
        return Err("removal-registration-conflict".into());
    }
    for entry in fs::read_dir(registry).map_err(|_| "removal-registry-unavailable")? {
        let path = entry.map_err(|_| "removal-registry-unavailable")?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let other = read_record(&path)?;
        if other.data != root {
            plan.remove_gui_profile = false;
        }
        if other.data != root
            && (other.install == install
                || other.data.starts_with(root)
                || root.starts_with(&other.data))
        {
            plan.blockers
                .push("removal-shared-installation-or-data".into());
        }
    }
    if root.join("manager-update/transaction.json").exists() {
        plan.blockers.push("update-recovery-required".into());
    }
    let inventory: serde_json::Value = serde_json::from_slice(
        &fs::read(marker(&install)).map_err(|_| "managed-install-marker-missing")?,
    )
    .map_err(|_| "managed-install-marker-invalid")?;
    let version = inventory["version"]
        .as_str()
        .ok_or("managed-install-marker-invalid")?;
    plan.files = update::validate_removal_inventory(&install, version, true)?
        .into_iter()
        .map(|p| install.join(p))
        .collect();
    for file in &plan.files {
        safe_path(file)?;
    }
    if let Err(error) = inventory_tree(&install, &plan.files) {
        plan.blockers.push(error);
    }
    if delete_data {
        safe_data_tree(root)?;
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            if install == home.join(".local/lib/risunest-sync") {
                let launcher = home.join(".local/bin/risunest-sync-manager");
                safe_path(&launcher)?;
                if launcher.exists() {
                    let expected = "#!/bin/sh\n# RisuNest sync manager launcher\nexec \"$HOME/.local/lib/risunest-sync/risunest-sync-manager\" \"$@\"\n";
                    if fs::read_to_string(&launcher)
                        .map_err(|_| "removal-launcher-unavailable")?
                        .replace("\r\n", "\n")
                        != expected
                    {
                        plan.blockers
                            .push("removal-launcher-ownership-conflict".into());
                    } else {
                        plan.launcher = Some(launcher);
                    }
                }
            }
        }
    }
    Ok(plan)
}

pub async fn cleanup_services(root: &Path, server: &Path) -> Result<()> {
    if root.join("manager-update/transaction.json").exists() {
        return Err("update-recovery-required".into());
    }
    update::remove_schedule_while_locked(root, &platform::manager_executable()?, server)?;
    lifecycle::stop(root, &Client::new(root.to_owned())?).await?;
    platform::remove_startup(root, server)?;
    #[cfg(any(windows, target_os = "macos"))]
    platform::gui::remove(root)?;
    #[cfg(not(windows))]
    platform::cleanup_update_helpers(root)?;
    Ok(())
}

fn remove_file(path: &Path) -> Result<()> {
    safe_path(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(format!("removal-file-failed: {}", path.display())),
    }
}

pub fn delete_registered_data(root: &Path, server: &Path) -> Result<()> {
    let registry = registry()?;
    let record = read_record(&record_path(&registry, root))?;
    if record.schema != 1
        || record.data != root
        || record.server != server
        || record.install != install_directory(server)?
        || record.instance != platform::instance_name(root)
        || root.starts_with(&record.install)
        || record.install.starts_with(root)
    {
        return Err("removal-registration-invalid".into());
    }
    if root.exists() && read_record(&root.join(RECORD))? != record {
        return Err("removal-registration-conflict".into());
    }
    let mut shared_profile = false;
    for entry in fs::read_dir(&registry).map_err(|_| "removal-registry-unavailable")? {
        let path = entry.map_err(|_| "removal-registry-unavailable")?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let other = read_record(&path)?;
        if other.data == root {
            continue;
        }
        if other.data.starts_with(root)
            || root.starts_with(&other.data)
            || other.install == record.install
        {
            return Err("removal-shared-installation-or-data".into());
        }
        shared_profile = true;
    }
    if lifecycle::owner_active(root)? || update::has_active_activity(root)? {
        return Err("removal-process-still-running".into());
    }
    if root.join("manager-update/transaction.json").exists() {
        return Err("update-recovery-required".into());
    }
    safe_data_tree(root)?;
    if !shared_profile {
        let profiles = platform::webview_data_dir()?
            .into_iter()
            .chain(platform::identifier_directories()?);
        for profile in profiles {
            safe_tree(&profile)?;
            if profile.exists() {
                fs::remove_dir_all(&profile).map_err(|_| "removal-profile-failed")?;
            }
        }
    }
    platform::cleanup_update_helpers(root)?;
    remove_data_files(root)?;
    Ok(())
}

fn remove_data_files(root: &Path) -> Result<()> {
    safe_data_tree(root)?;
    if root.exists() {
        for entry in fs::read_dir(root).map_err(|_| "removal-data-failed")? {
            let path = entry.map_err(|_| "removal-data-failed")?.path();
            if path == root.join(RECORD) {
                continue;
            }
            safe_tree(&path)?;
            if path.is_dir() {
                fs::remove_dir_all(&path).map_err(|_| "removal-data-failed")?;
            } else {
                remove_file(&path)?;
            }
        }
        remove_file(&root.join(RECORD))?;
        fs::remove_dir(root).map_err(|_| "removal-data-failed")?;
    }
    Ok(())
}

pub fn forget_registration(root: &Path, server: &Path) -> Result<()> {
    let registry = registry()?;
    let path = record_path(&registry, root);
    if !path.exists() {
        return Ok(());
    }
    let record = read_record(&path)?;
    if record.schema != 1
        || record.data != root
        || record.server != server
        || record.install != install_directory(server)?
        || record.instance != platform::instance_name(root)
    {
        return Err("removal-registration-invalid".into());
    }
    remove_file(&path)?;
    let _ = fs::remove_dir(registry);
    Ok(())
}

pub async fn execute(root: &Path, server: &Path, delete_data: bool) -> Result<()> {
    let plan = plan(root, server, delete_data)?;
    if !plan.blockers.is_empty() {
        return Err(plan.blockers.join(", "));
    }
    #[cfg(windows)]
    {
        let uninstaller = plan.install.join("uninstall.exe");
        safe_path(&uninstaller)?;
        if !uninstaller.is_file() {
            return Err("removal-use-windows-installed-uninstaller".into());
        }
        let mut command = platform::process(&uninstaller);
        command.env("RISUNEST_SYNC_UNINSTALL_DATA_DIR", root);
        if delete_data {
            command.arg("/DELETEAPPDATA");
        }
        command
            .spawn()
            .map_err(|_| "removal-uninstaller-launch-failed")?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let lock = update::try_lock(root)?;
        if update::has_active_activity(root)? {
            return Err("removal-close-other-managers".into());
        }
        cleanup_services(root, server).await?;
        #[cfg(target_os = "macos")]
        if plan.remove_gui_profile {
            clear_macos_profile(&plan.install)?;
        }
        if let Some(launcher) = &plan.launcher {
            remove_file(launcher)?;
        }
        drop(lock);
        if delete_data {
            delete_registered_data(root, server)?;
        }
        // Retain every binary until data cleanup succeeds so retries can clear the GUI profile.
        let manager = server.with_file_name("risunest-sync-manager");
        let inventory = marker(&plan.install);
        for file in &plan.files {
            if file != &manager && file != &inventory {
                remove_file(file)?;
            }
        }
        for file in [&manager, &inventory] {
            remove_file(file)?;
        }
        remove_file(&record_path(&registry()?, root))?;
        let _ = fs::remove_dir(registry()?);
        remove_empty_directories(&plan.install)?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn clear_macos_profile(install: &Path) -> Result<()> {
    let gui = install.join("Contents/MacOS/risunest-sync-gui");
    safe_path(&gui)?;
    if !gui.is_file() {
        return Err("removal-profile-gui-missing".into());
    }
    let mut child = platform::process(&gui)
        .arg("--clear-removal-profile")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "removal-profile-launch-failed")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        match child
            .try_wait()
            .map_err(|_| "removal-profile-wait-failed")?
        {
            Some(status) if status.success() => return Ok(()),
            Some(_) => return Err("removal-profile-failed".into()),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("removal-profile-timeout".into());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

#[cfg(not(windows))]
fn remove_empty_directories(path: &Path) -> Result<()> {
    safe_path(path)?;
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path).map_err(|_| "removal-path-unavailable")? {
        let entry = entry.map_err(|_| "removal-path-unavailable")?;
        if entry
            .file_type()
            .map_err(|_| "removal-path-unavailable")?
            .is_dir()
        {
            remove_empty_directories(&entry.path())?;
        }
    }
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            Err("removal-unowned-files-remain".into())
        }
        Err(_) => Err("removal-directory-failed".into()),
    }
}

pub async fn cli(root: &Path, server: &Path, arguments: &[String]) -> Result<()> {
    let mut delete_data = false;
    let mut dry_run = false;
    let mut yes = false;
    for argument in arguments {
        match argument.as_str() {
            "--delete-data" => delete_data = true,
            "--dry-run" => dry_run = true,
            "--yes" => yes = true,
            _ => return Err("invalid-uninstall-option".into()),
        }
    }
    let preview = plan(root, server, delete_data)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&preview).map_err(|_| "removal-preview-failed")?
    );
    if !preview.blockers.is_empty() {
        return Err(preview.blockers.join(", "));
    }
    if dry_run {
        return Ok(());
    }
    if !yes {
        use std::io::{self, IsTerminal, Write};
        if !io::stdin().is_terminal() {
            return Err("removal-confirmation-required-use-yes".into());
        }
        print!("RisuNest Sync를 제거하시겠습니까? 계속하려면 REMOVE를 입력하세요: ");
        io::stdout().flush().map_err(|_| "terminal-write-failed")?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|_| "terminal-read-failed")?;
        if answer.trim() != "REMOVE" {
            return Err("removal-cancelled".into());
        }
    }
    execute(root, server, delete_data).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("data");
        let install = base.join("install");
        let registry = base.join("registry");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&install).unwrap();
        let server = install.join("server");
        fs::write(&server, "synthetic").unwrap();
        let bundle = marker(&install);
        fs::create_dir_all(bundle.parent().unwrap()).unwrap();
        fs::write(bundle, serde_json::to_vec(&serde_json::json!({"schema":"risunest-sync-bundle/v1", "product":"sync", "variant":"managed", "version":"1.0.0", "protocolId":risunest_sync_server::PROTOCOL_ID,"storeFormatId":risunest_sync_server::STORE_FORMAT_ID,"files":["server"],"vendor":[]})).unwrap()).unwrap();
        register_at(&registry, &root, &server, &install).unwrap();
        (temp, root, server, registry)
    }
    #[test]
    fn preview_preserves_data_and_does_not_create_update_state() {
        let (_temp, root, server, registry) = fixture();
        fs::write(root.join("synthetic"), "keep").unwrap();
        let plan = plan_at(&registry, &root, &server, false).unwrap();
        assert!(!plan.delete_data);
        assert!(plan.blockers.is_empty());
        assert_eq!(fs::read(root.join("synthetic")).unwrap(), b"keep");
        assert!(!root.join("manager-update").exists());
    }
    #[test]
    fn missing_registration_cannot_create_or_delete_a_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("absent");
        assert!(plan_at(
            &temp.path().join("registry"),
            &root,
            &temp.path().join("install/server"),
            true
        )
        .is_err());
        assert!(!root.exists());
        assert!(!temp.path().join("registry").exists());
    }
    #[test]
    fn shared_install_and_unfinished_transaction_block_removal() {
        let (_temp, root, server, registry) = fixture();
        let other = root.with_file_name("other");
        fs::create_dir(&other).unwrap();
        register_at(&registry, &other, &server, server.parent().unwrap()).unwrap();
        fs::create_dir(root.join("manager-update")).unwrap();
        fs::write(root.join("manager-update/transaction.json"), "{}").unwrap();
        let plan = plan_at(&registry, &root, &server, true).unwrap();
        assert!(plan
            .blockers
            .contains(&"removal-shared-installation-or-data".into()));
        assert!(plan.blockers.contains(&"update-recovery-required".into()));
    }
    #[test]
    fn missing_inventory_files_are_retryable_but_parent_paths_are_rejected() {
        let (_temp, root, server, registry) = fixture();
        fs::remove_file(&server).unwrap();
        assert!(plan_at(&registry, &root, &server, false).is_ok());
        let path = marker(server.parent().unwrap());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["files"] = serde_json::json!(["../outside"]);
        fs::write(path, value.to_string()).unwrap();
        assert!(plan_at(&registry, &root, &server, true).is_err());
    }
    #[test]
    fn deletes_owned_data_but_preserves_external_exports_and_other_roots() {
        let (temp, root, _server, _registry) = fixture();
        fs::create_dir(root.join("objects")).unwrap();
        fs::write(root.join("objects/blob"), "synthetic").unwrap();
        fs::write(root.join("metadata.sqlite"), "synthetic").unwrap();
        fs::write(temp.path().join("export.zip"), "external").unwrap();
        remove_data_files(&root).unwrap();
        assert!(!root.exists());
        assert!(temp.path().join("export.zip").exists());
        remove_data_files(&root).unwrap();
    }
    #[test]
    fn unknown_data_or_install_files_prevent_destructive_removal() {
        let (_temp, root, server, registry) = fixture();
        fs::write(root.join("unrelated.txt"), "keep").unwrap();
        assert!(plan_at(&registry, &root, &server, true).is_err());
        assert!(root.join("unrelated.txt").exists());
        fs::write(server.with_file_name("unrelated.txt"), "keep").unwrap();
        assert!(plan_at(&registry, &root, &server, false)
            .unwrap()
            .blockers
            .contains(&"removal-unowned-install-files".into()));
    }
    #[cfg(windows)]
    #[test]
    fn locked_data_keeps_ownership_record_for_retry() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, root, _server, _registry) = fixture();
        let path = root.join("metadata.sqlite");
        fs::write(&path, "synthetic").unwrap();
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(remove_data_files(&root).is_err());
        assert!(root.join(RECORD).exists());
        drop(handle);
        remove_data_files(&root).unwrap();
        assert!(!root.exists());
    }
    #[cfg(unix)]
    #[test]
    fn linked_data_is_never_traversed() {
        let (temp, root, server, registry) = fixture();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "keep").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(plan_at(&registry, &root, &server, true).is_err());
        assert!(outside.join("keep").exists());
    }
}

#[cfg(not(windows))]
pub fn spawn_after_exit(root: &Path, server: &Path, delete_data: bool) -> Result<()> {
    let preview = plan(root, server, delete_data)?;
    if !preview.blockers.is_empty() {
        return Err(preview.blockers.join(", "));
    }
    let temporary = tempfile::Builder::new()
        .prefix("risunest-sync-removal-")
        .tempdir()
        .map_err(|_| "removal-helper-unavailable")?;
    let helper = temporary.path().join("risunest-sync-manager");
    fs::copy(platform::manager_executable()?, &helper).map_err(|_| "removal-helper-unavailable")?;
    let output = fs::File::create(temporary.path().join("result.txt"))
        .map_err(|_| "removal-helper-unavailable")?;
    let mut command = platform::process(&helper);
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    command
        .arg("--data-dir")
        .arg(root)
        .arg("--server")
        .arg(server)
        .arg("removal-helper")
        .arg(std::process::id().to_string())
        .arg(if delete_data { "delete" } else { "preserve" })
        .stdin(std::process::Stdio::null())
        .stdout(
            output
                .try_clone()
                .map_err(|_| "removal-helper-unavailable")?,
        )
        .stderr(output);
    command.spawn().map_err(|_| "removal-helper-unavailable")?;
    let _ = temporary.keep();
    Ok(())
}

#[cfg(not(windows))]
pub async fn finish_after_exit(
    root: &Path,
    server: &Path,
    parent: u32,
    delete_data: bool,
) -> Result<()> {
    let helper = std::env::current_exe().map_err(|_| "removal-helper-unavailable")?;
    let directory = helper.parent().ok_or("removal-helper-unavailable")?;
    if helper
        .file_name()
        .is_none_or(|n| n != "risunest-sync-manager")
        || directory
            .file_name()
            .is_none_or(|n| !n.to_string_lossy().starts_with("risunest-sync-removal-"))
    {
        return Err("removal-helper-path-invalid".into());
    }
    let result = async {
        platform::wait_for_parent_exit(parent, std::time::Duration::from_secs(60))?;
        execute(root, server, delete_data).await
    }
    .await;
    if let Err(error) = &result {
        eprintln!("RisuNest Sync removal failed: {error}");
    }
    #[cfg(target_os = "macos")]
    {
        let text = match &result {
            Ok(()) if delete_data => "RisuNest Sync와 서버 데이터를 제거했습니다.".to_owned(),
            Ok(()) => "RisuNest Sync를 제거했습니다. 서버 데이터는 유지됩니다.".to_owned(),
            Err(_) => format!("RisuNest Sync 제거를 완료하지 못했습니다. 다시 시도할 수 있도록 제거 도구를 보존했습니다.\n결과 파일: {}", directory.join("result.txt").display()),
        };
        // Pass text as an argument so filesystem characters never become AppleScript source.
        let status = platform::process("/usr/bin/osascript")
            .args(["-e", "on run argv\ndisplay dialog (item 1 of argv) with title \"RisuNest Sync\" buttons {\"확인\"} default button 1\nend run", "--"])
            .arg(text).status().map_err(|_| "removal-result-dialog-failed")?;
        if !status.success() {
            return Err("removal-result-dialog-failed".into());
        }
    }
    result?;
    remove_file(&helper)?;
    remove_file(&directory.join("result.txt"))?;
    fs::remove_dir(directory).map_err(|_| "removal-helper-cleanup-failed".into())
}
