//! Deciding whether `kiln.lock` still describes `kiln.toml`.
//!
//! This is what makes Kiln safe to run in CI. `--locked` has to be able to say
//! "the lockfile would change" *before* anything is resolved, because the point
//! is to fail rather than to quietly pick a different version.
//!
//! The check is entirely offline and entirely structural: for this platform,
//! does every requirement in the manifest have a lockfile entry that was locked
//! against that same requirement, and does the lockfile mention nothing the
//! manifest no longer wants? If so, resolution will short-circuit to the
//! lockfile and produce exactly what is already on disk — so no network call is
//! needed to know the answer.

use std::fmt;

use kiln_config::Manifest;
use kiln_core::Platform;
use kiln_core::error::Error;

use crate::lockfile::Lockfile;

/// One way a lockfile and a manifest disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    /// The runtime the disagreement is about.
    pub runtime: String,
    /// What the disagreement is.
    pub reason: DriftReason,
}

/// Why an entry does not match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftReason {
    /// The manifest wants it and the lockfile has no entry for this platform.
    NotLocked {
        /// The requirement as written in the manifest.
        requirement: String,
    },
    /// The lockfile has an entry, but it was locked against a different
    /// requirement — someone edited `kiln.toml`.
    RequirementChanged {
        /// What the lockfile was locked against.
        locked: String,
        /// What the manifest asks for now.
        manifest: String,
    },
    /// The lockfile pins something the manifest no longer mentions.
    NoLongerRequired,
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            DriftReason::NotLocked { requirement } => {
                write!(f, "{} {requirement} is not locked", self.runtime)
            }
            DriftReason::RequirementChanged { locked, manifest } => write!(
                f,
                "{} is locked for `{locked}`, but kiln.toml asks for `{manifest}`",
                self.runtime
            ),
            DriftReason::NoLongerRequired => {
                write!(f, "{} is locked but no longer in kiln.toml", self.runtime)
            }
        }
    }
}

/// Every way `lockfile` fails to describe `manifest` on `platform`.
///
/// An empty result means resolution for this platform will come entirely from
/// the lockfile, so it will neither touch the network nor change the file.
pub fn drift(manifest: &Manifest, lockfile: Option<&Lockfile>, platform: &Platform) -> Vec<Drift> {
    let Some(lockfile) = lockfile else {
        // No lockfile at all: everything the manifest wants is unlocked.
        return manifest
            .requirements()
            .map(|(name, requirement)| Drift {
                runtime: name.to_string(),
                reason: DriftReason::NotLocked {
                    requirement: requirement.to_string(),
                },
            })
            .collect();
    };

    let locked = lockfile.for_platform(platform);
    let mut drifts = Vec::new();

    for (name, requirement) in manifest.requirements() {
        match locked.and_then(|p| p.runtime.get(name.as_str())) {
            None => drifts.push(Drift {
                runtime: name.to_string(),
                reason: DriftReason::NotLocked {
                    requirement: requirement.to_string(),
                },
            }),
            Some(entry) if entry.requirement != requirement.to_string() => drifts.push(Drift {
                runtime: name.to_string(),
                reason: DriftReason::RequirementChanged {
                    locked: entry.requirement.clone(),
                    manifest: requirement.to_string(),
                },
            }),
            Some(_) => {}
        }
    }

    // A runtime removed from the manifest leaves a stale entry behind, which
    // `kiln lock` would clean up — so it counts as drift.
    if let Some(locked) = locked {
        for name in locked.runtime.keys() {
            let still_wanted = manifest
                .requirements()
                .any(|(manifest_name, _)| manifest_name.as_str() == name);
            if !still_wanted {
                drifts.push(Drift {
                    runtime: name.clone(),
                    reason: DriftReason::NoLongerRequired,
                });
            }
        }
    }

    drifts.sort_by(|a, b| a.runtime.cmp(&b.runtime));
    drifts
}

/// Whether the lockfile fully describes the manifest for this platform.
pub fn is_up_to_date(
    manifest: &Manifest,
    lockfile: Option<&Lockfile>,
    platform: &Platform,
) -> bool {
    drift(manifest, lockfile, platform).is_empty()
}

/// The error to report when the lockfile is out of date and may not be changed.
///
/// Written to be read by whoever is staring at a red CI job: it names every
/// entry, says which side changed, and gives the one command that fixes it.
pub fn locked_error(drifts: &[Drift], platform: &Platform) -> Error {
    let detail = drifts
        .iter()
        .map(|drift| format!("  {drift}"))
        .collect::<Vec<_>>()
        .join("\n");

    Error::conflict(format!(
        "kiln.lock does not cover {platform}, and `--locked` was given"
    ))
    .because(format!(
        "Kiln would have to change the lockfile to continue:\n{detail}"
    ))
    .hint("run `kiln lock` and commit the result")
    .hint("drop `--locked` to update it as part of installing")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{LockedArtifact, LockedRuntime, PlatformLock};
    use kiln_core::{ArtifactFormat, Digest, HashAlgorithm, Version};
    use std::path::Path;

    fn manifest(text: &str) -> Manifest {
        kiln_config::parse_str(text, Path::new("kiln.toml")).expect("valid manifest")
    }

    fn host() -> Platform {
        Platform::detect().expect("host platform")
    }

    /// A lockfile pinning the named runtimes, each locked against `requirement`.
    fn lockfile_with(entries: &[(&str, &str)], platform: &Platform) -> Lockfile {
        let mut lockfile = Lockfile::new("app");
        let mut lock = PlatformLock::default();
        for (name, requirement) in entries {
            lock.runtime.insert(
                (*name).to_string(),
                LockedRuntime {
                    provider: (*name).to_string(),
                    requirement: (*requirement).to_string(),
                    version: Version::new(22, 14, 0),
                    artifact: LockedArtifact {
                        url: "https://example.test/x.tar.gz".to_string(),
                        digest: Digest::of_bytes(HashAlgorithm::Sha256, name.as_bytes()),
                        format: ArtifactFormat::TarGz,
                        size: None,
                    },
                },
            );
        }
        lockfile.platform.insert(platform.key(), lock);
        lockfile
    }

    const APP: &str = "[project]\nname = \"app\"\n[runtime]\nnode = \"22.14.0\"\n";

    #[test]
    fn a_matching_lockfile_has_no_drift() {
        let lockfile = lockfile_with(&[("node", "22.14.0")], &host());
        assert!(drift(&manifest(APP), Some(&lockfile), &host()).is_empty());
        assert!(is_up_to_date(&manifest(APP), Some(&lockfile), &host()));
    }

    #[test]
    fn no_lockfile_means_everything_is_unlocked() {
        let drifts = drift(&manifest(APP), None, &host());
        assert_eq!(drifts.len(), 1);
        assert_eq!(
            drifts[0].reason,
            DriftReason::NotLocked {
                requirement: "22.14.0".into()
            }
        );
        assert!(drifts[0].to_string().contains("is not locked"));
    }

    #[test]
    fn an_edited_requirement_is_drift() {
        // The lockfile was made for `22.14.0`; kiln.toml now asks for `22`.
        let lockfile = lockfile_with(&[("node", "22.14.0")], &host());
        let drifts = drift(
            &manifest("[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\n"),
            Some(&lockfile),
            &host(),
        );

        assert_eq!(
            drifts[0].reason,
            DriftReason::RequirementChanged {
                locked: "22.14.0".into(),
                manifest: "22".into(),
            }
        );
        let message = drifts[0].to_string();
        assert!(message.contains("locked for `22.14.0`"), "{message}");
        assert!(message.contains("asks for `22`"), "{message}");
    }

    #[test]
    fn a_newly_added_runtime_is_drift() {
        let lockfile = lockfile_with(&[("node", "22.14.0")], &host());
        let drifts = drift(
            &manifest(
                "[project]\nname = \"app\"\n[runtime]\nnode = \"22.14.0\"\npython = \"3.13\"\n",
            ),
            Some(&lockfile),
            &host(),
        );
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].runtime, "python");
    }

    #[test]
    fn a_removed_runtime_is_drift() {
        // A stale entry means `kiln lock` would rewrite the file, so `--locked`
        // has to notice even though nothing is missing.
        let lockfile = lockfile_with(&[("node", "22.14.0"), ("python", "3.13")], &host());
        let drifts = drift(&manifest(APP), Some(&lockfile), &host());

        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].runtime, "python");
        assert_eq!(drifts[0].reason, DriftReason::NoLongerRequired);
    }

    #[test]
    fn a_lockfile_for_another_platform_does_not_count() {
        let elsewhere = Platform::new(kiln_core::Os::Windows, kiln_core::Arch::X86_64, None);
        let lockfile = lockfile_with(&[("node", "22.14.0")], &elsewhere);

        assert!(!is_up_to_date(&manifest(APP), Some(&lockfile), &host()));
    }

    #[test]
    fn drift_is_reported_in_a_stable_order() {
        let lockfile = lockfile_with(&[], &host());
        let drifts = drift(
            &manifest("[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\npython = \"3.13\"\n"),
            Some(&lockfile),
            &host(),
        );
        let names: Vec<&str> = drifts.iter().map(|d| d.runtime.as_str()).collect();
        assert_eq!(names, ["node", "python"]);
    }

    #[test]
    fn the_locked_error_names_every_entry_and_the_fix() {
        let drifts = drift(
            &manifest("[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\npython = \"3.13\"\n"),
            None,
            &host(),
        );
        let error = locked_error(&drifts, &host());

        assert_eq!(error.kind(), kiln_core::ErrorKind::Conflict);
        let reason = error.reason().unwrap();
        assert!(reason.contains("node"), "{reason}");
        assert!(reason.contains("python"), "{reason}");
        assert!(error.hints().iter().any(|h| h.text().contains("kiln lock")));
    }
}
