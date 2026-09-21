//! The single source of every filesystem root RisuNest Sync owns.
//!
//! Nothing else in the crate may read the environment for a root. The
//! identifier names the bundle and nothing on disk, so a store root can never
//! coincide with the directory an installer writes into.

use crate::Result;
use std::path::PathBuf;

// Directory leaf names. A platform naming decision belongs here and nowhere
// else.
const WINDOWS_DATA_LEAF: &str = "RisuNestSyncData";
const WINDOWS_WEBVIEW_LEAF: &str = "RisuNestSyncWebViewData";
const WINDOWS_INSTALL_LEAF: &str = "RisuNestSync";
const MACOS_DATA_LEAF: &str = "RisuNest Sync";
const MACOS_INSTALL_DIR: &str = "/Applications/RisuNest Sync.app";
const LINUX_DATA_LEAF: &str = "risunest-sync";
const LINUX_WEBVIEW_LEAF: &str = "risunest-sync-webview";
const LINUX_INSTALL_LEAF: &str = ".local/lib/risunest-sync";
/// The bundle identity. It names no directory the product writes to.
pub const IDENTIFIER: &str = "io.github.rsyumi.risunest.sync-manager";

/// Every target rather than only the one being compiled, so one table can pin
/// the whole matrix from any host.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        #[cfg(windows)]
        return Self::Windows;
        #[cfg(target_os = "macos")]
        return Self::MacOs;
        #[cfg(all(unix, not(target_os = "macos")))]
        return Self::Linux;
    }
}

/// The OS-owned base directories a layout is built from. This is a flat
/// mapping of the platform environment; no leaf name may appear here.
#[derive(Clone, Debug, Default)]
pub struct Hives {
    /// `%APPDATA%`.
    pub roaming: Option<PathBuf>,
    /// `%LOCALAPPDATA%`.
    pub local: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub xdg_data: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    pub xdg_cache: Option<PathBuf>,
}

impl Hives {
    pub fn from_environment() -> Self {
        let read = |name: &str| std::env::var_os(name).map(PathBuf::from);
        Self {
            roaming: read("APPDATA"),
            local: read("LOCALAPPDATA"),
            home: read("HOME"),
            xdg_data: read("XDG_DATA_HOME"),
            xdg_config: read("XDG_CONFIG_HOME"),
            xdg_cache: read("XDG_CACHE_HOME"),
        }
    }

    fn absolute(hive: &Option<PathBuf>) -> Option<PathBuf> {
        hive.clone().filter(|path| path.is_absolute())
    }

    fn xdg(&self, explicit: &Option<PathBuf>, fallback: &str) -> Option<PathBuf> {
        Self::absolute(explicit)
            .or_else(|| Self::absolute(&self.home).map(|home| home.join(fallback)))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncPaths {
    /// The authoritative server and manager store.
    pub data: PathBuf,
    /// `None` where the platform manages the WebView profile itself.
    pub webview: Option<PathBuf>,
    /// What Tauri would resolve from the identifier. Swept, never written.
    pub tauri_derived: Vec<PathBuf>,
    /// The directory the installer selects by default.
    pub default_install: Option<PathBuf>,
    /// The directory holding the user service unit for this platform. `None`
    /// where startup registration is not a unit file.
    pub user_services: Option<PathBuf>,
}

pub fn layout(platform: Platform, hives: &Hives) -> Result<SyncPaths> {
    let missing = || "user-data-directory-unavailable";
    let (data, webview, default_install) = match platform {
        Platform::Windows => {
            let local = Hives::absolute(&hives.local).ok_or_else(missing)?;
            (
                local.join(WINDOWS_DATA_LEAF),
                Some(local.join(WINDOWS_WEBVIEW_LEAF)),
                Some(local.join(WINDOWS_INSTALL_LEAF)),
            )
        }
        Platform::MacOs => {
            let support = Hives::absolute(&hives.home)
                .ok_or_else(missing)?
                .join("Library/Application Support");
            (
                support.join(MACOS_DATA_LEAF),
                None,
                Some(PathBuf::from(MACOS_INSTALL_DIR)),
            )
        }
        Platform::Linux => {
            let data_home = hives
                .xdg(&hives.xdg_data, ".local/share")
                .ok_or_else(missing)?;
            (
                data_home.join(LINUX_DATA_LEAF),
                Some(data_home.join(LINUX_WEBVIEW_LEAF)),
                Hives::absolute(&hives.home).map(|home| home.join(LINUX_INSTALL_LEAF)),
            )
        }
    };
    Ok(SyncPaths {
        data,
        webview,
        tauri_derived: tauri_derived(platform, hives),
        default_install,
        user_services: match platform {
            Platform::Linux => Some(
                hives
                    .xdg(&hives.xdg_config, ".config")
                    .ok_or_else(missing)?
                    .join("systemd/user"),
            ),
            Platform::MacOs => Some(
                Hives::absolute(&hives.home)
                    .ok_or_else(missing)?
                    .join("Library/LaunchAgents"),
            ),
            Platform::Windows => None,
        },
    })
}

fn tauri_derived(platform: Platform, hives: &Hives) -> Vec<PathBuf> {
    let mut derived = match platform {
        Platform::Windows => [&hives.roaming, &hives.local]
            .into_iter()
            .filter_map(Hives::absolute)
            .map(|hive| hive.join(IDENTIFIER))
            .collect::<Vec<_>>(),
        Platform::MacOs => Hives::absolute(&hives.home)
            .map(|home| {
                ["Library/Application Support", "Library/Caches", "Library/Logs"]
                    .into_iter()
                    .map(|hive| home.join(hive).join(IDENTIFIER))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        Platform::Linux => [
            hives.xdg(&hives.xdg_data, ".local/share"),
            hives.xdg(&hives.xdg_config, ".config"),
            hives.xdg(&hives.xdg_cache, ".cache"),
        ]
        .into_iter()
        .flatten()
        .map(|hive| hive.join(IDENTIFIER))
        .collect(),
    };
    derived.sort();
    derived.dedup();
    derived
}

pub fn current() -> Result<SyncPaths> {
    layout(Platform::current(), &Hives::from_environment())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:\\synthetic\\{name}"))
        } else {
            PathBuf::from(format!("/synthetic/{name}"))
        }
    }

    fn hives() -> Hives {
        Hives {
            roaming: Some(base("roaming")),
            local: Some(base("local")),
            home: Some(base("home")),
            xdg_data: None,
            xdg_config: None,
            xdg_cache: None,
        }
    }

    /// Component-wise containment. A string prefix would report
    /// `RisuNestSyncData` as sitting inside `RisuNestSync`.
    fn overlaps(left: &std::path::Path, right: &std::path::Path) -> bool {
        left.starts_with(right) || right.starts_with(left)
    }

    #[test]
    fn every_platform_row_matches_the_recorded_matrix() {
        let windows = layout(Platform::Windows, &hives()).unwrap();
        assert_eq!(windows.data, base("local").join("RisuNestSyncData"));
        assert_eq!(
            windows.webview,
            Some(base("local").join("RisuNestSyncWebViewData"))
        );
        assert_eq!(
            windows.default_install,
            Some(base("local").join("RisuNestSync"))
        );

        let macos = layout(Platform::MacOs, &hives()).unwrap();
        assert_eq!(
            macos.data,
            base("home").join("Library/Application Support/RisuNest Sync")
        );
        assert_eq!(macos.webview, None);

        let linux = layout(Platform::Linux, &hives()).unwrap();
        assert_eq!(
            linux.data,
            base("home").join(".local/share/risunest-sync")
        );
        assert_eq!(
            linux.webview,
            Some(base("home").join(".local/share/risunest-sync-webview"))
        );
        assert_eq!(
            linux.default_install,
            Some(base("home").join(".local/lib/risunest-sync"))
        );

        assert_eq!(
            linux.user_services,
            Some(base("home").join(".config/systemd/user"))
        );
        assert_eq!(
            macos.user_services,
            Some(base("home").join("Library/LaunchAgents"))
        );
        assert_eq!(windows.user_services, None);

        let explicit = Hives {
            xdg_data: Some(base("xdg-data")),
            xdg_config: Some(base("xdg-config")),
            ..hives()
        };
        let resolved = layout(Platform::Linux, &explicit).unwrap();
        assert_eq!(resolved.data, base("xdg-data").join("risunest-sync"));
        assert_eq!(
            resolved.user_services,
            Some(base("xdg-config").join("systemd/user"))
        );

        // An empty or relative value is not a hive; the absolute home takes over.
        for invalid in [PathBuf::new(), PathBuf::from("relative/config")] {
            let relative = Hives {
                xdg_config: Some(invalid),
                ..hives()
            };
            assert_eq!(
                layout(Platform::Linux, &relative).unwrap().user_services,
                Some(base("home").join(".config/systemd/user"))
            );
        }
        assert!(layout(
            Platform::Linux,
            &Hives {
                home: Some(PathBuf::from("relative/home")),
                ..Hives::default()
            }
        )
        .is_err());
    }

    #[test]
    fn no_root_overlaps_the_installation_directory_or_the_identifier() {
        for platform in [Platform::Windows, Platform::MacOs, Platform::Linux] {
            let paths = layout(platform, &hives()).unwrap();
            let install = paths
                .default_install
                .as_deref()
                .expect("recorded installation directory");
            for root in [Some(&paths.data), paths.webview.as_ref()]
                .into_iter()
                .flatten()
                .chain(paths.tauri_derived.iter())
            {
                assert!(
                    !overlaps(root, install),
                    "{platform:?}: {} overlaps {}",
                    root.display(),
                    install.display()
                );
            }
            assert!(!paths.tauri_derived.is_empty());
            for derived in &paths.tauri_derived {
                assert!(derived.ends_with(IDENTIFIER));
                assert!(!overlaps(derived, &paths.data));
            }
        }
    }

    #[test]
    fn a_relative_or_missing_hive_is_an_error_not_a_guess() {
        let relative = Hives {
            local: Some(PathBuf::from("relative")),
            ..hives()
        };
        assert!(layout(Platform::Windows, &relative).is_err());
        assert!(layout(Platform::Windows, &Hives::default()).is_err());
        assert!(layout(Platform::MacOs, &Hives::default()).is_err());
        assert!(layout(Platform::Linux, &Hives::default()).is_err());
    }
}
