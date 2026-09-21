//! The single source of every filesystem root this application owns.
//!
//! No other module may call `dirs::*`, a `tauri::Manager::path()` accessor, or
//! read the environment for a root. Callers take the resolved manifest from
//! managed state. `tests/pathBoundary.node-test.mjs` enforces that boundary.

use std::path::{Path, PathBuf};

// Directory leaf names. A platform naming decision belongs here and nowhere
// else, so that it can never be expressed through the bundle identifier again.
const WINDOWS_DATA_LEAF: &str = "RisuNestData";
const WINDOWS_WEBVIEW_LEAF: &str = "RisuNestWebViewData";
const WINDOWS_CACHE_LEAF: &str = "RisuNestCache";
const WINDOWS_CLEANUP_LEAF: &str = "RisuNest-cleanup";
const WINDOWS_INSTALL_LEAF: &str = "RisuNest";
const MACOS_DATA_LEAF: &str = "RisuNest";
const MACOS_CACHE_LEAF: &str = "RisuNest";
const MACOS_CLEANUP_LEAF: &str = "RisuNest-cleanup";
const MACOS_INSTALL_DIR: &str = "/Applications/RisuNest.app";
const LINUX_DATA_LEAF: &str = "risunest";
const LINUX_WEBVIEW_LEAF: &str = "risunest-webview";
const LINUX_CACHE_LEAF: &str = "risunest";
const LINUX_CLEANUP_LEAF: &str = "risunest-cleanup";
const LINUX_INSTALL_DIRS: [&str; 2] = ["/usr/bin", "/usr/lib/risunest"];
const MOBILE_CLEANUP_LEAF: &str = "risunest-cleanup";
const LOGS_LEAF: &str = "logs";
/// An agent build carries the installed product name, so every owned root takes
/// a distinct leaf and an agent run cannot open real user data.
const AGENT_SUFFIX: &str = "-agent";
const CLEANUP_SUFFIX: &str = "-cleanup";

/// Every target rather than only the one being compiled, so one table can pin
/// the whole matrix from any host.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Platform {
    Windows,
    MacOs,
    Linux,
    Android,
    Ios,
}

impl Platform {
    pub(crate) fn current() -> Self {
        #[cfg(windows)]
        return Self::Windows;
        #[cfg(target_os = "macos")]
        return Self::MacOs;
        #[cfg(target_os = "linux")]
        return Self::Linux;
        #[cfg(target_os = "android")]
        return Self::Android;
        #[cfg(target_os = "ios")]
        return Self::Ios;
    }

    const fn is_desktop(self) -> bool {
        matches!(self, Self::Windows | Self::MacOs | Self::Linux)
    }
}

/// The OS-owned base directories a layout is built from. This is a flat mapping
/// of the platform accessors; no leaf name may appear here.
#[derive(Clone, Debug, Default)]
pub(crate) struct Hives {
    /// `%APPDATA%`, `~/Library/Application Support`, `$XDG_DATA_HOME`.
    pub roaming: Option<PathBuf>,
    /// `%LOCALAPPDATA%`, `~/Library/Application Support`, `$XDG_DATA_HOME`.
    pub local: Option<PathBuf>,
    /// `%APPDATA%`, `~/Library/Application Support`, `$XDG_CONFIG_HOME`.
    pub config: Option<PathBuf>,
    /// `%LOCALAPPDATA%`, `~/Library/Caches`, `$XDG_CACHE_HOME`.
    pub cache: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// The sandbox directories the OS hands a mobile application.
    pub os_data: Option<PathBuf>,
    pub os_cache: Option<PathBuf>,
}

impl Hives {
    fn from_platform() -> Self {
        Self {
            roaming: dirs::data_dir(),
            local: dirs::data_local_dir(),
            config: dirs::config_dir(),
            cache: dirs::cache_dir(),
            home: dirs::home_dir(),
            os_data: None,
            os_cache: None,
        }
    }

    fn require(hive: &Option<PathBuf>, name: &str) -> Result<PathBuf, String> {
        hive.clone()
            .filter(|path| path.is_absolute())
            .ok_or_else(|| format!("{name} directory unavailable"))
    }
}

/// XDG roots the Linux desktop-integration files live in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Integration {
    pub data_home: PathBuf,
    pub config_home: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    /// The authoritative native store.
    pub data: PathBuf,
    /// `None` where the platform manages the WebView profile itself.
    pub webview: Option<PathBuf>,
    pub logs: PathBuf,
    pub cache: PathBuf,
    pub cleanup_control: PathBuf,
    /// What Tauri would resolve from the identifier. Swept, never written.
    pub tauri_derived: Vec<PathBuf>,
    /// The directory an uninstall removes. `None` on mobile and for an
    /// AppImage, which is a single file rather than an installed directory.
    pub install: Option<PathBuf>,
    /// Linux only.
    pub integration: Option<Integration>,
}

impl AppPaths {
    /// Resolve the manifest from a configuration alone. Desktop startup and the
    /// cleanup CLI both run before there is an `AppHandle`.
    #[cfg(desktop)]
    pub(crate) fn desktop(config: &tauri::utils::config::Config) -> Result<Self, String> {
        let platform = Platform::current();
        let mut paths = Self::layout(
            platform,
            &Hives::from_platform(),
            &config.identifier,
            agent_build(config),
        )?;
        paths.install = install_directory(platform, appimage_build());
        Ok(paths)
    }

    /// Apply the platform checks that turn a computed store root into a usable
    /// one. Kept out of the constructors so the cleanup CLI never creates a
    /// directory it is about to delete.
    #[cfg(desktop)]
    pub(crate) fn prepared(mut self) -> Result<Self, String> {
        self.data = platform_store_root(&self.data, Platform::current())?;
        self.logs = self.data.join(LOGS_LEAF);
        Ok(self)
    }

    /// Resolve the manifest on mobile, where only the OS knows the sandbox.
    #[cfg(mobile)]
    pub(crate) fn resolve<R: tauri::Runtime>(
        app: &impl tauri::Manager<R>,
    ) -> Result<Self, String> {
        use tauri::Manager;
        let platform = Platform::current();
        let mut hives = Hives::default();
        hives.os_data = app.path().app_data_dir().ok();
        hives.os_cache = app.path().app_cache_dir().ok();
        let mut paths = Self::layout(platform, &hives, &app.config().identifier, false)?;
        paths.data = platform_store_root(&paths.data, platform)?;
        paths.logs = paths.data.join(LOGS_LEAF);
        paths.cache = platform_store_root(&paths.cache, platform)?;
        paths.cleanup_control = paths
            .data
            .parent()
            .ok_or("application data root has no parent")?
            .join(MOBILE_CLEANUP_LEAF);
        Ok(paths)
    }

    /// Every root inside one directory. An alternative native entry uses this
    /// so that its files never reach the installed product's directories.
    pub fn isolated(root: PathBuf) -> Result<Self, String> {
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("an isolated root needs a named directory")?;
        let cleanup_control = root.with_file_name(format!("{name}{CLEANUP_SUFFIX}"));
        Ok(Self {
            logs: root.join(LOGS_LEAF),
            cache: root.join("cache"),
            webview: Some(root.join("webview")),
            data: root,
            cleanup_control,
            tauri_derived: Vec::new(),
            install: None,
            integration: None,
        })
    }

    /// The whole layout, derived from nothing but the platform, its base
    /// directories, and the identifier. `tests.rs` pins every row of it.
    fn layout(
        platform: Platform,
        hives: &Hives,
        identifier: &str,
        agent: bool,
    ) -> Result<Self, String> {
        validate_identifier(identifier)?;
        let leaf = |name: &str| {
            if agent && platform.is_desktop() {
                format!("{name}{AGENT_SUFFIX}")
            } else {
                name.to_owned()
            }
        };
        let (data, webview, cache, cleanup_control, integration) = match platform {
            Platform::Windows => {
                let local = Hives::require(&hives.local, "local application data")?;
                (
                    local.join(leaf(WINDOWS_DATA_LEAF)),
                    Some(local.join(leaf(WINDOWS_WEBVIEW_LEAF))),
                    local.join(leaf(WINDOWS_CACHE_LEAF)),
                    local.join(leaf(WINDOWS_CLEANUP_LEAF)),
                    None,
                )
            }
            Platform::MacOs => {
                let support = Hives::require(&hives.roaming, "application support")?;
                let cache = Hives::require(&hives.cache, "cache")?;
                (
                    support.join(leaf(MACOS_DATA_LEAF)),
                    None,
                    cache.join(leaf(MACOS_CACHE_LEAF)),
                    support.join(leaf(MACOS_CLEANUP_LEAF)),
                    None,
                )
            }
            Platform::Linux => {
                let data_home = Hives::require(&hives.roaming, "user data")?;
                let config_home = Hives::require(&hives.config, "user configuration")?;
                let cache = Hives::require(&hives.cache, "cache")?;
                (
                    data_home.join(leaf(LINUX_DATA_LEAF)),
                    Some(data_home.join(leaf(LINUX_WEBVIEW_LEAF))),
                    cache.join(leaf(LINUX_CACHE_LEAF)),
                    data_home.join(leaf(LINUX_CLEANUP_LEAF)),
                    Some(Integration {
                        data_home,
                        config_home,
                    }),
                )
            }
            Platform::Android | Platform::Ios => {
                let data = Hives::require(&hives.os_data, "application data")?;
                let cache = Hives::require(&hives.os_cache, "application cache")?;
                let control = data
                    .parent()
                    .ok_or("application data root has no parent")?
                    .join(MOBILE_CLEANUP_LEAF);
                (data, None, cache, control, None)
            }
        };
        Ok(Self {
            logs: data.join(LOGS_LEAF),
            data,
            webview,
            cache,
            cleanup_control,
            // An agent build writes to none of them and must not delete the
            // installed product's, which are not leaf-suffixed.
            tauri_derived: if agent && platform.is_desktop() {
                Vec::new()
            } else {
                tauri_derived(platform, hives, identifier)
            },
            install: None,
            integration,
        })
    }

    /// The roots the cleanup operation deletes.
    pub(crate) fn owned_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.data.clone(), self.cache.clone()];
        roots.extend(self.webview.clone());
        roots.extend(self.tauri_derived.iter().cloned());
        roots.sort();
        roots.dedup();
        roots
    }

    /// The first owned root that equals, contains, or sits inside the
    /// installation directory. Deleting such a root can never succeed.
    pub(crate) fn install_conflict(&self) -> Option<PathBuf> {
        let install = self.install.as_deref()?;
        self.owned_roots()
            .into_iter()
            .chain(std::iter::once(self.cleanup_control.clone()))
            .find(|root| overlaps(root, install))
    }
}

/// The roots the renderer is allowed to use. It takes them from here instead of
/// deriving any of its own.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Roots {
    data: String,
}

#[tauri::command]
pub(crate) fn app_paths_roots(app: tauri::AppHandle) -> Result<Roots, String> {
    let paths = manifest(&app)?;
    Ok(Roots {
        data: paths
            .data
            .to_str()
            .ok_or("application data root is not valid UTF-8")?
            .to_owned(),
    })
}

/// Permit the renderer to reach the manifest roots. The static capability scope
/// cannot name them, because an agent build uses distinct leaves.
pub(crate) fn permit_renderer_access(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_fs::FsExt;
    let paths = manifest(app)?;
    app.fs_scope()
        .allow_directory(&paths.data, true)
        .map_err(|error| format!("data root file permission unavailable: {error}"))?;
    app.asset_protocol_scope()
        .allow_directory(&paths.data, true)
        .map_err(|error| format!("data root asset permission unavailable: {error}"))
}

/// The resolved manifest. It is managed before any plugin, command, or setup
/// step can run, so an absent one is a programming error rather than a state.
pub(crate) fn manifest<R: tauri::Runtime>(
    app: &impl tauri::Manager<R>,
) -> Result<tauri::State<'_, AppPaths>, String> {
    app.try_state::<AppPaths>()
        .ok_or_else(|| "application paths are unavailable".to_owned())
}

/// The authoritative native store, for the callers that need nothing else.
pub(crate) fn data_root<R: tauri::Runtime>(
    app: &impl tauri::Manager<R>,
) -> Result<PathBuf, String> {
    manifest(app).map(|paths| paths.data.clone())
}

/// Component-wise containment. A string prefix would report `RisuNestData` as
/// sitting inside `RisuNest`.
fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn tauri_derived(platform: Platform, hives: &Hives, identifier: &str) -> Vec<PathBuf> {
    let mut derived = Vec::new();
    match platform {
        Platform::Windows | Platform::MacOs | Platform::Linux => {
            for hive in [&hives.roaming, &hives.local, &hives.config, &hives.cache] {
                if let Some(hive) = hive.as_deref().filter(|path| path.is_absolute()) {
                    derived.push(hive.join(identifier));
                }
            }
            if platform == Platform::MacOs {
                if let Some(home) = hives.home.as_deref().filter(|path| path.is_absolute()) {
                    derived.push(home.join("Library/Logs").join(identifier));
                }
            }
        }
        // The OS sandbox root is the store itself and the OS removes it on
        // uninstall, so there is nothing separate to sweep.
        Platform::Android | Platform::Ios => {}
    }
    derived.sort();
    derived.dedup();
    derived
}

/// The installation directory each platform's installer selects. Used by the
/// matrix test; `install` itself comes from the running executable.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn default_install_dirs(platform: Platform, hives: &Hives) -> Vec<PathBuf> {
    match platform {
        Platform::Windows => hives
            .local
            .iter()
            .map(|local| local.join(WINDOWS_INSTALL_LEAF))
            .collect(),
        Platform::MacOs => vec![PathBuf::from(MACOS_INSTALL_DIR)],
        Platform::Linux => LINUX_INSTALL_DIRS.iter().map(PathBuf::from).collect(),
        Platform::Android | Platform::Ios => Vec::new(),
    }
}

#[cfg(desktop)]
fn install_directory(platform: Platform, appimage: bool) -> Option<PathBuf> {
    if !platform.is_desktop() || appimage {
        return None;
    }
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    if platform == Platform::MacOs {
        // A macOS uninstall removes the bundle, not the directory holding the
        // executable inside it.
        if let Some(bundle) = directory
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        {
            return Some(bundle.to_owned());
        }
    }
    Some(directory.to_owned())
}

#[cfg(desktop)]
fn appimage_build() -> bool {
    #[cfg(target_os = "linux")]
    return std::env::var_os("APPIMAGE").is_some();
    #[cfg(not(target_os = "linux"))]
    return false;
}

#[cfg(desktop)]
fn agent_build(config: &tauri::utils::config::Config) -> bool {
    config
        .plugins
        .0
        .get("risunest")
        .and_then(|value| value.get("agent"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn validate_identifier(identifier: &str) -> Result<(), String> {
    let valid = !identifier.is_empty()
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    valid
        .then_some(())
        .ok_or_else(|| "application identifier is invalid".to_owned())
}

/// Android mount aliases and Linux data-home aliases are trusted only above the
/// OS-provided root. CAS still rejects every app-owned link.
fn platform_store_root(root: &Path, platform: Platform) -> Result<PathBuf, String> {
    if !matches!(platform, Platform::Android | Platform::Linux) {
        return Ok(root.to_owned());
    }
    resolve_platform_root(root, platform == Platform::Linux)
        .map_err(|error| format!("application data root unavailable: {error}"))
}

#[cfg(desktop)]
pub(crate) fn take_main_window_with_webview_root(
    context: &mut tauri::Context<tauri::Wry>,
    paths: &AppPaths,
) -> std::io::Result<Option<(tauri::utils::config::WindowConfig, PathBuf)>> {
    let Some(root) = paths.webview.clone() else {
        return Ok(None);
    };
    let index = context
        .config_mut()
        .app
        .windows
        .iter()
        .position(|window| window.label == "main")
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "main window missing")
        })?;
    let window = context.config_mut().app.windows.remove(index);
    Ok(Some((window, root)))
}

fn resolve_platform_root(root: &Path, create_missing: bool) -> std::io::Result<PathBuf> {
    use std::{fs, io};

    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Application data root must be absolute",
        ));
    }
    let metadata = match fs::symlink_metadata(root) {
        Err(error) if create_missing && error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(root)?;
            fs::symlink_metadata(root)?
        }
        result => result?,
    };
    if crate::trust_boundary::is_link_like(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Application data root must be a real directory",
        ));
    }

    fs::canonicalize(root)
}

#[cfg(test)]
mod tests;
