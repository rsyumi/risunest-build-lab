use crate::{client::Client, lifecycle, platform, update, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const RECORD: &str = "risunest-sync-instance.json";
/// The marker of the replaced bundle, kept by an NSIS install until obsolete files are removed.
const INSTALL_BASELINE: &str = "risunest-sync-bundle.previous.json";
/// Antivirus scans and exiting helper processes briefly hold files that are free to remove.
const TRANSIENT_REMOVAL_BUDGET: Duration = Duration::from_secs(5);
#[cfg(windows)]
const REMOVAL_RETRY: &str = ".risunest-sync-removal";

#[cfg(windows)]
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinalizationReceipt {
    schema: u32,
    data: PathBuf,
    server: PathBuf,
    registration: Option<Registration>,
}

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
    pub data_kept_without_registration: bool,
    pub files: Vec<PathBuf>,
    pub registrations: Vec<String>,
    pub launcher: Option<PathBuf>,
    pub blockers: Vec<String>,
    pub linger_preserved: bool,
    pub remove_gui_profile: bool,
}

fn registry() -> Result<PathBuf> {
    registry_for_default(&platform::default_data_dir()?)
}

fn registry_for_default(default: &Path) -> Result<PathBuf> {
    platform::resolve_manager_root(
        &default
            .parent()
            .ok_or("removal-registry-unavailable")?
            .join(".risunest-sync-instances"),
    )
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

fn safe_profile_tree(path: &Path) -> Result<()> {
    safe_path(path)?;
    if path.file_name().is_some_and(|name| {
        ["metadata.sqlite", "owner.lock", RECORD]
            .iter()
            .any(|reserved| name == *reserved)
    }) {
        return Err("removal-unowned-profile-files".into());
    }
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| "removal-path-unavailable")? {
            safe_profile_tree(&entry.map_err(|_| "removal-path-unavailable")?.path())?;
        }
    }
    Ok(())
}

fn remove_profiles(profiles: &[PathBuf]) -> Result<()> {
    for profile in profiles {
        safe_profile_tree(profile)?;
    }
    for profile in profiles {
        if profile.exists() {
            fs::remove_dir_all(profile).map_err(|_| "removal-profile-failed")?;
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
            && !name
                .to_str()
                .is_some_and(|name| name.starts_with(".risunest-tmp-"))
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
    let bytes = fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "removal-registration-missing".to_owned()
        } else {
            format!("removal-registration-read-failed: {}", path.display())
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|_| "removal-registration-invalid".into())
}

fn record_path(registry: &Path, root: &Path) -> PathBuf {
    registry.join(format!("{}.json", platform::instance_name(root)))
}

pub fn register(root: &Path, server: &Path) -> Result<()> {
    let install = install_directory(server)?;
    if !marker(&install).exists() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        if install
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation")
        {
            return Err("removal-installation-not-writable".into());
        }
        let path = std::ffi::CString::new(install.as_os_str().as_bytes())
            .map_err(|_| "removal-path-invalid")?;
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err("removal-path-unavailable".into());
        }
        if unsafe { info.assume_init() }.f_flag & libc::ST_RDONLY as libc::c_ulong != 0 {
            return Err("removal-installation-not-writable".into());
        }
    }
    update::managed_bundle_version(&install)?;
    register_at_with(&registry()?, root, server, &install, || {
        platform::reconcile_relocated_registration(root, server)
    })
}

#[cfg(test)]
fn register_at(registry: &Path, root: &Path, server: &Path, install: &Path) -> Result<()> {
    register_at_with(registry, root, server, install, || Ok(()))
}

fn register_at_with(
    registry: &Path,
    root: &Path,
    server: &Path,
    install: &Path,
    reconcile: impl FnOnce() -> Result<()>,
) -> Result<()> {
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
    let mut relocation_lock = None;
    if local.exists() {
        let mut previous = read_record(&local)?;
        record.linger_changed |= previous.linger_changed;
        previous.linger_changed = record.linger_changed;
        if previous != record {
            let old_install = previous.install.clone();
            previous.install = record.install.clone();
            previous.server = record.server.clone();
            if previous != record || marker(&old_install).exists() {
                return Err("removal-registration-conflict".into());
            }
            relocation_lock = Some(update::try_lock(root)?);
            if root.join("manager-update/transaction.json").exists() {
                return Err("update-recovery-required".into());
            }
            if lifecycle::owner_active(root)? {
                return Err("removal-process-still-running".into());
            }
        }
    }
    let _relocation_lock = relocation_lock;
    if _relocation_lock.is_some() {
        reconcile()?;
    }
    update::write_json(&local, &record, "removal-registration-write-failed")
        .map_err(|error| format!("{error}: {}", local.display()))?;
    let path = record_path(registry, root);
    update::write_json(&path, &record, "removal-registration-write-failed")
        .map_err(|error| format!("{error}: {}", path.display()))
}

pub fn plan(root: &Path, server: &Path, delete_data: bool) -> Result<RemovalPlan> {
    plan_at(&registry()?, root, server, delete_data)
}

fn plan_at(registry: &Path, root: &Path, server: &Path, delete_data: bool) -> Result<RemovalPlan> {
    plan_at_with_force(registry, root, server, delete_data, false)
}

pub fn plan_with_force(
    root: &Path,
    server: &Path,
    delete_data: bool,
    force: bool,
) -> Result<RemovalPlan> {
    plan_at_with_force(&registry()?, root, server, delete_data, force)
}

fn plan_at_with_force(
    registry: &Path,
    root: &Path,
    server: &Path,
    delete_data: bool,
    force: bool,
) -> Result<RemovalPlan> {
    let install = install_directory(server)?;
    for path in [registry, root, server, &install] {
        safe_path(path)?;
    }
    let mut plan = RemovalPlan {
        data: root.to_owned(),
        install: install.clone(),
        delete_data,
        data_kept_without_registration: false,
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
    if root.starts_with(&install) || install.starts_with(root) {
        return Err("removal-registration-invalid".into());
    }
    let mut records = Vec::new();
    for path in [record_path(registry, root), root.join(RECORD)] {
        match read_record(&path) {
            Ok(record) => {
                if record.schema != 1
                    || record.data != root
                    || record.install != install
                    || record.server != server
                    || record.instance != platform::instance_name(root)
                {
                    return Err("removal-registration-invalid".into());
                }
                records.push(record);
            }
            Err(error)
                if error == "removal-registration-missing"
                    && (force || (!root.exists() && !records.is_empty())) =>
            {
                ()
            }
            Err(error) => return Err(error),
        }
    }
    if records.len() == 2 && records[0] != records[1] {
        return Err("removal-registration-conflict".into());
    }
    if records.len() < 2 && force {
        plan.data_kept_without_registration = delete_data;
        plan.delete_data = false;
        plan.remove_gui_profile = false;
    }
    let entries = match fs::read_dir(registry) {
        Ok(entries) => Some(entries),
        Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("removal-registry-unavailable".into()),
    };
    for entry in entries.into_iter().flatten() {
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
    let files = update::validate_removal_inventory(&install, version, true)?;
    #[cfg(windows)]
    let files = {
        let mut files = files;
        include_install_baseline(&install, &mut files)?;
        files
    };
    plan.files = files.into_iter().map(|p| install.join(p)).collect();
    if cfg!(windows)
        && install.join(INSTALL_BASELINE).is_file()
        && !plan.files.contains(&install.join(INSTALL_BASELINE))
    {
        plan.files.push(install.join(INSTALL_BASELINE));
    }
    for file in &plan.files {
        safe_path(file)?;
    }
    if let Err(error) = inventory_tree(&install, &plan.files) {
        if !force || error != "removal-unowned-install-files" {
            plan.blockers.push(error);
        }
    }
    if plan.delete_data {
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
    #[cfg(windows)]
    platform::sweep_stale_update_helpers(root)?;
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

fn retry_transient<T>(
    budget: Duration,
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let deadline = Instant::now() + budget;
    loop {
        match operation() {
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(5 | 32))
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

fn remove_file(path: &Path) -> Result<()> {
    remove_file_within(path, Duration::ZERO)
}

fn remove_file_within(path: &Path, budget: Duration) -> Result<()> {
    safe_path(path)?;
    match retry_transient(budget, || fs::remove_file(path)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("removal-file-failed: {} ({error})", path.display())),
    }
}

#[cfg(any(windows, test))]
fn existing_install_inventory(path: &Path) -> Result<Option<Vec<PathBuf>>> {
    safe_path(path)?;
    Ok(update::previous_owned_files(path).unwrap_or(None))
}

#[cfg(any(windows, test))]
pub fn preserve_install_baseline(server: &Path) -> Result<bool> {
    let install = install_directory(server)?;
    safe_path(&install)?;
    let current = marker(&install);
    let Some(mut files) = existing_install_inventory(&current)? else {
        return Ok(false);
    };
    let baseline = install.join(INSTALL_BASELINE);
    safe_path(&baseline)?;
    if let Some(previous) = update::previous_owned_files(&baseline)? {
        files.extend(previous);
    }
    files.sort();
    files.dedup();
    let mut inventory: serde_json::Value =
        serde_json::from_slice(&fs::read(current).map_err(|_| "managed-install-marker-invalid")?)
            .map_err(|_| "managed-install-marker-invalid")?;
    inventory["files"] =
        serde_json::to_value(files).map_err(|_| "managed-install-marker-invalid")?;
    update::write_json(&baseline, &inventory, "installer-baseline-write-failed")?;
    Ok(true)
}

#[cfg(any(windows, test))]
pub(crate) fn include_install_baseline(install: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let baseline = install.join(INSTALL_BASELINE);
    safe_path(&baseline)?;
    if let Some(previous) = update::previous_owned_files(&baseline)? {
        for relative in previous {
            safe_path(&install.join(&relative))?;
            if !files.contains(&relative) {
                files.push(relative);
            }
        }
    }
    if baseline.is_file() {
        files.push(PathBuf::from(INSTALL_BASELINE));
    }
    Ok(())
}

pub fn remove_obsolete_owned_files(server: &Path, previous: &Path) -> Result<bool> {
    let install = install_directory(server)?;
    safe_path(&install)?;
    let Some(previous) = update::previous_owned_files(previous)? else {
        return Ok(false);
    };
    let current =
        update::previous_owned_files(&marker(&install))?.ok_or("managed-install-marker-invalid")?;
    let mut obsolete = Vec::new();
    for relative in previous {
        if current.iter().any(|file| {
            if cfg!(windows) {
                file.to_string_lossy()
                    .eq_ignore_ascii_case(&relative.to_string_lossy())
            } else {
                file == &relative
            }
        }) {
            continue;
        }
        let file = install.join(relative);
        safe_path(&file)?;
        obsolete.push(file);
    }
    for file in obsolete {
        remove_file(&file)?;
    }
    Ok(true)
}

#[cfg(windows)]
pub fn remove_owned_files(root: &Path, server: &Path, force: bool) -> Result<()> {
    remove_owned_files_with(root, server, force, false)
}

#[cfg(windows)]
pub fn remove_owned_files_resumable(root: &Path, server: &Path, force: bool) -> Result<()> {
    remove_owned_files_with(root, server, force, true)
}

#[cfg(windows)]
fn remove_owned_files_with(root: &Path, server: &Path, force: bool, resumable: bool) -> Result<()> {
    let plan = plan_with_force(root, server, false, force)?;
    if !plan.blockers.is_empty() {
        return Err(plan.blockers.join(", "));
    }
    if lifecycle::owner_active(root)? {
        return Err("removal-process-still-running".into());
    }
    // These shipped command semantics must survive archive updates that retain uninstall.exe.
    if !resumable {
        return remove_released_files(&plan, TRANSIENT_REMOVAL_BUDGET);
    }
    remove_planned_files_with(&plan, TRANSIENT_REMOVAL_BUDGET, || {
        prepare_finalization(
            &registry()?,
            root,
            server,
            force,
            &std::env::current_exe().map_err(|_| "removal-helper-unavailable")?,
        )
    })
}

#[cfg(windows)]
fn remove_released_files(plan: &RemovalPlan, budget: Duration) -> Result<()> {
    let last = final_files(&plan.install);
    for file in plan.files.iter().filter(|file| !last.contains(file)) {
        remove_file_within(file, budget)?;
    }
    for file in last.iter().filter(|file| plan.files.contains(file)) {
        remove_file_within(file, budget)?;
    }
    Ok(())
}

#[cfg(windows)]
fn final_files(install: &Path) -> [PathBuf; 3] {
    [
        install.join("risunest-sync-manager.exe"),
        install.join("risunest-sync-manager-background.exe"),
        marker(install),
    ]
}

#[cfg(windows)]
fn finalization_directory(server: &Path) -> Result<PathBuf> {
    let directory = install_directory(server)?.join(REMOVAL_RETRY);
    safe_path(&directory)?;
    for name in [
        "manager.exe",
        "manager.exe.new",
        "receipt.json",
        "receipt.new",
        "complete",
        "complete.new",
        "data-path",
        "data-path.new",
    ] {
        safe_path(&directory.join(name))?;
    }
    Ok(directory)
}

#[cfg(windows)]
fn prepare_finalization(
    registry: &Path,
    root: &Path,
    server: &Path,
    force: bool,
    helper: &Path,
) -> Result<()> {
    let directory = finalization_directory(server)?;
    if directory.join("receipt.json").exists() || directory.join("complete").exists() {
        return Err("removal-finalization-pending".into());
    }
    let registration = match read_record(&record_path(registry, root)) {
        Ok(record) => Some(record),
        Err(error) if force && error == "removal-registration-missing" => None,
        Err(error) => return Err(error),
    };
    fs::create_dir_all(&directory).map_err(|_| "removal-helper-unavailable")?;
    let temporary = directory.join("manager.exe.new");
    fs::copy(helper, &temporary).map_err(|_| "removal-helper-copy-failed")?;
    fs::OpenOptions::new()
        .write(true)
        .open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|_| "removal-helper-copy-failed")?;
    fs::rename(&temporary, directory.join("manager.exe"))
        .map_err(|_| "removal-helper-copy-failed")?;
    use std::os::windows::ffi::OsStrExt;
    let data_path: Vec<_> = root
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    write_finalization_bytes(&directory, "data-path", &data_path)?;
    write_finalization(
        &directory,
        "receipt",
        &FinalizationReceipt {
            schema: 1,
            data: root.to_owned(),
            server: server.to_owned(),
            registration,
        },
    )
}

#[cfg(windows)]
fn write_finalization(directory: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    write_finalization_bytes(
        directory,
        name,
        &serde_json::to_vec(value).map_err(|_| "removal-finalization-write-failed")?,
    )
}

#[cfg(windows)]
fn write_finalization_bytes(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let temporary = directory.join(format!("{name}.new"));
    safe_path(&temporary)?;
    let mut file = fs::File::create(&temporary).map_err(|_| "removal-finalization-write-failed")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "removal-finalization-write-failed")?;
    drop(file);
    let target = directory.join(if name == "receipt" {
        "receipt.json"
    } else {
        name
    });
    fs::rename(temporary, target).map_err(|_| "removal-finalization-write-failed".into())
}

#[cfg(windows)]
fn read_finalization(registry: &Path, server: &Path) -> Result<FinalizationReceipt> {
    let directory = finalization_directory(server)?;
    let path = directory.join("receipt.json");
    if fs::metadata(&path)
        .map_err(|_| "removal-finalization-invalid")?
        .len()
        > 16 * 1024
    {
        return Err("removal-finalization-invalid".into());
    }
    let receipt: FinalizationReceipt =
        serde_json::from_slice(&fs::read(path).map_err(|_| "removal-finalization-invalid")?)
            .map_err(|_| "removal-finalization-invalid")?;
    let install = install_directory(server)?;
    safe_path(&receipt.data)?;
    if receipt.schema != 1
        || receipt.server != server
        || receipt.data.starts_with(&install)
        || install.starts_with(&receipt.data)
    {
        return Err("removal-finalization-invalid".into());
    }
    use std::os::windows::ffi::OsStrExt;
    let data_path: Vec<_> = receipt
        .data
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    let path = directory.join("data-path");
    if fs::metadata(&path)
        .map_err(|_| "removal-finalization-invalid")?
        .len()
        != data_path.len() as u64
        || fs::read(path).map_err(|_| "removal-finalization-invalid")? != data_path
    {
        return Err("removal-finalization-invalid".into());
    }
    if let Some(record) = &receipt.registration {
        if record.schema != 1
            || record.data != receipt.data
            || record.server != server
            || record.install != install
            || record.instance != platform::instance_name(&receipt.data)
        {
            return Err("removal-finalization-invalid".into());
        }
    }
    let current = match read_record(&record_path(registry, &receipt.data)) {
        Ok(record) => Some(record),
        Err(error) if error == "removal-registration-missing" => None,
        Err(error) => return Err(error),
    };
    // Forgetting registration is the last native step. A retry after it may only
    // clean its fixed local bookkeeping, never infer another deletion plan.
    if current != receipt.registration
        && !(current.is_none() && final_files(&install).iter().all(|file| !file.exists()))
    {
        return Err("removal-registration-conflict".into());
    }
    Ok(receipt)
}

#[cfg(windows)]
pub fn resume_finalization(server: &Path, delete_data: bool) -> Result<bool> {
    resume_finalization_at(&registry()?, server, delete_data, removal_profiles)
}

#[cfg(windows)]
fn resume_finalization_at(
    registry: &Path,
    server: &Path,
    delete_data: bool,
    profiles: impl FnOnce() -> Result<Vec<PathBuf>>,
) -> Result<bool> {
    let receipt = read_finalization(registry, server)?;
    let registered = delete_data
        && record_path(registry, &receipt.data)
            .try_exists()
            .map_err(|_| "removal-path-unavailable")?;
    let data_kept = delete_data
        && !registered
        && receipt
            .data
            .try_exists()
            .map_err(|_| "removal-path-unavailable")?;
    if delete_data && registered {
        delete_registered_data_at(registry, &receipt.data, server, profiles)?;
    }
    let _lock = if receipt.data.exists() {
        safe_tree(&receipt.data.join("manager-update"))?;
        let started = Instant::now();
        Some(loop {
            match update::try_lock(&receipt.data) {
                Ok(lock) => break lock,
                Err(_) if started.elapsed() < TRANSIENT_REMOVAL_BUDGET => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(error) => return Err(error),
            }
        })
    } else {
        None
    };
    safe_path(&receipt.data.join("owner.lock"))?;
    if lifecycle::owner_active(&receipt.data)? {
        return Err("removal-process-still-running".into());
    }
    let files = final_files(&install_directory(server)?);
    for file in &files {
        safe_path(file)?;
    }
    remove_final_files(&files, TRANSIENT_REMOVAL_BUDGET)?;
    forget_finalization_at(registry, &receipt.data, server)?;
    commit_finalization_at(registry, server)?;
    Ok(data_kept)
}

#[cfg(windows)]
pub fn commit_finalization(server: &Path) -> Result<()> {
    commit_finalization_at(&registry()?, server)
}

#[cfg(windows)]
fn commit_finalization_at(registry: &Path, server: &Path) -> Result<()> {
    let receipt = read_finalization(registry, server)?;
    if final_files(&install_directory(server)?)
        .iter()
        .any(|file| file.exists())
        || record_path(&registry, &receipt.data).exists()
    {
        return Err("removal-finalization-pending".into());
    }
    write_finalization(&finalization_directory(server)?, "complete", &true)
}

/// Keep the installed removal entry points until ordinary files are gone.
/// Open every final file for deletion before changing any disposition.
#[cfg(all(windows, test))]
fn remove_planned_files(plan: &RemovalPlan, budget: Duration) -> Result<()> {
    remove_planned_files_with(plan, budget, || Ok(()))
}

#[cfg(windows)]
fn remove_planned_files_with(
    plan: &RemovalPlan,
    budget: Duration,
    before_final: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let last = final_files(&plan.install);
    for file in &plan.files {
        let relative = file
            .strip_prefix(&plan.install)
            .map_err(|_| "managed-install-marker-invalid")?;
        let first = relative
            .components()
            .next()
            .ok_or("managed-install-marker-invalid")?;
        let first = first.as_os_str().to_string_lossy();
        if first.eq_ignore_ascii_case("uninstall.exe") || first.eq_ignore_ascii_case(REMOVAL_RETRY)
        {
            return Err("managed-install-marker-invalid".into());
        }
    }
    let same_path = |left: &Path, right: &Path| {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    };
    for file in plan
        .files
        .iter()
        .filter(|file| !last.iter().any(|last| same_path(file, last)))
    {
        remove_file_within(file, budget)?;
    }
    before_final()?;
    remove_final_files(
        &last
            .into_iter()
            .filter(|file| plan.files.iter().any(|owned| same_path(file, owned)))
            .collect::<Vec<_>>(),
        budget,
    )
}

#[cfg(windows)]
fn remove_final_files(files: &[PathBuf], budget: Duration) -> Result<()> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, DELETE, FILE_DISPOSITION_INFO,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        },
    };
    let started = Instant::now();
    let handles = loop {
        let opened = files
            .iter()
            .enumerate()
            .filter_map(|(index, path)| {
                match fs::OpenOptions::new()
                    .access_mode(DELETE)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                    .open(path)
                {
                    Ok(file) => Some(Ok((index, file))),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => Some(Err(format!(
                        "removal-file-failed: {} ({error})",
                        path.display()
                    ))),
                }
            })
            .collect::<Result<Vec<_>>>();
        match opened {
            Ok(handles) => break handles,
            Err(_) if started.elapsed() < budget => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(error),
        }
    };
    let disposition = |file: &fs::File, delete: bool| {
        let info = FILE_DISPOSITION_INFO { DeleteFile: delete };
        let result = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as HANDLE,
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        };
        if result == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    set_final_dispositions(handles.len(), budget, |index, delete| {
        disposition(&handles[index].1, delete)
    })
    .map_err(|(index, error)| {
        format!(
            "removal-file-failed: {} ({error})",
            files[handles[index].0].display()
        )
    })
}

#[cfg(windows)]
fn set_final_dispositions(
    count: usize,
    budget: Duration,
    mut disposition: impl FnMut(usize, bool) -> std::io::Result<()>,
) -> std::result::Result<(), (usize, std::io::Error)> {
    let mut failed = 0;
    retry_transient(budget, || {
        for index in 0..count {
            if let Err(error) = disposition(index, true) {
                failed = index;
                for previous in 0..index {
                    let _ = disposition(previous, false);
                }
                return Err(error);
            }
        }
        Ok(())
    })
    .map_err(|error| (failed, error))
}

fn removal_profiles() -> Result<Vec<PathBuf>> {
    Ok(platform::webview_data_dir()?
        .into_iter()
        .chain(platform::identifier_directories()?)
        .collect())
}

pub fn delete_registered_data(root: &Path, server: &Path) -> Result<()> {
    delete_registered_data_at(&registry()?, root, server, removal_profiles)
}

fn delete_registered_data_at(
    registry: &Path,
    root: &Path,
    server: &Path,
    profiles: impl FnOnce() -> Result<Vec<PathBuf>>,
) -> Result<()> {
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
        remove_profiles(&profiles()?)?;
    }
    platform::cleanup_update_helpers(root)?;
    // The installer guard publishes its release before its executable in manager-update exits.
    remove_data_files(root, TRANSIENT_REMOVAL_BUDGET)?;
    Ok(())
}

fn remove_data_files(root: &Path, budget: Duration) -> Result<()> {
    safe_data_tree(root)?;
    if root.exists() {
        for entry in fs::read_dir(root).map_err(|_| "removal-data-failed")? {
            let path = entry.map_err(|_| "removal-data-failed")?.path();
            if path == root.join(RECORD) {
                continue;
            }
            safe_tree(&path)?;
            if path.is_dir() {
                retry_transient(budget, || fs::remove_dir_all(&path))
                    .map_err(|_| "removal-data-failed")?;
            } else {
                remove_file_within(&path, budget)?;
            }
        }
        remove_file(&root.join(RECORD))?;
        fs::remove_dir(root).map_err(|_| "removal-data-failed")?;
    }
    Ok(())
}

pub fn forget_registration(root: &Path, server: &Path) -> Result<()> {
    forget_registration_at(&registry()?, root, server)
}

fn forget_registration_at(registry: &Path, root: &Path, server: &Path) -> Result<()> {
    forget_records_at(registry, root, server)
}

#[cfg(windows)]
pub fn forget_finalization(root: &Path, server: &Path) -> Result<()> {
    forget_finalization_at(&registry()?, root, server)
}

#[cfg(windows)]
fn forget_finalization_at(registry: &Path, root: &Path, server: &Path) -> Result<()> {
    let receipt = read_finalization(registry, server)?;
    if receipt.data != root {
        return Err("removal-registration-conflict".into());
    }
    if final_files(&install_directory(server)?)
        .iter()
        .any(|file| file.exists())
    {
        return Err("removal-finalization-pending".into());
    }
    forget_records_at(registry, root, server)
}

fn forget_records_at(registry: &Path, root: &Path, server: &Path) -> Result<()> {
    let path = record_path(registry, root);
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
    let local = root.join(RECORD);
    if local.exists() {
        if read_record(&local)? != record {
            return Err("removal-registration-conflict".into());
        }
        remove_file(&local)?;
    }
    remove_file(&path)?;
    let _ = fs::remove_dir(registry);
    Ok(())
}

pub async fn execute(root: &Path, server: &Path, delete_data: bool) -> Result<()> {
    execute_with_force(root, server, delete_data, false).await
}

async fn execute_with_force(
    root: &Path,
    server: &Path,
    delete_data: bool,
    force: bool,
) -> Result<()> {
    let plan = plan_with_force(root, server, delete_data, force)?;
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
        if force {
            command.arg("/FORCE");
        }
        command.spawn().map_err(|error| {
            format!(
                "removal-uninstaller-launch-failed: {} ({error})",
                uninstaller.display()
            )
        })?;
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
        forget_registration(root, server)?;
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
    let force = cfg!(windows) && arguments.iter().any(|argument| argument == "--force");
    for argument in arguments {
        match argument.as_str() {
            "--delete-data" => delete_data = true,
            "--dry-run" => dry_run = true,
            "--yes" => yes = true,
            #[cfg(windows)]
            "--force" => (),
            _ => return Err("invalid-uninstall-option".into()),
        }
    }
    let preview = plan_with_force(root, server, delete_data, force)?;
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
    execute_with_force(root, server, delete_data, force).await?;
    #[cfg(windows)]
    println!("제거 프로그램을 실행했습니다.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relocation_reconciles_under_lock_before_publishing_either_record() {
        let (_temp, root, server, registry) = fixture();
        let prior = read_record(&root.join(RECORD)).unwrap();
        fs::remove_file(marker(server.parent().unwrap())).unwrap();
        let install = server.parent().unwrap().with_file_name("moved");
        fs::create_dir(&install).unwrap();
        let moved_server = install.join("server");
        let check = || {
            assert_eq!(read_record(&root.join(RECORD)).unwrap(), prior);
            assert_eq!(read_record(&record_path(&registry, &root)).unwrap(), prior);
            assert_eq!(
                update::try_lock(&root).unwrap_err(),
                "update-already-running"
            );
        };
        assert_eq!(
            register_at_with(&registry, &root, &moved_server, &install, || {
                check();
                Err("synthetic-reconcile-failure".into())
            })
            .unwrap_err(),
            "synthetic-reconcile-failure"
        );
        assert_eq!(read_record(&root.join(RECORD)).unwrap(), prior);
        assert_eq!(read_record(&record_path(&registry, &root)).unwrap(), prior);
        register_at_with(&registry, &root, &moved_server, &install, || {
            check();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            read_record(&root.join(RECORD)).unwrap().server,
            moved_server
        );
        assert_eq!(
            read_record(&record_path(&registry, &root)).unwrap().server,
            moved_server
        );
    }

    #[cfg(unix)]
    #[test]
    fn instance_registry_resolves_a_linked_home_ancestor_without_creating_directories() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("actual-home");
        fs::create_dir(&home).unwrap();
        let alias = temp.path().join("home");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let registry = registry_for_default(&alias.join(".local/share/risunest-sync")).unwrap();
        assert_eq!(
            registry,
            home.canonicalize()
                .unwrap()
                .join(".local/share/.risunest-sync-instances")
        );
        assert!(!home.join(".local").exists());
    }
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
    #[cfg(windows)]
    #[test]
    fn interrupted_finalization_resumes_each_partial_delete_and_after_forgetting() {
        for (deleted, delete_data) in
            (0..=4).flat_map(|deleted| [false, true].map(move |delete_data| (deleted, delete_data)))
        {
            let (_temp, root, server, registry) = fixture();
            if delete_data {
                fs::remove_dir_all(&root).unwrap();
            }
            let install = server.parent().unwrap();
            let files = final_files(install);
            for file in &files {
                fs::write(file, b"synthetic").unwrap();
            }
            let helper = install.join("source-helper");
            fs::write(&helper, b"synthetic helper").unwrap();
            prepare_finalization(&registry, &root, &server, false, &helper).unwrap();
            for file in files.iter().take(deleted.min(3)) {
                fs::remove_file(file).unwrap();
            }
            if deleted == 4 {
                forget_finalization_at(&registry, &root, &server).unwrap();
                assert!(!finalization_directory(&server)
                    .unwrap()
                    .join("complete")
                    .exists());
            }
            resume_finalization_at(&registry, &server, false, || Ok(Vec::new())).unwrap();
            assert!(files.iter().all(|file| !file.exists()));
            assert!(!record_path(&registry, &root).exists());
            assert_eq!(
                fs::read(finalization_directory(&server).unwrap().join("complete")).unwrap(),
                b"true"
            );
            assert_eq!(root.exists(), !delete_data);
            resume_finalization_at(&registry, &server, false, || Ok(Vec::new())).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn finalization_rejects_replaced_receipt_and_registration_without_deleting_files() {
        let (_temp, root, server, registry) = fixture();
        let install = server.parent().unwrap();
        let files = final_files(install);
        for file in &files {
            fs::write(file, b"synthetic").unwrap();
        }
        let helper = install.join("source-helper");
        fs::write(&helper, b"synthetic helper").unwrap();
        prepare_finalization(&registry, &root, &server, false, &helper).unwrap();
        assert_eq!(
            forget_finalization_at(&registry, &root, &server).unwrap_err(),
            "removal-finalization-pending"
        );
        assert!(record_path(&registry, &root).exists());
        let receipt = finalization_directory(&server)
            .unwrap()
            .join("receipt.json");
        let original = fs::read(&receipt).unwrap();
        for malformed in [b"{".to_vec(), {
            let mut extra: serde_json::Value = serde_json::from_slice(&original).unwrap();
            extra["files"] = serde_json::json!([root.join("synthetic-user-file")]);
            serde_json::to_vec(&extra).unwrap()
        }] {
            fs::write(&receipt, malformed).unwrap();
            assert!(resume_finalization_at(&registry, &server, false, || Ok(Vec::new())).is_err());
            assert!(forget_finalization_at(&registry, &root, &server).is_err());
            assert!(record_path(&registry, &root).exists());
            assert!(files.iter().all(|file| file.exists()));
        }
        let mut altered: serde_json::Value = serde_json::from_slice(&original).unwrap();
        altered["server"] = serde_json::json!(install.join("other-server"));
        fs::write(&receipt, serde_json::to_vec(&altered).unwrap()).unwrap();
        assert!(resume_finalization_at(&registry, &server, false, || Ok(Vec::new())).is_err());
        fs::write(&receipt, &original).unwrap();
        let data_path = finalization_directory(&server).unwrap().join("data-path");
        let original_path = fs::read(&data_path).unwrap();
        let mut changed_path = original_path.clone();
        changed_path[0] ^= 1;
        fs::write(&data_path, changed_path).unwrap();
        assert!(resume_finalization_at(&registry, &server, true, || Ok(Vec::new())).is_err());
        assert!(files.iter().all(|file| file.exists()));
        assert!(root.exists());
        fs::write(&data_path, original_path).unwrap();
        fs::remove_file(record_path(&registry, &root)).unwrap();
        assert!(resume_finalization_at(&registry, &server, false, || Ok(Vec::new())).is_err());
        assert!(files.iter().all(|file| file.exists()));
    }

    #[cfg(windows)]
    #[test]
    fn removal_inventory_cannot_delete_the_retry_entry_point() {
        let (_temp, root, server, registry) = fixture();
        let mut plan = plan_at(&registry, &root, &server, false).unwrap();
        let uninstaller = plan.install.join("UNINSTALL.EXE");
        fs::write(&uninstaller, b"retry entry").unwrap();
        plan.files.push(uninstaller.clone());
        assert!(remove_planned_files(&plan, Duration::ZERO).is_err());
        assert!(server.exists() && uninstaller.exists());
        plan.files.pop();
        let manager = plan.install.join("RISUNEST-SYNC-MANAGER.EXE");
        fs::write(&manager, b"synthetic manager").unwrap();
        plan.files.push(manager.clone());
        assert!(remove_planned_files_with(&plan, Duration::ZERO, || {
            assert!(
                manager.exists(),
                "final entry remains until the receipt is ready"
            );
            Err("synthetic-before-final-stop".into())
        })
        .is_err());
        assert!(manager.exists());
    }

    #[test]
    fn profiles_are_all_validated_before_any_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("plain");
        let raw = temp.path().join("identifier");
        fs::create_dir_all(&plain).unwrap();
        fs::create_dir_all(&raw).unwrap();
        fs::write(plain.join("cache"), b"profile").unwrap();
        fs::write(raw.join("metadata.sqlite"), b"synthetic raw store").unwrap();
        assert_eq!(
            remove_profiles(&[plain.clone(), raw.clone()]).unwrap_err(),
            "removal-unowned-profile-files"
        );
        assert!(plain.join("cache").exists());
        assert!(raw.join("metadata.sqlite").exists());
        fs::remove_file(raw.join("metadata.sqlite")).unwrap();
        remove_profiles(&[plain.clone(), raw.clone()]).unwrap();
        assert!(!plain.exists() && !raw.exists());
    }

    #[test]
    fn relocation_requires_the_old_bundle_to_be_gone_and_updates_both_records() {
        let (_temp, root, server, registry) = fixture();
        let old_install = server.parent().unwrap();
        let install = old_install.with_file_name("moved");
        fs::create_dir(&install).unwrap();
        let moved_server = install.join("server");
        fs::write(&moved_server, b"synthetic").unwrap();
        assert_eq!(
            register_at(&registry, &root, &moved_server, &install).unwrap_err(),
            "removal-registration-conflict"
        );
        assert_eq!(read_record(&root.join(RECORD)).unwrap().server, server);
        fs::remove_file(marker(old_install)).unwrap();
        register_at(&registry, &root, &moved_server, &install).unwrap();
        let local = read_record(&root.join(RECORD)).unwrap();
        assert_eq!(local.server, moved_server);
        assert_eq!(local, read_record(&record_path(&registry, &root)).unwrap());
    }

    #[test]
    fn relocation_cannot_cross_a_pending_transaction() {
        let (_temp, root, server, registry) = fixture();
        fs::remove_file(marker(server.parent().unwrap())).unwrap();
        fs::create_dir_all(root.join("manager-update")).unwrap();
        fs::write(root.join("manager-update/transaction.json"), b"{}").unwrap();
        let install = server.parent().unwrap().with_file_name("moved");
        fs::create_dir(&install).unwrap();
        assert_eq!(
            register_at(&registry, &root, &install.join("server"), &install).unwrap_err(),
            "update-recovery-required"
        );
        assert_eq!(read_record(&root.join(RECORD)).unwrap().server, server);
    }

    #[test]
    fn owned_crash_temporary_files_do_not_block_data_removal() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".risunest-tmp-synthetic"), b"partial").unwrap();
        safe_data_tree(temp.path()).unwrap();
        fs::write(temp.path().join(".tmp-unowned"), b"user").unwrap();
        assert_eq!(
            safe_data_tree(temp.path()).unwrap_err(),
            "removal-unowned-data-files"
        );
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
    fn force_keeps_unregistered_data_and_unowned_files_without_mutating_a_preview() {
        let (_temp, root, server, registry) = fixture();
        fs::remove_file(record_path(&registry, &root)).unwrap();
        fs::remove_file(root.join(RECORD)).unwrap();
        fs::write(root.join("keep-data"), "synthetic").unwrap();
        fs::write(server.with_file_name("keep-file"), "synthetic").unwrap();
        assert!(plan_at(&registry, &root, &server, true).is_err());
        let plan = plan_at_with_force(&registry, &root, &server, true, true).unwrap();
        assert!(plan.blockers.is_empty());
        assert!(!plan.delete_data && !plan.remove_gui_profile);
        assert!(plan.data_kept_without_registration);
        assert!(!plan.files.contains(&server.with_file_name("keep-file")));
        assert_eq!(fs::read(root.join("keep-data")).unwrap(), b"synthetic");
        assert!(!root.join("manager-update").exists());
        assert!(!root.join(RECORD).exists());
    }

    #[test]
    fn force_preserves_ownership_and_recovery_boundaries() {
        let (_temp, root, server, registry) = fixture();
        let other = root.with_file_name("other");
        fs::create_dir(&other).unwrap();
        register_at(&registry, &other, &server, server.parent().unwrap()).unwrap();
        fs::remove_file(record_path(&registry, &root)).unwrap();
        fs::remove_file(root.join(RECORD)).unwrap();
        fs::create_dir(root.join("manager-update")).unwrap();
        fs::write(root.join("manager-update/transaction.json"), "{}").unwrap();
        let plan = plan_at_with_force(&registry, &root, &server, true, true).unwrap();
        assert!(plan
            .blockers
            .contains(&"removal-shared-installation-or-data".into()));
        assert!(plan.blockers.contains(&"update-recovery-required".into()));
        fs::write(root.join(RECORD), "{}").unwrap();
        assert!(plan_at_with_force(&registry, &root, &server, false, true).is_err());
    }

    #[test]
    fn obsolete_cleanup_reads_released_file_lists_without_format_version_matching() {
        let (_temp, _root, server, _registry) = fixture();
        let install = server.parent().unwrap();
        let marker_path = marker(install);
        let current = fs::read(&marker_path).unwrap();
        let mut intermediate: serde_json::Value = serde_json::from_slice(&current).unwrap();
        preserve_install_baseline(&server).unwrap();
        intermediate["files"] = serde_json::json!(["server", "middle.exe"]);
        fs::write(&marker_path, intermediate.to_string()).unwrap();
        fs::write(install.join("middle.exe"), "synthetic").unwrap();
        preserve_install_baseline(&server).unwrap();
        intermediate["files"] = serde_json::json!(["server", "later.exe"]);
        fs::write(&marker_path, intermediate.to_string()).unwrap();
        fs::write(install.join("later.exe"), "synthetic").unwrap();
        preserve_install_baseline(&server).unwrap();
        let mut owned = vec![PathBuf::from("server")];
        include_install_baseline(install, &mut owned).unwrap();
        assert!(
            owned.contains(&PathBuf::from("middle.exe"))
                && owned.contains(&PathBuf::from("later.exe"))
        );
        let baseline_path = install.join(INSTALL_BASELINE);
        let accumulated = fs::read(&baseline_path).unwrap();
        #[cfg(windows)]
        for locked in [&marker_path, &baseline_path] {
            use std::os::windows::fs::OpenOptionsExt;
            let handle = fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(locked)
                .unwrap();
            if locked == &marker_path {
                assert!(!preserve_install_baseline(&server).unwrap());
            } else {
                assert!(preserve_install_baseline(&server).is_err());
            }
            drop(handle);
            assert_eq!(fs::read(&baseline_path).unwrap(), accumulated);
        }
        fs::write(&marker_path, b"invalid current inventory").unwrap();
        assert!(!preserve_install_baseline(&server).unwrap());
        assert_eq!(fs::read(&baseline_path).unwrap(), accumulated);
        for files in [
            serde_json::json!([null]),
            serde_json::json!(["server", "server"]),
            serde_json::json!(["../outside"]),
        ] {
            let mut invalid: serde_json::Value = serde_json::from_slice(&current).unwrap();
            invalid["files"] = files;
            fs::write(&marker_path, invalid.to_string()).unwrap();
            assert!(!preserve_install_baseline(&server).unwrap());
            assert_eq!(fs::read(&baseline_path).unwrap(), accumulated);
        }
        fs::write(&marker_path, &current).unwrap();
        fs::write(&baseline_path, b"invalid accumulated inventory").unwrap();
        let mut owned = vec![PathBuf::from("server")];
        include_install_baseline(install, &mut owned).unwrap();
        assert_eq!(
            owned,
            [PathBuf::from("server"), PathBuf::from(INSTALL_BASELINE)]
        );
        preserve_install_baseline(&server).unwrap();
        assert_eq!(
            update::previous_owned_files(&baseline_path)
                .unwrap()
                .unwrap(),
            [PathBuf::from("server")]
        );
        assert!(install.join("middle.exe").exists() && install.join("later.exe").exists());
        fs::write(&baseline_path, accumulated).unwrap();
        fs::write(&marker_path, current).unwrap();
        assert!(remove_obsolete_owned_files(&server, &install.join(INSTALL_BASELINE)).unwrap());
        assert!(!install.join("middle.exe").exists() && !install.join("later.exe").exists());
        assert!(server.exists());

        let (temp, root, server, registry) = fixture();
        let previous = temp.path().join("previous.json");
        let old_file = server.with_file_name("old.exe");
        let unowned = server.with_file_name("keep.txt");
        fs::write(&old_file, "synthetic old").unwrap();
        fs::write(&unowned, "synthetic unowned").unwrap();
        fs::write(
            &previous,
            serde_json::json!({
                "schema":"risunest-sync-bundle/v1", "product":"sync", "variant":"managed",
                "version":"0.1.0", "protocolId":"old-protocol", "storeFormatId":"old-format",
                "files":["old.exe", "server"]
            })
            .to_string(),
        )
        .unwrap();
        assert!(remove_obsolete_owned_files(&server, &previous).unwrap());
        assert!(!old_file.exists());
        assert!(server.exists() && unowned.exists());
        assert!(remove_obsolete_owned_files(&server, &previous).unwrap());
        fs::remove_file(&unowned).unwrap();
        assert!(plan_at(&registry, &root, &server, false)
            .unwrap()
            .blockers
            .is_empty());
        fs::write(&previous, "invalid").unwrap();
        assert!(!remove_obsolete_owned_files(&server, &previous).unwrap());
        fs::write(&previous, serde_json::json!({"schema":"risunest-sync-bundle/v1", "product":"sync", "variant":"managed", "files":["server", "../outside"]}).to_string()).unwrap();
        assert!(remove_obsolete_owned_files(&server, &previous).is_err());
        assert!(server.exists());
    }

    #[test]
    fn server_initialization_preserves_the_early_registration() {
        let (_temp, root, _server, _registry) = fixture();
        let before = fs::read(root.join(RECORD)).unwrap();
        let store = risunest_sync_server::store::Store::init(&root).unwrap();
        assert!(root.join("metadata.sqlite").exists());
        assert_eq!(fs::read(root.join(RECORD)).unwrap(), before);
        drop(store);
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
        remove_data_files(&root, Duration::ZERO).unwrap();
        assert!(!root.exists());
        assert!(temp.path().join("export.zip").exists());
        remove_data_files(&root, Duration::ZERO).unwrap();
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
        assert!(remove_data_files(&root, Duration::ZERO).is_err());
        assert!(root.join(RECORD).exists());
        drop(handle);
        remove_data_files(&root, Duration::ZERO).unwrap();
        assert!(!root.exists());
    }
    #[cfg(windows)]
    #[test]
    fn data_removal_waits_for_an_exiting_guard_to_release_its_executable() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, root, _server, _registry) = fixture();
        let helper = root.join("manager-update/helper/risunest-sync-update-helper.exe");
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        fs::write(&helper, "synthetic").unwrap();
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&helper)
            .unwrap();
        let exit = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(handle);
        });
        remove_data_files(&root, Duration::from_secs(5)).unwrap();
        exit.join().unwrap();
        assert!(!root.exists());
    }
    #[cfg(windows)]
    fn windows_bundle(server: &Path, names: &[&str]) {
        let install = server.parent().unwrap();
        let path = marker(install);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["files"] = serde_json::json!(names);
        fs::write(path, value.to_string()).unwrap();
        for name in names {
            fs::write(install.join(name), "synthetic").unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn files_a_retried_uninstall_needs_are_removed_last() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, root, server, registry) = fixture();
        let install = server.parent().unwrap().to_owned();
        windows_bundle(
            &server,
            &[
                "server",
                "risunest-sync-manager.exe",
                "risunest-sync-manager-background.exe",
                "risunest-sync-server.exe",
            ],
        );
        for locked in [
            "risunest-sync-manager.exe",
            "risunest-sync-manager-background.exe",
            "risunest-sync-bundle.json",
        ] {
            let handle = fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(install.join(locked))
                .unwrap();
            let files = [
                "risunest-sync-manager.exe",
                "risunest-sync-manager-background.exe",
                "risunest-sync-bundle.json",
            ]
            .map(|name| install.join(name));
            assert!(remove_final_files(&files, Duration::ZERO).is_err());
            for file in files {
                assert!(file.is_file(), "{} was removed early", file.display());
            }
            drop(handle);
        }
        let final_files = [
            "risunest-sync-manager.exe",
            "risunest-sync-manager-background.exe",
            "risunest-sync-bundle.json",
        ]
        .map(|name| install.join(name));
        for readonly in &final_files {
            let original = fs::metadata(readonly).unwrap().permissions();
            let mut permissions = original.clone();
            permissions.set_readonly(true);
            fs::set_permissions(readonly, permissions).unwrap();
            assert!(remove_final_files(&final_files, Duration::ZERO).is_err());
            for file in &final_files {
                assert!(file.is_file(), "{} was removed early", file.display());
            }
            fs::set_permissions(readonly, original).unwrap();
        }
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(install.join("risunest-sync-server.exe"))
            .unwrap();
        let plan = plan_at(&registry, &root, &server, false).unwrap();
        assert!(plan.blockers.is_empty());
        assert!(remove_planned_files(&plan, Duration::ZERO).is_err());
        for name in [
            "risunest-sync-manager.exe",
            "risunest-sync-manager-background.exe",
            "risunest-sync-bundle.json",
        ] {
            assert!(install.join(name).is_file(), "{name} was removed early");
        }
        let scan = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(handle);
        });
        let plan = plan_at(&registry, &root, &server, false).unwrap();
        remove_planned_files(&plan, Duration::from_secs(5)).unwrap();
        scan.join().unwrap();
        assert_eq!(fs::read_dir(&install).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn final_disposition_retries_roll_back_before_retry_and_stop_at_the_budget() {
        for code in [5, 32, 87] {
            let mut marked = [false; 2];
            let mut fail = true;
            let result = set_final_dispositions(2, Duration::from_millis(150), |index, delete| {
                if index == 0 && delete {
                    assert!(!marked[0]);
                }
                if index == 1 && delete && fail {
                    fail = false;
                    return Err(std::io::Error::from_raw_os_error(code));
                }
                marked[index] = delete;
                Ok(())
            });
            assert_eq!(result.is_ok(), code != 87);
            assert_eq!(marked, if code == 87 { [false; 2] } else { [true; 2] });
        }
        let mut marked = false;
        let started = Instant::now();
        let result = set_final_dispositions(2, Duration::from_millis(75), |index, delete| {
            if index == 1 {
                return Err(std::io::Error::from_raw_os_error(32));
            }
            if delete {
                assert!(!marked);
            }
            marked = delete;
            Ok(())
        });
        assert_eq!(result.unwrap_err().1.raw_os_error(), Some(32));
        assert!(started.elapsed() >= Duration::from_millis(75));
        assert!(!marked);
    }

    #[cfg(windows)]
    #[test]
    fn resumed_delete_data_uses_registration_and_keeps_data_after_forgetting() {
        for (registered, deleted) in [false, true]
            .into_iter()
            .flat_map(|registered| [false, true].map(move |deleted| (registered, deleted)))
        {
            let (_temp, root, server, registry) = fixture();
            fs::write(root.join("metadata.sqlite"), b"synthetic").unwrap();
            let install = server.parent().unwrap();
            let helper = install.join("source-helper");
            fs::write(&helper, b"synthetic").unwrap();
            prepare_finalization(&registry, &root, &server, false, &helper).unwrap();
            if deleted {
                delete_registered_data_at(&registry, &root, &server, || Ok(Vec::new())).unwrap();
            }
            if !registered {
                for file in final_files(install) {
                    remove_file(&file).unwrap();
                }
                forget_finalization_at(&registry, &root, &server).unwrap();
            }
            let kept = resume_finalization_at(&registry, &server, true, || Ok(Vec::new())).unwrap();
            assert_eq!(kept, !registered && !deleted);
            assert_eq!(
                root.join("metadata.sqlite").exists(),
                !registered && !deleted
            );
            assert_eq!(root.exists(), !registered && !deleted);
            assert!(!record_path(&registry, &root).exists());
        }
    }
    #[cfg(windows)]
    #[test]
    fn an_unfinished_install_baseline_is_removed_with_the_installation() {
        let (_temp, root, server, registry) = fixture();
        let baseline = server.with_file_name(INSTALL_BASELINE);
        fs::write(&baseline, "synthetic").unwrap();
        let plan = plan_at(&registry, &root, &server, false).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        remove_planned_files(&plan, Duration::ZERO).unwrap();
        assert!(!baseline.exists());
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
