use crate::{app_paths::DesktopProfiles, launch_profile::ProfileName};

#[path = "guard.rs"]
mod guard;

#[path = "controller.rs"]
pub(crate) mod controller;
#[path = "adapters.rs"]
pub(crate) mod adapters;

const PROFILE_ENV: &str = "RISUNEST_OLLAMA_UI_PROFILE";

fn synthetic_profile(value: Option<&str>) -> Result<ProfileName, &'static str> {
    ProfileName::parse(guard::synthetic_profile(value)?).map_err(|_| "invalid native UI profile")
}

pub(super) fn validate_startup(
    profiles: &DesktopProfiles,
    profile: Option<&ProfileName>,
    embedded: bool,
) -> Result<(), &'static str> {
    guard::validate_startup(profiles.agent(), embedded, profile.map(ProfileName::as_str))
}

#[test]
#[ignore = "requires an embedded agent frontend and the owned synthetic UI runner"]
fn run_app() {
    let value = std::env::var(PROFILE_ENV).ok();
    let profile = synthetic_profile(value.as_deref()).expect("native UI profile refused");
    crate::run_with_arguments([std::ffi::OsString::from(profile.argument())], true);
}

#[test]
fn startup_requires_compiled_agent_and_embedded_frontend() {
    let profile = synthetic_profile(Some("agent-ollama-012345abcdef-0")).unwrap();
    let mut config = tauri::utils::config::Config::default();
    assert!(validate_startup(&DesktopProfiles::new(&config), Some(&profile), true).is_err());
    config.plugins.0.insert("risunest".into(), serde_json::json!({ "agent": false }));
    assert!(validate_startup(&DesktopProfiles::new(&config), Some(&profile), true).is_err());
    config.plugins.0.insert("risunest".into(), serde_json::json!({ "agent": true }));
    let profiles = DesktopProfiles::new(&config);
    assert!(validate_startup(&profiles, Some(&profile), false).is_err());
    assert_eq!(validate_startup(&profiles, Some(&profile), true), Ok(()));
    assert!(validate_startup(&profiles, None, true).is_err());
    assert!(validate_startup(&profiles, Some(&ProfileName::parse("personal").unwrap()), true).is_err());
}

#[test]
fn validated_profile_selects_only_the_product_launch_profile() {
    let profile = synthetic_profile(Some("agent-ollama-012345abcdef-12")).unwrap();
    let (selected, rest) = crate::launch_profile::split([std::ffi::OsString::from(profile.argument())]).unwrap();
    assert_eq!(selected, Some(profile));
    assert!(rest.is_empty());
}
