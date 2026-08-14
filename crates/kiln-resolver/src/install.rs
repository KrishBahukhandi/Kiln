//! Materialising a resolution: download, verify, unpack, store.
//!
//! The order matters and is not negotiable. For each runtime:
//!
//! 1. If the store already has the artifact's digest, stop — nothing to do.
//! 2. Download to staging, hashing in flight.
//! 3. Compare against the digest the publisher declared. A mismatch discards
//!    the bytes and fails.
//! 4. Unpack into staging.
//! 5. `rename(2)` the finished tree into the store.
//!
//! Only step 5 makes anything visible as installed, and it is atomic. An
//! install interrupted at any earlier point leaves the store exactly as it was,
//! and leaves at worst a directory in `staging/` that the next run cleans up.
//!
//! Installs run one at a time. Downloading in parallel would be faster and is on
//! the roadmap for Phase 7; doing it now would mean building multi-bar progress
//! and cross-thread error aggregation before the single-threaded path has ever
//! run against a real artifact.

use std::path::{Path, PathBuf};

use kiln_cache::{ContentStore, EntryMeta, StoreEntry};
use kiln_core::error::{Error, IoResultExt, Result};
use kiln_core::{KilnPaths, Version};
use kiln_net::{Download, Http, Progress, SilentProgress};
use kiln_runtime::Registry;

use crate::resolve::{Resolution, ResolvedRuntime};

/// Watches an installation happen, so the CLI can draw progress without this
/// crate knowing what a terminal is.
pub trait Observer {
    /// A runtime is already in the store and will not be fetched.
    fn reused(&mut self, runtime: &ResolvedRuntime);
    /// A download is about to start; return something to report progress to.
    fn downloading(&mut self, runtime: &ResolvedRuntime) -> Box<dyn Progress>;
    /// The archive is being unpacked into the store.
    fn unpacking(&mut self, runtime: &ResolvedRuntime);
    /// A runtime is now installed.
    fn installed(&mut self, runtime: &ResolvedRuntime, entry: &StoreEntry);
}

/// An [`Observer`] that reports nothing.
#[derive(Debug, Default)]
pub struct SilentObserver;

impl Observer for SilentObserver {
    fn reused(&mut self, _runtime: &ResolvedRuntime) {}
    fn downloading(&mut self, _runtime: &ResolvedRuntime) -> Box<dyn Progress> {
        Box::new(SilentProgress)
    }
    fn unpacking(&mut self, _runtime: &ResolvedRuntime) {}
    fn installed(&mut self, _runtime: &ResolvedRuntime, _entry: &StoreEntry) {}
}

/// One runtime, installed and ready to be put on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRuntime {
    /// The identifier from `kiln.toml`.
    pub id: String,
    /// The provider's human-facing name.
    pub display_name: String,
    /// The exact version installed.
    pub version: Version,
    /// Where it lives in the store.
    pub entry: StoreEntry,
    /// Directories to prepend to `PATH`, in order.
    pub bin_dirs: Vec<PathBuf>,
    /// Whether it was already present before this run.
    pub was_cached: bool,
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// Every runtime the project needs, installed.
    pub runtimes: Vec<InstalledRuntime>,
}

impl InstallOutcome {
    /// How many runtimes were already in the store.
    pub fn reused(&self) -> usize {
        self.runtimes.iter().filter(|r| r.was_cached).count()
    }

    /// How many runtimes were downloaded.
    pub fn downloaded(&self) -> usize {
        self.runtimes.len() - self.reused()
    }

    /// Every `PATH` entry the project environment needs, in order.
    pub fn path_entries(&self) -> Vec<PathBuf> {
        self.runtimes
            .iter()
            .flat_map(|r| r.bin_dirs.iter().cloned())
            .collect()
    }
}

/// Install everything in `resolution`.
pub fn install(
    resolution: &Resolution,
    registry: &Registry,
    paths: &KilnPaths,
    http: &Http,
    observer: &mut dyn Observer,
) -> Result<InstallOutcome> {
    paths.ensure()?;
    let store = ContentStore::new(paths.store());

    let mut installed = Vec::with_capacity(resolution.runtimes.len());
    for runtime in &resolution.runtimes {
        installed.push(install_one(
            runtime, registry, &store, paths, http, observer,
        )?);
    }
    Ok(InstallOutcome {
        runtimes: installed,
    })
}

fn install_one(
    runtime: &ResolvedRuntime,
    registry: &Registry,
    store: &ContentStore,
    paths: &KilnPaths,
    http: &Http,
    observer: &mut dyn Observer,
) -> Result<InstalledRuntime> {
    let provider = registry
        .get(&runtime.id)
        .ok_or_else(|| registry.unknown(&runtime.id))?;
    let layout = provider.layout();

    let bin_dirs_of = |entry: &StoreEntry| -> Vec<PathBuf> {
        layout
            .bin_dirs
            .iter()
            .map(|dir| entry.content_path().join(dir))
            .collect()
    };

    // Already installed: the whole point of content addressing.
    if let Some(entry) = store.get(&runtime.artifact.digest) {
        observer.reused(runtime);
        return Ok(InstalledRuntime {
            id: runtime.id.clone(),
            display_name: runtime.display_name.clone(),
            version: runtime.version.clone(),
            bin_dirs: bin_dirs_of(&entry),
            entry,
            was_cached: true,
        });
    }

    let workspace = Workspace::create(paths, &runtime.id, &runtime.version)?;
    let archive = workspace.path().join("artifact");

    let label = format!("{} {}", runtime.display_name, runtime.version);
    let mut progress = observer.downloading(runtime);
    let bytes = Download {
        url: &runtime.artifact.url,
        what: &label,
        expected: &runtime.artifact.digest,
        size: runtime.artifact.size,
    }
    .to_file(http, &archive, progress.as_mut())?;

    observer.unpacking(runtime);
    let content = kiln_cache::extract(
        &archive,
        runtime.artifact.format,
        workspace.path(),
        layout.strip_components,
    )?;

    // The archive is no longer needed and would otherwise be moved into the
    // store along with everything else in the workspace.
    let _ = std::fs::remove_file(&archive);

    let meta = EntryMeta::new(
        &runtime.id,
        runtime.version.to_string(),
        &runtime.artifact.url,
        &runtime.artifact.digest,
        bytes,
    );
    let entry = store.insert(&content, &runtime.artifact.digest, &meta)?;

    observer.installed(runtime, &entry);
    Ok(InstalledRuntime {
        id: runtime.id.clone(),
        display_name: runtime.display_name.clone(),
        version: runtime.version.clone(),
        bin_dirs: bin_dirs_of(&entry),
        entry,
        was_cached: false,
    })
}

/// A scratch directory under `~/.kiln/staging`, removed when it goes out of
/// scope.
///
/// It lives beside the store rather than in `/tmp` so that promoting a finished
/// entry is a rename within one filesystem. Cleaning up on drop means an
/// interrupted or failed install does not accumulate half-unpacked runtimes.
struct Workspace {
    path: PathBuf,
}

impl Workspace {
    fn create(paths: &KilnPaths, id: &str, version: &Version) -> Result<Self> {
        // The id is a validated runtime name and the version is parsed, so
        // neither can contribute a path separator.
        let path = paths
            .staging()
            .join(format!("{id}-{version}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .io_context("Could not create the staging directory", &path)?;
        Ok(Workspace { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Remove staging directories left behind by interrupted runs.
pub fn clean_staging(paths: &KilnPaths) -> Result<usize> {
    let staging = paths.staging();
    if !staging.is_dir() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in std::fs::read_dir(&staging)
        .io_context("Could not read the staging directory", &staging)?
        .flatten()
    {
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// The error to report when an install is asked for offline and the store does
/// not already have what it needs.
pub fn offline_gap(missing: &[&ResolvedRuntime]) -> Error {
    let names: Vec<String> = missing
        .iter()
        .map(|r| format!("{} {}", r.display_name, r.version))
        .collect();

    Error::new(
        kiln_core::ErrorKind::Network,
        format!(
            "{} not available locally",
            if names.len() == 1 {
                format!("{} is", names[0])
            } else {
                format!("{} are", names.join(", "))
            }
        ),
    )
    .because("Kiln is running offline, and these are not in the store yet.")
    .hint("run without `--offline` once, to fetch them")
    .command("kiln cache list")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::{Digest, HashAlgorithm};

    fn paths() -> (tempfile::TempDir, KilnPaths) {
        let directory = tempfile::tempdir().unwrap();
        let paths = KilnPaths::with_root(directory.path().join("kiln"));
        (directory, paths)
    }

    fn runtime(name: &str) -> ResolvedRuntime {
        ResolvedRuntime {
            id: "node".into(),
            display_name: name.into(),
            requirement: kiln_core::VersionReq::parse("22").unwrap(),
            version: Version::new(22, 14, 0),
            artifact: kiln_runtime::ArtifactSpec {
                url: "https://nodejs.org/x.tar.gz".into(),
                digest: Digest::of_bytes(HashAlgorithm::Sha256, b"x"),
                format: kiln_core::ArtifactFormat::TarGz,
                size: None,
            },
            from_lockfile: false,
        }
    }

    #[test]
    fn a_workspace_cleans_up_after_itself() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();

        let path = {
            let workspace = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();
            std::fs::write(workspace.path().join("artifact"), b"partial").unwrap();
            workspace.path().to_path_buf()
        };

        // A failed install must not leave a half-downloaded artifact behind.
        assert!(!path.exists());
    }

    #[test]
    fn a_workspace_lives_beside_the_store() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();
        let workspace = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();

        // Promotion into the store is a rename, which needs one filesystem.
        assert_eq!(workspace.path().parent(), Some(paths.staging().as_path()));
    }

    #[test]
    fn a_workspace_can_be_recreated_over_a_previous_attempt() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();

        let first = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();
        let path = first.path().to_path_buf();
        std::fs::write(path.join("artifact"), b"stale").unwrap();
        std::mem::forget(first); // simulate a killed process

        let second = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();
        assert!(
            !second.path().join("artifact").exists(),
            "a retry must not inherit the previous attempt's files"
        );
    }

    #[test]
    fn cleaning_staging_removes_orphans() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();
        std::fs::create_dir_all(paths.staging().join("node-22.14.0-999")).unwrap();
        std::fs::create_dir_all(paths.staging().join("python-3.13.5-998")).unwrap();

        assert_eq!(clean_staging(&paths).unwrap(), 2);
        assert!(paths.staging().read_dir().unwrap().next().is_none());
    }

    #[test]
    fn cleaning_a_store_that_was_never_created_is_fine() {
        let (_guard, paths) = paths();
        assert_eq!(clean_staging(&paths).unwrap(), 0);
    }

    #[test]
    fn installing_offline_names_what_is_missing() {
        let node = runtime("Node.js");
        let error = offline_gap(&[&node]);

        assert_eq!(error.kind(), kiln_core::ErrorKind::Network);
        assert!(error.summary().contains("Node.js 22.14.0 is"));
        assert!(error.hints().iter().any(|h| h.text().contains("--offline")));
    }

    #[test]
    fn the_offline_message_reads_correctly_for_several_runtimes() {
        let node = runtime("Node.js");
        let python = runtime("Python");
        let error = offline_gap(&[&node, &python]);
        assert!(error.summary().contains("are not available"));
    }

    #[test]
    fn outcome_counts_split_cached_from_downloaded() {
        let entry = StoreEntry {
            digest: Digest::of_bytes(HashAlgorithm::Sha256, b"x"),
            path: PathBuf::from("/store/x"),
            meta: None,
            last_used: None,
        };
        let make = |cached: bool| InstalledRuntime {
            id: "node".into(),
            display_name: "Node.js".into(),
            version: Version::new(22, 14, 0),
            entry: entry.clone(),
            bin_dirs: vec![PathBuf::from("/store/x/content/bin")],
            was_cached: cached,
        };

        let outcome = InstallOutcome {
            runtimes: vec![make(true), make(false), make(true)],
        };
        assert_eq!(outcome.reused(), 2);
        assert_eq!(outcome.downloaded(), 1);
        assert_eq!(outcome.path_entries().len(), 3);
    }
}
