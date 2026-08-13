//! Where Kiln keeps its data.
//!
//! ```text
//! ~/.kiln/
//!   store/        content-addressed artifacts, immutable once written
//!   staging/      partially written artifacts, never visible as installed
//!   state/        metadata caches; safe to delete
//! ```
//!
//! `staging/` deliberately lives inside the same tree as `store/` so that
//! promoting a verified artifact is a `rename(2)` within one filesystem, which
//! is atomic. Staging under `/tmp` would make that a cross-device copy and open
//! a window where a half-written artifact is visible as an installed runtime.

use std::path::{Path, PathBuf};

use crate::error::{Error, IoResultExt, Result};

/// Environment variable that relocates the whole Kiln tree.
///
/// Set by the test suite, and useful for CI caching.
pub const KILN_HOME_ENV: &str = "KILN_HOME";

/// Resolved locations of Kiln's on-disk data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KilnPaths {
    root: PathBuf,
}

impl KilnPaths {
    /// Use an explicit root. Prefer [`KilnPaths::discover`] outside tests.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        KilnPaths { root: root.into() }
    }

    /// Locate Kiln's home directory: `$KILN_HOME` if set, otherwise `~/.kiln`.
    ///
    /// Kiln uses a dotfile rather than the platform's application-data directory
    /// because the store is developer-facing: people inspect it, measure it and
    /// delete it, and `~/Library/Application Support/Kiln` makes that hostile.
    pub fn discover() -> Result<Self> {
        if let Some(explicit) = std::env::var_os(KILN_HOME_ENV) {
            if explicit.is_empty() {
                return Err(Error::config(format!("{KILN_HOME_ENV} is set but empty"))
                    .because("Kiln cannot tell whether you meant a path or the default")
                    .hint(format!(
                        "unset {KILN_HOME_ENV}, or set it to an absolute path"
                    )));
            }
            return Ok(KilnPaths::with_root(PathBuf::from(explicit)));
        }

        let base = directories::BaseDirs::new().ok_or_else(|| {
            Error::not_found("Could not determine your home directory")
                .because("the operating system did not report a home directory for this user")
                .hint(format!(
                    "set {KILN_HOME_ENV} to the directory Kiln should use"
                ))
        })?;
        Ok(KilnPaths::with_root(base.home_dir().join(".kiln")))
    }

    /// The Kiln home directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Content-addressed artifact store. Entries here are immutable.
    pub fn store(&self) -> PathBuf {
        self.root.join("store")
    }

    /// Scratch space for artifacts being downloaded, verified and unpacked.
    pub fn staging(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// Derived metadata that can be regenerated, such as cached release indexes.
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// Create every directory Kiln needs, if it does not already exist.
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.store(),
            self.staging(),
            self.state(),
        ] {
            std::fs::create_dir_all(&dir)
                .io_context("Could not create Kiln's home directory", &dir)?;
        }
        Ok(())
    }

    /// Whether Kiln's home directory exists yet.
    pub fn exists(&self) -> bool {
        self.root.is_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_rooted_at_the_home_directory() {
        let paths = KilnPaths::with_root("/opt/kiln");
        assert_eq!(paths.root(), Path::new("/opt/kiln"));
        assert_eq!(paths.store(), Path::new("/opt/kiln/store"));
        assert_eq!(paths.staging(), Path::new("/opt/kiln/staging"));
        assert_eq!(paths.state(), Path::new("/opt/kiln/state"));
    }

    #[test]
    fn staging_shares_a_filesystem_with_the_store() {
        // Atomic promotion depends on this; assert the structural property.
        let paths = KilnPaths::with_root("/opt/kiln");
        assert_eq!(paths.staging().parent(), paths.store().parent());
    }

    #[test]
    fn ensure_creates_the_whole_tree_and_is_idempotent() {
        let root = std::env::temp_dir().join(format!("kiln-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = KilnPaths::with_root(&root);

        assert!(!paths.exists());
        paths.ensure().expect("create tree");
        paths.ensure().expect("second call must succeed");

        assert!(paths.exists());
        assert!(paths.store().is_dir());
        assert!(paths.staging().is_dir());
        assert!(paths.state().is_dir());

        std::fs::remove_dir_all(&root).ok();
    }
}
