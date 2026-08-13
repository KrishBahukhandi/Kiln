//! What a project's environment looks like *right now*, without touching the
//! network.
//!
//! Read from three places, in this order: the manifest says what is wanted, the
//! lockfile says what that resolved to, and the store says what is actually
//! here. Anything the lockfile does not answer, or that the store does not
//! hold, is reported as missing rather than fetched — `kiln run` must not
//! surprise anyone with a fifty-megabyte download.
//!
//! Four commands need this exact answer — `run`, `shell`, `list` and `doctor` —
//! so it lives here once. Three copies of "find the lockfile entry, check the
//! requirement still matches, look in the store" is three chances to disagree
//! about what "installed" means.

use std::path::PathBuf;

use kiln_cache::{ContentStore, StoreEntry};
use kiln_config::Manifest;
use kiln_core::error::{Error, Result};
use kiln_core::{Platform, Version, VersionReq};
use kiln_runtime::Registry;

use crate::lockfile::Lockfile;

/// One of a project's runtimes, and how far along it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRuntime {
    /// The identifier from `kiln.toml`.
    pub id: String,
    /// The provider's human-facing name.
    pub display_name: String,
    /// The requirement as written by the user.
    pub requirement: VersionReq,
    /// The version the lockfile pins, if it pins one that still applies.
    pub version: Option<Version>,
    /// The store entry, if the artifact is actually here.
    pub entry: Option<StoreEntry>,
    /// Directories this runtime contributes to `PATH`. Empty until installed.
    pub bin_dirs: Vec<PathBuf>,
}

impl ActiveRuntime {
    /// Whether this runtime is ready to use.
    pub fn is_installed(&self) -> bool {
        self.entry.is_some()
    }

    /// A short description of where this runtime has got to.
    pub fn state(&self) -> &'static str {
        match (&self.version, &self.entry) {
            (Some(_), Some(_)) => "installed",
            (Some(_), None) => "locked, not installed",
            (None, _) => "not resolved yet",
        }
    }
}

/// The project's environment as it currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEnvironment {
    /// The platform this was read for.
    pub platform: Platform,
    /// Every runtime the manifest pins, in manifest order.
    pub runtimes: Vec<ActiveRuntime>,
}

impl ProjectEnvironment {
    /// Runtimes that are not usable yet.
    pub fn missing(&self) -> Vec<&ActiveRuntime> {
        self.runtimes.iter().filter(|r| !r.is_installed()).collect()
    }

    /// Whether every pinned runtime is installed.
    pub fn is_ready(&self) -> bool {
        self.runtimes.iter().all(ActiveRuntime::is_installed)
    }

    /// The directories to prepend to `PATH`, in manifest order.
    ///
    /// Order is the manifest's: the first runtime that provides a `node` wins
    /// over a `node` bundled inside some later tool.
    pub fn path_entries(&self) -> Vec<PathBuf> {
        self.runtimes
            .iter()
            .flat_map(|runtime| runtime.bin_dirs.iter().cloned())
            .collect()
    }

    /// Fail unless every runtime is installed.
    ///
    /// Kiln does not install as a side effect of `kiln run`. Downloading fifty
    /// megabytes because someone asked to run a one-line script is a surprise,
    /// and surprises are what a reproducible environment is supposed to remove.
    pub fn require_ready(&self) -> Result<()> {
        let missing = self.missing();
        if missing.is_empty() {
            return Ok(());
        }

        let names: Vec<String> = missing
            .iter()
            .map(|runtime| match &runtime.version {
                Some(version) => format!("{} {version}", runtime.display_name),
                None => format!("{} {}", runtime.display_name, runtime.requirement),
            })
            .collect();

        Err(Error::not_found(format!(
            "The project environment is not installed: {}",
            names.join(", ")
        ))
        .because("Kiln does not download runtimes as a side effect of running a command.")
        .command("kiln install"))
    }
}

/// Read the project's environment from the lockfile and the store.
pub fn activate(
    manifest: &Manifest,
    registry: &Registry,
    platform: &Platform,
    store: &ContentStore,
    lockfile: Option<&Lockfile>,
) -> ProjectEnvironment {
    let locked = lockfile.and_then(|l| l.for_platform(platform));

    let runtimes = manifest
        .requirements()
        .map(|(name, requirement)| {
            let provider = registry.get(name.as_str());

            // A lockfile entry describes this manifest only while the
            // requirement still matches what it was locked against. Editing
            // `kiln.toml` must not leave a stale runtime silently in use.
            let entry = locked
                .and_then(|p| p.runtime.get(name.as_str()))
                .filter(|locked| locked.requirement == requirement.to_string());

            let stored = entry.and_then(|locked| store.get(&locked.artifact.digest));

            let bin_dirs = match (&stored, provider) {
                (Some(stored), Some(provider)) => provider
                    .layout()
                    .bin_dirs
                    .iter()
                    .map(|dir| stored.content_path().join(dir))
                    .collect(),
                _ => Vec::new(),
            };

            ActiveRuntime {
                id: name.to_string(),
                display_name: provider
                    .map(|p| p.display_name().to_string())
                    .unwrap_or_else(|| name.to_string()),
                requirement: requirement.clone(),
                version: entry.map(|e| e.version.clone()),
                entry: stored,
                bin_dirs,
            }
        })
        .collect();

    ProjectEnvironment {
        platform: *platform,
        runtimes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{LockedArtifact, LockedRuntime, PlatformLock};
    use kiln_cache::EntryMeta;
    use kiln_core::{ArtifactFormat, Digest, HashAlgorithm};
    use std::path::Path;

    fn manifest(text: &str) -> Manifest {
        kiln_config::parse_str(text, Path::new("kiln.toml")).expect("valid manifest")
    }

    fn host() -> Platform {
        Platform::detect().expect("host platform")
    }

    fn digest() -> Digest {
        Digest::of_bytes(HashAlgorithm::Sha256, b"node-22.14.0")
    }

    fn locked_for(requirement: &str, platform: &Platform) -> Lockfile {
        let mut lockfile = Lockfile::new("app");
        let mut lock = PlatformLock::default();
        lock.runtime.insert(
            "node".to_string(),
            LockedRuntime {
                provider: "node".to_string(),
                requirement: requirement.to_string(),
                version: Version::new(22, 14, 0),
                artifact: LockedArtifact {
                    url: "https://nodejs.org/x.tar.gz".to_string(),
                    digest: digest(),
                    format: ArtifactFormat::TarGz,
                    size: None,
                },
            },
        );
        lockfile.platform.insert(platform.key(), lock);
        lockfile
    }

    /// A store with the Node artifact really in it.
    struct Installed {
        _directory: tempfile::TempDir,
        store: ContentStore,
    }

    impl Installed {
        fn new(present: bool) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let store = ContentStore::new(directory.path().join("store"));
            if present {
                let content = directory.path().join("staged");
                std::fs::create_dir_all(content.join("bin")).unwrap();
                std::fs::write(content.join("bin/node"), b"#!/bin/sh\n").unwrap();
                store
                    .insert(
                        &content,
                        &digest(),
                        &EntryMeta::new("node", "22.14.0", "https://nodejs.org/x", &digest(), 10),
                    )
                    .unwrap();
            }
            Installed {
                _directory: directory,
                store,
            }
        }
    }

    const APP: &str = "[project]\nname = \"app\"\n[runtime]\nnode = \"22.14.0\"\n";

    #[test]
    fn an_installed_project_is_ready_and_contributes_a_bin_directory() {
        let installed = Installed::new(true);
        let environment = activate(
            &manifest(APP),
            &Registry::builtin(),
            &host(),
            &installed.store,
            Some(&locked_for("22.14.0", &host())),
        );

        assert!(environment.is_ready());
        assert!(environment.missing().is_empty());
        assert!(environment.require_ready().is_ok());

        let node = &environment.runtimes[0];
        assert_eq!(node.version, Some(Version::new(22, 14, 0)));
        assert_eq!(node.state(), "installed");

        let paths = environment.path_entries();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("content/bin"));
        assert!(paths[0].join("node").is_file());
    }

    #[test]
    fn a_locked_but_uninstalled_project_is_not_ready() {
        let installed = Installed::new(false);
        let environment = activate(
            &manifest(APP),
            &Registry::builtin(),
            &host(),
            &installed.store,
            Some(&locked_for("22.14.0", &host())),
        );

        assert!(!environment.is_ready());
        assert_eq!(environment.runtimes[0].state(), "locked, not installed");
        assert!(environment.path_entries().is_empty());

        let error = environment.require_ready().unwrap_err();
        assert!(error.summary().contains("Node.js 22.14.0"));
        assert!(error.hints().iter().any(|h| h.text() == "kiln install"));
    }

    #[test]
    fn an_unresolved_project_reports_the_requirement_instead_of_a_version() {
        let installed = Installed::new(false);
        let environment = activate(
            &manifest("[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\n"),
            &Registry::builtin(),
            &host(),
            &installed.store,
            None,
        );

        assert_eq!(environment.runtimes[0].state(), "not resolved yet");
        let error = environment.require_ready().unwrap_err();
        assert!(
            error.summary().contains("Node.js 22"),
            "{}",
            error.summary()
        );
    }

    #[test]
    fn editing_the_manifest_invalidates_the_lockfile_entry() {
        let installed = Installed::new(true);
        // Locked against `22.14.0`, but the manifest now asks for `20`.
        let environment = activate(
            &manifest("[project]\nname = \"app\"\n[runtime]\nnode = \"20\"\n"),
            &Registry::builtin(),
            &host(),
            &installed.store,
            Some(&locked_for("22.14.0", &host())),
        );

        // The artifact is in the store, but it is not the one this manifest
        // asks for, so it must not silently end up on PATH.
        assert!(!environment.is_ready());
        assert!(environment.path_entries().is_empty());
    }

    #[test]
    fn a_lockfile_for_another_platform_does_not_apply() {
        let installed = Installed::new(true);
        let other = Platform::new(kiln_core::Os::Windows, kiln_core::Arch::X86_64, None);
        let environment = activate(
            &manifest(APP),
            &Registry::builtin(),
            &host(),
            &installed.store,
            Some(&locked_for("22.14.0", &other)),
        );

        assert!(!environment.is_ready());
    }

    #[test]
    fn path_entries_follow_manifest_order() {
        let installed = Installed::new(true);
        let environment = activate(
            &manifest(
                "[project]\nname = \"app\"\n[runtime]\nnode = \"22.14.0\"\npython = \"3.13\"\n",
            ),
            &Registry::builtin(),
            &host(),
            &installed.store,
            Some(&locked_for("22.14.0", &host())),
        );

        // Only Node is installed, so only Node contributes — but the ordering
        // rule is that entries appear in the order the manifest lists them.
        let ids: Vec<&str> = environment.runtimes.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["node", "python"]);
        assert_eq!(environment.path_entries().len(), 1);
    }

    #[test]
    fn a_project_with_nothing_missing_needs_no_message() {
        let installed = Installed::new(true);
        let environment = activate(
            &manifest(APP),
            &Registry::builtin(),
            &host(),
            &installed.store,
            Some(&locked_for("22.14.0", &host())),
        );
        assert!(environment.require_ready().is_ok());
    }
}
