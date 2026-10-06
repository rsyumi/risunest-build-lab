use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Appearance {
    pub(crate) background: String,
    pub(crate) caption: String,
    pub(crate) text: String,
    pub(crate) dark: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            background: "#282a36".into(),
            caption: "#21222c".into(),
            text: "#f8f8f2".into(),
            dark: true,
        }
    }
}

pub(crate) fn rgb(value: &str) -> Result<[u8; 3], String> {
    if value.len() != 7
        || !value.starts_with('#')
        || !value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return Err("Invalid Windows appearance color".into());
    }
    let value =
        u32::from_str_radix(&value[1..], 16).map_err(|_| "Invalid Windows appearance color")?;
    Ok([(value >> 16) as u8, (value >> 8) as u8, value as u8])
}

impl Appearance {
    pub(crate) fn validate(&self) -> Result<(), String> {
        rgb(&self.background)?;
        rgb(&self.caption)?;
        rgb(&self.text)?;
        Ok(())
    }

    pub(crate) fn color(&self, startup: bool) -> tauri::window::Color {
        let [r, g, b] = rgb(if startup {
            &self.caption
        } else {
            &self.background
        })
        .expect("validated appearance");
        tauri::window::Color(r, g, b, 255)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn read_hint(path: &std::path::Path) -> Appearance {
    use std::io::Read;
    let read = || -> Option<Appearance> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(1025)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > 1024 {
            return None;
        }
        let value: Appearance = serde_json::from_slice(&bytes).ok()?;
        value.validate().ok()?;
        Some(value)
    };
    read().unwrap_or_default()
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn startup_config(
    config: &tauri::utils::config::WindowConfig,
    appearance: &Appearance,
) -> tauri::utils::config::WindowConfig {
    let mut config = config.clone();
    if config.background_color.is_none() {
        config.background_color = Some(appearance.color(true));
    }
    // macOS supports application appearance, but not a public WebView background setter.
    #[cfg(target_os = "macos")]
    if config.theme.is_none() {
        config.theme = Some(if appearance.dark {
            tauri::Theme::Dark
        } else {
            tauri::Theme::Light
        });
    }
    config
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn configure<'a>(
    app: &'a tauri::AppHandle,
    config: &tauri::utils::config::WindowConfig,
    builder: tauri::WebviewWindowBuilder<'a, tauri::Wry, tauri::App>,
) -> tauri::WebviewWindowBuilder<'a, tauri::Wry, tauri::App> {
    let appearance = crate::app_paths::data_root(app)
        .ok()
        .map(|root| read_hint(&root.join("startup-appearance.json")))
        .unwrap_or_default();
    let configured = startup_config(config, &appearance);
    let builder = builder.background_color(configured.background_color.expect("startup color"));
    #[cfg(target_os = "macos")]
    let builder = builder.theme(configured.theme);
    builder
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tauri::command]
pub(crate) fn desktop_cache_appearance(
    window: tauri::WebviewWindow,
    appearance: Appearance,
) -> Result<(), String> {
    crate::native_log::logged_without_detail(
        "desktop_cache_appearance",
        (|| {
            use std::io::Write;
            if window.label() != "main" {
                return Err("Appearance is limited to the main window".into());
            }
            appearance.validate()?;
            let save = || -> Result<(), Box<dyn std::error::Error>> {
                let root = crate::app_paths::data_root(&window)?;
                std::fs::create_dir_all(&root)?;
                let mut file = tempfile::NamedTempFile::new_in(&root)?;
                file.write_all(&serde_json::to_vec(&appearance)?)?;
                file.persist(root.join("startup-appearance.json"))?;
                Ok(())
            };
            save().map_err(|_| "Could not cache desktop startup colors".into())
        })(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_malformed_and_oversized_hints_keep_safe_defaults() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("hint.json");
        assert_eq!(read_hint(&path), Appearance::default());
        for bytes in [
            b"{".to_vec(),
            b"null".to_vec(),
            vec![b' '; 1025],
            br##"{"background":"#ffffff","caption":"red","text":"#000000","dark":false}"##.to_vec(),
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(read_hint(&path), Appearance::default());
        }
    }

    #[test]
    fn startup_uses_light_dark_and_custom_hints_but_preserves_configured_color() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("hint.json");
        for (caption, dark) in [("#ffffff", false), ("#21222c", true), ("#abcdef", false)] {
            let value = Appearance {
                caption: caption.into(),
                dark,
                ..Appearance::default()
            };
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let loaded = read_hint(&path);
            assert_eq!(loaded, value);
            let mut config = tauri::utils::config::WindowConfig::default();
            assert_eq!(
                startup_config(&config, &loaded).background_color,
                Some(value.color(true))
            );
            let explicit = tauri::window::Color(1, 2, 3, 255);
            config.background_color = Some(explicit);
            assert_eq!(
                startup_config(&config, &loaded).background_color,
                Some(explicit)
            );
        }
    }
}
