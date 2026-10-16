pub fn resolve_update_public_key<'a>(
    forwarded: Option<&'a str>,
    legacy: Option<&'a str>,
    required: bool,
) -> Result<&'a str, &'static str> {
    let key = forwarded.or(legacy).unwrap_or("").trim();
    if required {
        risunest_release_update::decode_tauri_public_key(key).map_err(|_| {
            "Official distribution requires a valid updater verification public key in TAURI_RISUNEST_UPDATE_PUBLIC_KEY or RISUNEST_UPDATE_PUBLIC_KEY"
        })?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    #[test]
    fn distribution_accepts_the_mobile_forwarded_key_without_the_legacy_environment() {
        assert_eq!(resolve_update_public_key(Some(KEY), None, true).unwrap(), KEY);
    }

    #[test]
    fn existing_local_key_input_remains_supported() {
        assert_eq!(resolve_update_public_key(None, Some(KEY), true).unwrap(), KEY);
        assert_eq!(resolve_update_public_key(None, None, false).unwrap(), "");
        assert_eq!(resolve_update_public_key(Some("invalid"), None, false).unwrap(), "invalid");
    }

    #[test]
    fn distribution_rejects_missing_blank_and_invalid_native_keys() {
        for key in [None, Some(""), Some(" \n "), Some("invalid")] {
            assert!(resolve_update_public_key(key, None, true).is_err());
        }
    }

    #[test]
    fn forwarded_input_cannot_be_hidden_by_a_valid_legacy_key() {
        for key in ["", "invalid"] {
            assert!(resolve_update_public_key(Some(key), Some(KEY), true).is_err());
        }
        assert_eq!(resolve_update_public_key(Some(KEY), Some("invalid"), true).unwrap(), KEY);
    }
}
