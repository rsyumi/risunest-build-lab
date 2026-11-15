pub(crate) fn synthetic_profile(value: Option<&str>) -> Result<&str, &'static str> {
    let value = value.ok_or("native UI profile is required")?;
    if value.len() > 32 { return Err("invalid native UI profile"); }
    let (run, case) = value.strip_prefix("agent-ollama-")
        .and_then(|suffix| suffix.split_once('-'))
        .ok_or("native UI profile is not synthetic")?;
    if run.len() != 12 || !run.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        || case.is_empty() || !case.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("native UI profile is not synthetic");
    }
    Ok(value)
}

pub(crate) fn validate_startup(agent: bool, embedded: bool, profile: Option<&str>) -> Result<(), &'static str> {
    if !agent { return Err("native UI verification requires an agent build"); }
    if !embedded { return Err("native UI verification requires custom-protocol"); }
    synthetic_profile(profile)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_requires_agent_embedded_mode_and_owned_profile() {
        let profile = Some("agent-ollama-012345abcdef-0");
        assert!(validate_startup(false, true, profile).is_err());
        assert!(validate_startup(true, false, profile).is_err());
        assert_eq!(validate_startup(true, true, profile), Ok(()));
        assert!(validate_startup(true, true, None).is_err());
        assert!(validate_startup(true, true, Some("personal")).is_err());
    }

    #[test]
    fn profile_input_rejects_default_ordinary_and_malformed_names() {
        assert!(synthetic_profile(None).is_err());
        for value in ["", "default", "personal", "agent-other-012345abcdef-0",
            "agent-ollama-012345abcdeg-0", "agent-ollama-012345abcdef-", "agent-ollama-012345abcdef-x",
            "agent-ollama-012345abcde-0", "agent-ollama-012345abcdef-0/../personal",
            "agent-ollama-012345abcdef-000000000"] {
            assert!(synthetic_profile(Some(value)).is_err(), "accepted {value}");
        }
        assert_eq!(synthetic_profile(Some("agent-ollama-012345abcdef-12")), Ok("agent-ollama-012345abcdef-12"));
    }
}
