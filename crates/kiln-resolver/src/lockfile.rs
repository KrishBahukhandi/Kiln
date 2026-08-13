//! `kiln.lock` — the exact environment a manifest resolved to.
//!
//! The lockfile is keyed by platform rather than describing one machine. A team
//! is not all on the same hardware, and a lockfile that only knew about
//! `macos-aarch64` would either be useless to the person on Linux or would have
//! to be regenerated on every push, which defeats the point of locking.
//!
//! ```toml
//! version = 1
//! generated_by = "kiln 0.1.0"
//!
//! [project]
//! name = "example-app"
//!
//! [platform.macos-aarch64.runtime.node]
//! provider = "node"
//! requirement = "22"
//! version = "22.14.0"
//!
//! [platform.macos-aarch64.runtime.node.artifact]
//! url = "https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-arm64.tar.xz"
//! digest = "sha256:…"
//! format = "tar.xz"
//! size = 45678901
//! ```
//!
//! # Status
//!
//! The format and its compatibility rules are implemented and tested here.
//! Nothing writes a lockfile yet — that needs resolution, which is Phase 2 — so
//! this module is the contract, not the producer.

use std::collections::BTreeMap;
use std::path::Path;

use kiln_core::error::{Error, IoResultExt, Result};
use kiln_core::{ArtifactFormat, Digest, Version};
use serde::{Deserialize, Serialize};

/// The lockfile format version this build writes and understands.
///
/// Bumped only for changes that older Kiln cannot safely ignore. Additive
/// fields do not need a bump, because unknown fields are tolerated on read.
pub const LOCKFILE_VERSION: u32 = 1;

/// A parsed `kiln.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    /// Format version. See [`LOCKFILE_VERSION`].
    pub version: u32,
    /// Which Kiln produced this file. Diagnostic only.
    pub generated_by: String,
    /// Identity of the project this lockfile belongs to.
    pub project: LockedProject,
    /// Resolved environments, keyed by [`kiln_core::Platform::key`].
    #[serde(default)]
    pub platform: BTreeMap<String, PlatformLock>,
}

/// The `[project]` table of a lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedProject {
    /// The project name, copied from the manifest so a stray lockfile can be
    /// traced back to where it belongs.
    pub name: String,
}

/// Everything resolved for one platform.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformLock {
    /// Resolved runtimes and tools, keyed by their manifest name.
    #[serde(default)]
    pub runtime: BTreeMap<String, LockedRuntime>,
}

/// One resolved runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedRuntime {
    /// The provider that resolved it.
    pub provider: String,
    /// The requirement as written in `kiln.toml`, so a drifted manifest is
    /// detectable without re-resolving.
    pub requirement: String,
    /// The exact version chosen.
    pub version: Version,
    /// Where the bytes come from and what they must hash to.
    pub artifact: LockedArtifact,
}

/// The downloadable artifact backing a locked runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedArtifact {
    /// Where the artifact was published.
    ///
    /// Advisory: the [`digest`](Self::digest) is what makes an artifact
    /// acceptable, not the URL it arrived from. A mirror is fine; a different
    /// payload is not.
    pub url: String,
    /// The digest the downloaded bytes must have.
    pub digest: Digest,
    /// How the artifact is packed.
    pub format: ArtifactFormat,
    /// Size in bytes, when the publisher states one. Used for progress display
    /// and to reject an obviously wrong response early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl Lockfile {
    /// An empty lockfile for a project.
    pub fn new(project_name: impl Into<String>) -> Self {
        Lockfile {
            version: LOCKFILE_VERSION,
            generated_by: format!("kiln {}", kiln_core::KILN_VERSION),
            project: LockedProject {
                name: project_name.into(),
            },
            platform: BTreeMap::new(),
        }
    }

    /// Parse lockfile text.
    ///
    /// The version is checked before the body, so a lockfile from a newer Kiln
    /// produces an explanation instead of a parse error about a field that did
    /// not exist yet.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }

        let probe: VersionProbe = toml::from_str(text).map_err(|e| {
            Error::config(format!("Invalid {}", path.display()))
                .because(e.message().to_string())
                .hint("delete it and run `kiln install` to regenerate it")
        })?;

        if probe.version > LOCKFILE_VERSION {
            return Err(Error::unsupported(format!(
                "{} was written by a newer version of Kiln",
                path.display()
            ))
            .because(format!(
                "the file uses lockfile format {}, and this build understands up to {LOCKFILE_VERSION}",
                probe.version
            ))
            .hint("upgrade Kiln to match the rest of the project"));
        }

        toml::from_str(text).map_err(|e| {
            Error::config(format!("Invalid {}", path.display()))
                .because(e.message().to_string())
                .hint("delete it and run `kiln install` to regenerate it")
        })
    }

    /// Read a lockfile from disk. `Ok(None)` when there is none.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Lockfile::parse(&text, path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io("Could not read the lockfile", path, e)),
        }
    }

    /// Render the lockfile as TOML.
    pub fn render(&self) -> Result<String> {
        let mut text = toml::to_string_pretty(self)
            .map_err(|e| Error::internal("Could not serialise the lockfile").with_source(e))?;
        text.insert_str(
            0,
            "# Generated by Kiln. Commit this file; do not edit it by hand.\n\n",
        );
        Ok(text)
    }

    /// Write the lockfile atomically, so an interrupted write cannot leave a
    /// project holding a truncated lockfile.
    pub fn write(&self, path: &Path) -> Result<()> {
        let text = self.render()?;
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = directory.join(format!(".kiln.lock.{}.tmp", std::process::id()));

        std::fs::write(&temporary, &text).io_context("Could not write the lockfile", &temporary)?;
        if let Err(e) = std::fs::rename(&temporary, path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(Error::io("Could not write the lockfile", path, e));
        }
        Ok(())
    }

    /// The environment locked for a platform, if this lockfile covers it.
    pub fn for_platform(&self, platform: &kiln_core::Platform) -> Option<&PlatformLock> {
        self.platform.get(&platform.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::{Arch, HashAlgorithm, Os, Platform};

    fn sample() -> Lockfile {
        let mut lockfile = Lockfile::new("example-app");
        let mut macos = PlatformLock::default();
        macos.runtime.insert(
            "node".to_string(),
            LockedRuntime {
                provider: "node".to_string(),
                requirement: "22".to_string(),
                version: Version::new(22, 14, 0),
                artifact: LockedArtifact {
                    url: "https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-arm64.tar.xz"
                        .to_string(),
                    digest: Digest::of_bytes(HashAlgorithm::Sha256, b"node-artifact"),
                    format: ArtifactFormat::TarXz,
                    size: Some(45_678_901),
                },
            },
        );
        lockfile.platform.insert("macos-aarch64".to_string(), macos);
        lockfile
    }

    #[test]
    fn round_trips_through_toml() {
        let original = sample();
        let text = original.render().expect("render");
        let parsed = Lockfile::parse(&text, Path::new("kiln.lock")).expect("parse");
        assert_eq!(parsed, original);
    }

    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(sample().render().unwrap(), sample().render().unwrap());
    }

    #[test]
    fn rendered_lockfiles_warn_against_hand_editing() {
        assert!(
            sample()
                .render()
                .unwrap()
                .starts_with("# Generated by Kiln.")
        );
    }

    #[test]
    fn the_format_records_everything_needed_to_reproduce_a_download() {
        let text = sample().render().unwrap();
        for expected in [
            "version = 1",
            "macos-aarch64",
            "22.14.0",
            "sha256:",
            "tar.xz",
            "https://nodejs.org/",
        ] {
            assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
        }
    }

    #[test]
    fn platforms_are_looked_up_by_key() {
        let lockfile = sample();
        let macos = Platform::new(Os::MacOs, Arch::Aarch64, None);
        let linux = Platform::new(Os::Linux, Arch::X86_64, Some(kiln_core::Libc::Gnu));

        assert!(lockfile.for_platform(&macos).is_some());
        assert!(lockfile.for_platform(&linux).is_none());
        assert_eq!(
            lockfile.for_platform(&macos).unwrap().runtime["node"].version,
            Version::new(22, 14, 0)
        );
    }

    #[test]
    fn a_newer_lockfile_asks_the_user_to_upgrade() {
        let text = "version = 999\ngenerated_by = \"kiln 9.9.9\"\n\n[project]\nname = \"app\"\n";
        let error = Lockfile::parse(text, Path::new("kiln.lock")).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::Unsupported);
        assert!(error.reason().unwrap().contains("999"));
        assert!(error.hints().iter().any(|h| h.text().contains("upgrade")));
    }

    #[test]
    fn a_malformed_lockfile_suggests_regenerating_it() {
        let error = Lockfile::parse("this is not toml", Path::new("kiln.lock")).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::Config);
        assert!(
            error
                .hints()
                .iter()
                .any(|h| h.text().contains("kiln install"))
        );
    }

    #[test]
    fn a_missing_lockfile_is_not_an_error() {
        assert!(
            Lockfile::read(Path::new("/nonexistent/kiln.lock"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn writes_atomically_and_reads_back() {
        let directory = std::env::temp_dir().join(format!("kiln-lock-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("kiln.lock");

        sample().write(&path).expect("write");
        assert_eq!(Lockfile::read(&path).unwrap().unwrap(), sample());

        let leftovers: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn artifact_formats_use_their_conventional_spelling() {
        for (format, spelling) in [
            (ArtifactFormat::TarGz, "tar.gz"),
            (ArtifactFormat::TarXz, "tar.xz"),
            (ArtifactFormat::Zip, "zip"),
        ] {
            let encoded = toml::to_string(&Wrapper { format }).unwrap();
            assert!(encoded.contains(spelling), "{encoded}");
        }

        #[derive(Serialize)]
        struct Wrapper {
            format: ArtifactFormat,
        }
    }
}
