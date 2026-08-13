//! Finding the project a command applies to.
//!
//! Kiln walks up from the working directory looking for `kiln.toml`, so
//! `kiln run` works from anywhere inside a repository rather than only at its
//! root. The walk stops at the filesystem root, and at a hard depth limit as a
//! second line of defence: a symlink loop in a mount point must not turn a typo
//! into an unbounded search.

use std::path::{Path, PathBuf};

use kiln_core::error::{Error, IoResultExt, Result};

use crate::manifest::{LOCKFILE_FILE, MANIFEST_FILE, Manifest};
use crate::parse;

/// How many parent directories Kiln will inspect before giving up.
const MAX_SEARCH_DEPTH: usize = 64;

/// A located project: its root, its manifest path, and the parsed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    root: PathBuf,
    manifest: Manifest,
}

impl Project {
    /// Find the nearest `kiln.toml` at or above `start`, and parse it.
    pub fn discover(start: &Path) -> Result<Self> {
        let manifest_path = find_manifest(start)?;
        Project::load(&manifest_path)
    }

    /// Load a project from a specific manifest path.
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let manifest = parse::parse_file(manifest_path)?;
        let root = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(Project { root, manifest })
    }

    /// The directory containing `kiln.toml`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path of the manifest itself.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    /// Where this project's lockfile lives, whether or not it exists yet.
    pub fn lockfile_path(&self) -> PathBuf {
        self.root.join(LOCKFILE_FILE)
    }

    /// The parsed manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

/// Search `start` and its parents for a manifest, returning its path.
pub fn find_manifest(start: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(start)?;

    let mut current = absolute.as_path();
    for _ in 0..MAX_SEARCH_DEPTH {
        let candidate = current.join(MANIFEST_FILE);
        if candidate.is_file() {
            return Ok(candidate);
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    Err(
        Error::not_found(format!("No {MANIFEST_FILE} found for this directory"))
            .because(format!(
                "Kiln looked in {} and every parent directory up to the filesystem root.",
                absolute.display()
            ))
            .hint("create one in the project root")
            .command("kiln init"),
    )
}

/// Resolve `path` against the working directory without touching the filesystem
/// beyond reading the working directory itself.
///
/// `canonicalize` is deliberately avoided: it resolves symlinks, and a developer
/// whose project lives behind a symlink should see the path they typed in error
/// messages, not the physical one.
fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd =
        std::env::current_dir().io_context("Could not determine the current directory", path)?;
    Ok(cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disposable directory tree.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("kiln-discover-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MANIFEST: &str = "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n";

    #[test]
    fn finds_a_manifest_in_the_current_directory() {
        let scratch = Scratch::new("here");
        std::fs::write(scratch.path().join(MANIFEST_FILE), MANIFEST).unwrap();

        let found = find_manifest(scratch.path()).expect("should find the manifest");
        assert_eq!(found, scratch.path().join(MANIFEST_FILE));
    }

    #[test]
    fn walks_up_from_a_nested_directory() {
        let scratch = Scratch::new("nested");
        std::fs::write(scratch.path().join(MANIFEST_FILE), MANIFEST).unwrap();
        let nested = scratch.path().join("src/components/widgets");
        std::fs::create_dir_all(&nested).unwrap();

        let project = Project::discover(&nested).expect("should discover the project");
        assert_eq!(project.root(), scratch.path());
        assert_eq!(project.manifest().project.name.as_str(), "app");
    }

    #[test]
    fn stops_at_the_nearest_manifest() {
        let scratch = Scratch::new("nearest");
        std::fs::write(scratch.path().join(MANIFEST_FILE), MANIFEST).unwrap();

        let inner = scratch.path().join("packages/api");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(
            inner.join(MANIFEST_FILE),
            "[project]\nname = \"api\"\n\n[runtime]\nnode = \"20.0.0\"\n",
        )
        .unwrap();

        let project = Project::discover(&inner).expect("discover");
        assert_eq!(project.manifest().project.name.as_str(), "api");
        assert_eq!(project.root(), inner);
    }

    #[test]
    fn a_directory_named_kiln_toml_is_not_a_manifest() {
        let scratch = Scratch::new("dir");
        std::fs::create_dir_all(scratch.path().join(MANIFEST_FILE)).unwrap();
        assert!(find_manifest(scratch.path()).is_err());
    }

    #[test]
    fn missing_manifests_explain_where_kiln_looked() {
        let scratch = Scratch::new("missing");
        let nested = scratch.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();

        // No manifest anywhere in the scratch tree; the search reaches the root.
        let error = find_manifest(&nested).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::NotFound);
        assert!(
            error
                .reason()
                .unwrap()
                .contains(&nested.display().to_string())
        );
        assert!(error.hints().iter().any(|h| h.text() == "kiln init"));
    }

    #[test]
    fn lockfile_sits_beside_the_manifest() {
        let scratch = Scratch::new("lock");
        std::fs::write(scratch.path().join(MANIFEST_FILE), MANIFEST).unwrap();

        let project = Project::discover(scratch.path()).unwrap();
        assert_eq!(project.lockfile_path(), scratch.path().join(LOCKFILE_FILE));
        assert_eq!(project.manifest_path(), scratch.path().join(MANIFEST_FILE));
    }
}
