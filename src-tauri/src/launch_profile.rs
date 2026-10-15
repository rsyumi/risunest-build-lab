//! The launch profile selected with `--profile=<name>` on the desktop command
//! line. Without the option the default profile keeps every existing root and
//! identity; a named profile is a separate, complete store.

use std::ffi::OsString;

const OPTION: &str = "--profile";
const MAX_NAME_LENGTH: usize = 32;
/// `default` names the unnamed profile, and the Windows device names cannot be
/// directory names there. Both are refused everywhere so a name means the same
/// thing on every desktop platform.
const RESERVED: [&str; 5] = ["default", "con", "prn", "aux", "nul"];

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ProfileName(String);

impl ProfileName {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let valid = (1..=MAX_NAME_LENGTH).contains(&value.len())
            && value.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
            && value.as_bytes()[0].is_ascii_alphanumeric();
        if !valid {
            return Err(format!(
                "invalid profile name \"{}\": use 1 to {MAX_NAME_LENGTH} lowercase letters, digits, hyphens or underscores, starting with a letter or digit",
                value.chars().take(MAX_NAME_LENGTH * 2).collect::<String>()
            ));
        }
        if RESERVED.contains(&value) || is_device_name(value) {
            return Err(format!("profile name \"{value}\" is reserved"));
        }
        Ok(Self(value.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// The argument that selects this profile in a relaunched process.
    pub(crate) fn argument(&self) -> String {
        format!("{OPTION}={}", self.0)
    }
}

fn is_device_name(value: &str) -> bool {
    ["com", "lpt"].iter().any(|prefix| {
        value.strip_prefix(prefix).is_some_and(|digit| digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit())
    })
}

/// The selected profile, managed for the life of the process. Absent state
/// means the default profile, as it does for alternative entries and mobile.
#[derive(Clone, Debug, Default)]
pub(crate) struct LaunchProfile(pub Option<ProfileName>);

pub(crate) fn current<R: tauri::Runtime>(app: &impl tauri::Manager<R>) -> Option<ProfileName> {
    app.try_state::<LaunchProfile>().and_then(|state| state.0.clone())
}

/// Separates the profile option from the remaining arguments, which keep their
/// order. The option may appear anywhere, but only once and only as one token.
pub(crate) fn split(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<(Option<ProfileName>, Vec<OsString>), String> {
    let mut profile = None;
    let mut rest = Vec::new();
    for argument in arguments {
        let text = argument.to_string_lossy();
        let value = if text == OPTION {
            return Err(format!("{OPTION} needs a name: use {OPTION}=<name>"));
        } else if let Some(value) = text.strip_prefix(OPTION).and_then(|value| value.strip_prefix('=')) {
            value
        } else {
            rest.push(argument);
            continue;
        };
        if profile.is_some() {
            return Err(format!("{OPTION} may be given only once"));
        }
        // A lossily decoded name never passes validation, so the replacement
        // character cannot select a profile.
        profile = Some(ProfileName::parse(value)?);
    }
    Ok((profile, rest))
}

/// The single-instance key. The default profile keeps the bundle identifier, so
/// it still meets a running released instance; `None` lets the plugin use it.
pub(crate) fn instance_id(identifier: &str, agent: bool, profile: Option<&ProfileName>) -> Option<String> {
    let name = profile?.as_str();
    Some(if agent { format!("{identifier}.agent.p_{name}") } else { format!("{identifier}.p_{name}") })
}

pub(crate) fn window_title(title: &str, profile: &ProfileName) -> String {
    format!("{title} ({})", profile.as_str())
}

/// The macOS WebKit data store of a named profile. It is derived from the name
/// alone, so nothing has to be stored to find it again.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub(crate) fn webview_store(agent: bool, profile: &ProfileName) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::new()
        .chain_update(b"risunest-webview-store\0")
        .chain_update(if agent { b"agent\0".as_slice() } else { b"\0".as_slice() })
        .chain_update(profile.as_str())
        .finalize();
    let mut identifier = [0; 16];
    identifier.copy_from_slice(&digest[..16]);
    // UUID version 8 (custom) with the RFC 9562 variant.
    identifier[6] = (identifier[6] & 0x0f) | 0x80;
    identifier[8] = (identifier[8] & 0x3f) | 0x80;
    identifier
}

/// Reports a rejected option before anything else starts. A release build on
/// Windows has no console of its own, so it borrows the one it was started from.
pub(crate) fn report_rejected_option(message: &str) {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
    crate::native_log::record_startup_failure(message);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_option_is_removed_wherever_it_appears_and_other_arguments_keep_their_order() {
        assert_eq!(split(arguments(&["a.risum", "--flag"])).unwrap(), (None, arguments(&["a.risum", "--flag"])));
        for values in [
            &["--profile=test", "--remove-local-data", "--yes"][..],
            &["--remove-local-data", "--profile=test", "--yes"][..],
            &["--remove-local-data", "--yes", "--profile=test"][..],
        ] {
            let (profile, rest) = split(arguments(values)).unwrap();
            assert_eq!(profile.unwrap().as_str(), "test");
            assert_eq!(rest, arguments(&["--remove-local-data", "--yes"]));
        }
        // A file named like the profile stays an operand of its own.
        let (profile, rest) = split(arguments(&["--profile=test", "test"])).unwrap();
        assert_eq!(profile.unwrap().argument(), "--profile=test");
        assert_eq!(rest, arguments(&["test"]));
        // Similar spellings are not the option.
        assert_eq!(split(arguments(&["--profiles=x", "--PROFILE=x"])).unwrap().0, None);
    }

    #[test]
    fn every_malformed_option_is_an_error_never_the_default_profile() {
        for values in [
            &["--profile"][..],
            &["--profile", "test"][..],
            &["--profile="][..],
            &["--profile=a", "--profile=a"][..],
            &["--profile=a", "--profile=b"][..],
            &["--profile=Test"][..],
            &["--profile=-test"][..],
            &["--profile=_test"][..],
            &["--profile=te st"][..],
            &["--profile=te/st"][..],
            &["--profile=..\\test"][..],
            &["--profile=테스트"][..],
            &["--profile=default"][..],
            &["--profile=con"][..],
            &["--profile=nul"][..],
            &["--profile=com1"][..],
            &["--profile=lpt9"][..],
        ] {
            assert!(split(arguments(values)).is_err(), "{values:?} was accepted");
        }
        assert!(split(arguments(&[&format!("--profile={}", "a".repeat(33))])).is_err());
        for accepted in ["a", "0", "test-1", "work_2", "com10", "lpt", "defaults", &"z".repeat(32)] {
            assert_eq!(ProfileName::parse(accepted).unwrap().as_str(), accepted);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_text_is_rejected() {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = b"--profile=a".to_vec();
        bytes.push(0xff);
        assert!(split([OsString::from_vec(bytes)]).is_err());
    }

    #[test]
    fn named_profiles_get_their_own_keys_and_the_default_keeps_the_identifier() {
        let identifier = "io.github.rsyumi.risunest";
        let test = ProfileName::parse("test").unwrap();
        assert_eq!(instance_id(identifier, false, None), None);
        assert_eq!(instance_id(identifier, true, None), None);
        assert_eq!(instance_id(identifier, false, Some(&test)).as_deref(), Some("io.github.rsyumi.risunest.p_test"));
        assert_eq!(instance_id(identifier, true, Some(&test)).as_deref(), Some("io.github.rsyumi.risunest.agent.p_test"));
        assert_eq!(window_title("RisuNest", &test), "RisuNest (test)");
    }

    #[test]
    fn webview_stores_are_stable_and_distinct_per_name_and_build() {
        let store = |agent, name| webview_store(agent, &ProfileName::parse(name).unwrap());
        assert_eq!(store(false, "test"), store(false, "test"));
        assert_eq!(uuid::Uuid::from_bytes(store(false, "test")).get_version_num(), 8);
        assert_eq!(uuid::Uuid::from_bytes(store(false, "test")).get_variant(), uuid::Variant::RFC4122);
        assert_ne!(store(false, "test"), store(true, "test"));
        assert_ne!(store(false, "a-b"), store(false, "a_b"));
    }
}
