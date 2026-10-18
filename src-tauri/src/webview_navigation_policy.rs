use url::Url;

pub(crate) fn allows_navigation(url: &Url, dev_url: Option<&Url>) -> bool {
    if url.username() != "" || url.password().is_some() { return false; }
    if url.scheme() == "about" && matches!(url.path(), "blank" | "srcdoc") { return true; }
    if let Some(dev) = dev_url {
        return url.scheme() == dev.scheme() && url.host_str() == dev.host_str()
            && url.port_or_known_default() == dev.port_or_known_default();
    }
    if cfg!(any(windows, target_os = "android")) {
        matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost") && url.port().is_none()
    } else {
        url.scheme() == "tauri" && url.host_str() == Some("localhost") && url.port().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_app_and_plugin_documents_without_allowing_external_navigation() {
        let app = if cfg!(any(windows, target_os = "android")) { "http://tauri.localhost/index.html#settings" } else { "tauri://localhost/index.html#settings" };
        for value in [app, "about:blank", "about:srcdoc"] {
            assert!(allows_navigation(&Url::parse(value).unwrap(), None), "{value}");
        }
        for value in ["https://example.com/", "https://tauri.localhost.example.com", "file:///tmp/file", "data:text/html,other", "blob:null/file", "javascript:void(0)", "http://user@tauri.localhost/", "http://tauri.localhost:9000/"] {
            assert!(!allows_navigation(&Url::parse(value).unwrap(), None), "{value}");
        }
    }

    #[test]
    fn development_navigation_is_limited_to_the_configured_origin() {
        let dev = Url::parse("http://localhost:5174/").unwrap();
        assert!(allows_navigation(&dev.join("index.html#settings").unwrap(), Some(&dev)));
        for value in ["http://localhost:5175", "http://localhost.evil:5174", "https://localhost:5174", "http://user@localhost:5174"] {
            assert!(!allows_navigation(&Url::parse(value).unwrap(), Some(&dev)));
        }
    }
}
