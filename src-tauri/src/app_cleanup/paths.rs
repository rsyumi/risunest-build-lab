use super::files::{self, Result};
use crate::app_paths::AppPaths;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(super) struct Paths {
    pub data: PathBuf,
    pub roots: Vec<PathBuf>,
    pub control: PathBuf,
    /// The root that overlaps the installation directory, when one does.
    /// Deleting it can never succeed, so the operation refuses instead.
    pub install_conflict: Option<PathBuf>,
}

impl Paths {
    pub fn validate(&self) -> Result<()> {
        files::validate(&self.control)?;
        for path in &self.roots {
            files::validate(path)?;
            if self.control.starts_with(path) || path.starts_with(&self.control) {
                return Err("cleanup-path-overlap".into());
            }
        }
        Ok(())
    }

    /// Runs before the operation mutates anything, including before the OS
    /// secret entries are removed. A configuration that cannot complete is
    /// reported as a fault rather than attempted and half applied.
    pub fn preflight(&self) -> Result<()> {
        self.validate()?;
        if self.install_conflict.is_some() {
            return Err("cleanup-root-overlaps-install".into());
        }
        Ok(())
    }

    pub fn request(&self) -> PathBuf {
        self.control.join("request.json")
    }

    pub fn from_app(app: &tauri::AppHandle) -> Result<Self> {
        let manifest =
            crate::app_paths::manifest(app).map_err(|_| "cleanup-path-unavailable".to_owned())?;
        let paths = Self::from_manifest(&manifest);
        #[cfg(mobile)]
        paths.validate()?;
        Ok(paths)
    }

    pub fn from_manifest(paths: &AppPaths) -> Self {
        Self {
            data: paths.data.clone(),
            roots: files::independent_roots(paths.owned_roots()),
            control: paths.cleanup_control.clone(),
            install_conflict: paths.install_conflict(),
        }
    }
}
