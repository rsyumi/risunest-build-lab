use super::{
    default_install_dirs, overlaps, resolve_platform_root, AppPaths, Hives, Integration, Platform,
};
use crate::asset_repository::{ExactObjectUnlink, PayloadCas};
use std::{fs, io::ErrorKind, path::Path, path::PathBuf};

const IDENTIFIER: &str = "io.github.rsyumi.risunest";

/// An absolute synthetic base on any host, so one table pins every platform row.
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
        config: Some(base("config")),
        cache: Some(base("cache")),
        home: Some(base("home")),
        os_data: Some(base("sandbox/files")),
        os_cache: Some(base("sandbox/cache")),
    }
}

fn layout(platform: Platform) -> AppPaths {
    AppPaths::layout(platform, &hives(), IDENTIFIER, false).expect("resolve synthetic layout")
}

#[test]
fn every_platform_row_matches_the_recorded_matrix() {
    let windows = layout(Platform::Windows);
    assert_eq!(windows.data, base("local").join("RisuNestData"));
    assert_eq!(
        windows.webview,
        Some(base("local").join("RisuNestWebViewData"))
    );
    assert_eq!(windows.cache, base("local").join("RisuNestCache"));
    assert_eq!(
        windows.cleanup_control,
        base("local").join("RisuNest-cleanup")
    );
    assert_eq!(windows.integration, None);

    let macos = layout(Platform::MacOs);
    assert_eq!(macos.data, base("roaming").join("RisuNest"));
    assert_eq!(macos.webview, None);
    assert_eq!(macos.cache, base("cache").join("RisuNest"));
    assert_eq!(
        macos.cleanup_control,
        base("roaming").join("RisuNest-cleanup")
    );
    assert!(macos
        .tauri_derived
        .contains(&base("home").join("Library/Logs").join(IDENTIFIER)));

    let linux = layout(Platform::Linux);
    assert_eq!(linux.data, base("roaming").join("risunest"));
    assert_eq!(linux.webview, Some(base("roaming").join("risunest-webview")));
    assert_eq!(linux.cache, base("cache").join("risunest"));
    assert_eq!(
        linux.cleanup_control,
        base("roaming").join("risunest-cleanup")
    );
    assert_eq!(
        linux.integration,
        Some(Integration {
            data_home: base("roaming"),
            config_home: base("config"),
        })
    );

    for platform in [Platform::Android, Platform::Ios] {
        let mobile = layout(platform);
        assert_eq!(mobile.data, base("sandbox/files"));
        assert_eq!(mobile.webview, None);
        assert_eq!(mobile.cache, base("sandbox/cache"));
        assert_eq!(
            mobile.cleanup_control,
            base("sandbox").join("risunest-cleanup")
        );
        // The OS removes the sandbox on uninstall, so nothing is swept.
        assert!(mobile.tauri_derived.is_empty());
    }

    for platform in [
        Platform::Windows,
        Platform::MacOs,
        Platform::Linux,
        Platform::Android,
        Platform::Ios,
    ] {
        let paths = layout(platform);
        assert_eq!(paths.logs, paths.data.join("logs"));
        assert_eq!(paths.install, None);
    }
}

#[test]
fn identifier_derived_directories_are_swept_but_are_never_the_store() {
    for platform in [Platform::Windows, Platform::MacOs, Platform::Linux] {
        let paths = layout(platform);
        assert!(!paths.tauri_derived.is_empty());
        for derived in &paths.tauri_derived {
            assert!(derived.ends_with(IDENTIFIER) || derived.ends_with(Path::new(IDENTIFIER)));
            assert_ne!(*derived, paths.data);
            assert!(!overlaps(derived, &paths.data));
        }
        assert!(paths.owned_roots().contains(&paths.data));
        assert!(paths.owned_roots().contains(&paths.cache));
    }
}

#[test]
fn no_owned_root_overlaps_the_default_installation_directory() {
    for platform in [Platform::Windows, Platform::MacOs, Platform::Linux] {
        let mut paths = layout(platform);
        let defaults = default_install_dirs(platform, &hives());
        assert!(
            !defaults.is_empty(),
            "{platform:?} has no recorded default installation directory"
        );
        for install in defaults {
            paths.install = Some(install.clone());
            assert_eq!(
                paths.install_conflict(),
                None,
                "{platform:?} root overlaps {}",
                install.display()
            );
        }
    }
}

#[test]
fn the_windows_store_is_a_sibling_of_the_installation_directory_not_a_child() {
    let paths = layout(Platform::Windows);
    let install = default_install_dirs(Platform::Windows, &hives())
        .pop()
        .expect("windows installation directory");
    assert_eq!(install, base("local").join("RisuNest"));
    // A string-prefix comparison would report this as nested.
    assert!(paths
        .data
        .to_string_lossy()
        .starts_with(&*install.to_string_lossy()));
    assert!(!overlaps(&paths.data, &install));
}

#[test]
fn a_root_that_holds_the_installation_directory_is_refused() {
    let mut paths = layout(Platform::Windows);
    paths.install = Some(paths.data.clone());
    assert_eq!(paths.install_conflict(), Some(paths.data.clone()));

    paths.install = Some(paths.data.join("nested"));
    assert_eq!(paths.install_conflict(), Some(paths.data.clone()));

    paths.install = Some(base("local"));
    assert!(paths.install_conflict().is_some());

    // The control directory is checked as well; it is deleted last but still
    // sits under a hive the installer could have been pointed at.
    let mut paths = layout(Platform::Windows);
    paths.install = Some(paths.cleanup_control.clone());
    assert_eq!(
        paths.install_conflict(),
        Some(paths.cleanup_control.clone())
    );
}

#[test]
fn an_agent_build_never_shares_a_desktop_root_with_the_installed_product() {
    for platform in [Platform::Windows, Platform::MacOs, Platform::Linux] {
        let installed = layout(platform);
        let agent = AppPaths::layout(platform, &hives(), IDENTIFIER, true).unwrap();
        assert_ne!(agent.data, installed.data);
        assert_ne!(agent.cleanup_control, installed.cleanup_control);
        assert_ne!(agent.cache, installed.cache);
        if installed.webview.is_some() {
            assert_ne!(agent.webview, installed.webview);
        }
        // The identifier-derived sweep roots carry no agent leaf, so an agent
        // build claims none of them rather than deleting the product's.
        assert!(agent.tauri_derived.is_empty());
        assert!(!installed.tauri_derived.is_empty());
        for root in agent.owned_roots() {
            assert!(
                !installed.owned_roots().contains(&root),
                "{} is shared with the installed product",
                root.display()
            );
        }
    }
}

#[test]
fn an_agent_build_keeps_the_mobile_sandbox_unchanged() {
    for platform in [Platform::Android, Platform::Ios] {
        assert_eq!(
            AppPaths::layout(platform, &hives(), IDENTIFIER, true).unwrap(),
            layout(platform)
        );
    }
}

#[test]
fn a_missing_hive_or_invalid_identifier_is_an_error_not_a_guess() {
    let mut missing = hives();
    missing.local = None;
    assert!(AppPaths::layout(Platform::Windows, &missing, IDENTIFIER, false).is_err());

    let mut relative = hives();
    relative.roaming = Some(PathBuf::from("relative"));
    assert!(AppPaths::layout(Platform::Linux, &relative, IDENTIFIER, false).is_err());

    for invalid in ["", "has space", "has/slash", "has\\slash"] {
        assert!(AppPaths::layout(Platform::Windows, &hives(), invalid, false).is_err());
    }
}

#[cfg(unix)]
fn link_directory(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create synthetic directory link");
}

#[cfg(windows)]
fn link_directory(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("create synthetic directory junction");
    assert!(output.status.success(), "create junction: {output:?}");
}

#[test]
fn platform_parent_alias_supports_cas_write_read_and_delete() {
    let fixture = tempfile::tempdir().unwrap();
    let real_parent = fixture.path().join("data");
    let real_root = real_parent.join("synthetic.app");
    fs::create_dir_all(&real_root).unwrap();
    let alias_parent = fixture.path().join("user-0");
    link_directory(&real_parent, &alias_parent);
    let platform_root = alias_parent.join("synthetic.app");

    // Untrusted CAS inputs must still reject links in their ancestry.
    let error = PayloadCas::new(&platform_root).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .starts_with("linked path component is forbidden:"));

    let root = resolve_platform_root(&platform_root, false).unwrap();
    assert_eq!(root, resolve_platform_root(&platform_root, true).unwrap());
    assert_eq!(root, fs::canonicalize(&real_root).unwrap());
    let cas = PayloadCas::new(&root).expect("open CAS through resolved platform root");
    let payload = cas.prepare_bytes(b"synthetic Android asset").unwrap();
    assert_eq!(
        cas.read_object(&payload.content_hash).unwrap().unwrap(),
        b"synthetic Android asset"
    );
    assert!(matches!(
        cas.unlink_exact_object(
            &payload.content_hash,
            payload.byte_size,
            &payload.physical_key
        )
        .unwrap(),
        ExactObjectUnlink::Removed { .. }
    ));
    assert!(cas.read_object(&payload.content_hash).unwrap().is_none());
}

#[test]
fn platform_root_does_not_assume_primary_user_or_package_name() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("user/10/another.synthetic.app");
    fs::create_dir_all(&root).unwrap();
    assert_eq!(
        resolve_platform_root(&root, false).unwrap(),
        fs::canonicalize(root).unwrap()
    );
}

#[test]
fn platform_root_itself_cannot_be_a_link() {
    let fixture = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = fixture.path().join("linked-app");
    link_directory(outside.path(), &root);
    assert_eq!(
        resolve_platform_root(&root, false).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn platform_root_must_be_an_existing_absolute_directory() {
    let fixture = tempfile::tempdir().unwrap();
    let file = fixture.path().join("file");
    fs::write(&file, b"synthetic").unwrap();
    assert_eq!(
        resolve_platform_root(&file, false).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        resolve_platform_root(&fixture.path().join("missing"), false)
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        resolve_platform_root(Path::new("relative-app"), false)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn first_run_creates_a_real_root_beneath_a_trusted_parent_alias() {
    let fixture = tempfile::tempdir().unwrap();
    let parent = fixture.path().join("real-parent");
    fs::create_dir(&parent).unwrap();
    let alias = fixture.path().join("home-alias");
    link_directory(&parent, &alias);
    let root = resolve_platform_root(&alias.join("RisuNest"), true).unwrap();
    assert_eq!(root, parent.join("RisuNest").canonicalize().unwrap());
    let cas = PayloadCas::new(&root).unwrap();
    let payload = cas.prepare_bytes(b"first-run asset").unwrap();
    assert_eq!(
        cas.read_object(&payload.content_hash).unwrap().unwrap(),
        b"first-run asset"
    );
}

#[test]
fn first_run_does_not_accept_a_linked_or_nonabsolute_root() {
    let fixture = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = fixture.path().join("linked-app");
    link_directory(outside.path(), &root);
    assert_eq!(
        resolve_platform_root(&root, true).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        resolve_platform_root(Path::new("relative-app"), true)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn resolved_root_does_not_allow_asset_directory_redirection() {
    let fixture = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = resolve_platform_root(fixture.path(), false).unwrap();
    let cas = PayloadCas::new(&root).unwrap();
    let sentinel = outside.path().join("sentinel");
    fs::write(&sentinel, b"untouched").unwrap();
    link_directory(outside.path(), &root.join("assets"));
    assert_eq!(
        cas.prepare_bytes(b"new asset").unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        cas.read_object(&"0".repeat(64)).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(fs::read(sentinel).unwrap(), b"untouched");
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
}

#[test]
fn resolved_root_replaced_by_link_is_rejected_by_cas() {
    let fixture = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let platform_root = fixture.path().join("app");
    fs::create_dir(&platform_root).unwrap();
    let root = resolve_platform_root(&platform_root, false).unwrap();
    let cas = PayloadCas::new(&root).unwrap();
    fs::remove_dir(&platform_root).unwrap();
    link_directory(outside.path(), &platform_root);
    assert_eq!(
        cas.prepare_bytes(b"new asset").unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert!(PayloadCas::new(&root).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}
