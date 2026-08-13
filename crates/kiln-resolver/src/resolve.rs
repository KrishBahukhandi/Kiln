//! Turning requirements into exact versions and artifact URLs.
//!
//! Resolution is the only step that depends on the outside world, so it is also
//! the step that decides how much of the outside world Kiln needs. Two
//! short-circuits do most of that work:
//!
//! 1. **A matching lockfile entry skips resolution entirely.** Same `kiln.toml`,
//!    same `kiln.lock`, same platform — no request is made at all.
//! 2. **An exact requirement skips the release index.** `node = "22.14.0"` needs
//!    one small checksum file, not a quarter-megabyte of JSON.
//!
//! Between them, a fully pinned project with a lockfile resolves offline, which
//! is what makes `kiln install` work on a plane.

use kiln_config::Manifest;
use kiln_core::error::Result;
use kiln_core::{Digest, Platform, Version, VersionReq};
use kiln_net::Http;
use kiln_runtime::{ArtifactSpec, ProviderContext, Registry};

use crate::lockfile::Lockfile;
use crate::plan::plan;

/// One requirement, resolved to exactly what will be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntime {
    /// The identifier from `kiln.toml`, e.g. `node`.
    pub id: String,
    /// The provider's human-facing name, e.g. `Node.js`.
    pub display_name: String,
    /// The requirement as written by the user.
    pub requirement: VersionReq,
    /// The exact version chosen.
    pub version: Version,
    /// Where to get it and what it must hash to.
    pub artifact: ArtifactSpec,
    /// Whether this came from `kiln.lock` rather than from the network.
    pub from_lockfile: bool,
}

/// Every requirement in a manifest, resolved for one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The platform this was resolved for.
    pub platform: Platform,
    /// The resolved runtimes, in manifest order.
    pub runtimes: Vec<ResolvedRuntime>,
}

impl Resolution {
    /// How many entries came from the lockfile rather than the network.
    pub fn locked_count(&self) -> usize {
        self.runtimes.iter().filter(|r| r.from_lockfile).count()
    }

    /// Whether resolution needed no network access at all.
    pub fn was_fully_locked(&self) -> bool {
        !self.runtimes.is_empty() && self.locked_count() == self.runtimes.len()
    }
}

/// Resolve a manifest, reusing `lockfile` where it still applies.
pub fn resolve(
    manifest: &Manifest,
    registry: &Registry,
    platform: &Platform,
    http: &Http,
    lockfile: Option<&Lockfile>,
) -> Result<Resolution> {
    // Offline validation first: a misspelled runtime should not cost a request.
    let planned = plan(manifest, registry, platform)?;
    let context = ProviderContext { http, platform };
    let locked = lockfile.and_then(|l| l.for_platform(platform));

    let mut runtimes = Vec::with_capacity(planned.len());
    for entry in planned {
        let provider = registry
            .get(&entry.id)
            .ok_or_else(|| registry.unknown(&entry.id))?;

        // The lockfile is only authoritative while the manifest still asks for
        // the same thing. Editing `kiln.toml` must re-resolve, or the lockfile
        // would quietly outrank the file a human just changed.
        if let Some(locked) = locked.and_then(|p| p.runtime.get(&entry.id))
            && locked.requirement == entry.requirement.to_string()
        {
            runtimes.push(ResolvedRuntime {
                id: entry.id,
                display_name: entry.display_name,
                requirement: entry.requirement,
                version: locked.version.clone(),
                artifact: ArtifactSpec {
                    url: locked.artifact.url.clone(),
                    digest: locked.artifact.digest.clone(),
                    format: locked.artifact.format,
                    size: locked.artifact.size,
                },
                from_lockfile: true,
            });
            continue;
        }

        let version = provider.resolve(&entry.requirement, &context)?;
        let artifact = provider.artifact(&version, &context)?;

        runtimes.push(ResolvedRuntime {
            id: entry.id,
            display_name: entry.display_name,
            requirement: entry.requirement,
            version,
            artifact,
            from_lockfile: false,
        });
    }

    Ok(Resolution {
        platform: *platform,
        runtimes,
    })
}

/// Record a resolution into a lockfile, preserving other platforms' entries.
///
/// Merging rather than replacing is what lets a repository accumulate a lockfile
/// covering everyone's machine: a Linux contributor's `kiln install` must not
/// delete the macOS entries a colleague committed.
///
/// Idempotent down to `generated_by`. Re-recording an unchanged resolution
/// returns the existing lockfile untouched, so upgrading Kiln does not rewrite
/// every project's lockfile — which would put noise in everyone's diff and turn
/// `--locked` into a tripwire that fires on the wrong thing.
pub fn record(
    resolution: &Resolution,
    manifest: &Manifest,
    existing: Option<Lockfile>,
) -> Lockfile {
    let platform_lock = platform_lock_for(resolution);
    let key = resolution.platform.key();

    let Some(existing) = existing else {
        let mut lockfile = Lockfile::new(manifest.project.name.as_str());
        lockfile.platform.insert(key, platform_lock);
        return lockfile;
    };

    let unchanged = existing.project.name == manifest.project.name.as_str()
        && existing.version == crate::lockfile::LOCKFILE_VERSION
        && existing.platform.get(&key) == Some(&platform_lock);
    if unchanged {
        return existing;
    }

    let mut updated = existing;
    updated.version = crate::lockfile::LOCKFILE_VERSION;
    updated.generated_by = format!("kiln {}", kiln_core::KILN_VERSION);
    updated.project.name = manifest.project.name.as_str().to_string();
    updated.platform.insert(key, platform_lock);
    updated
}

/// Record several platforms' resolutions into one lockfile.
///
/// This is what makes a lockfile useful to a team rather than to one laptop:
/// one person runs it, commits the result, and everyone else installs from it
/// without re-resolving — including the CI runner on a different OS.
pub fn record_all(
    resolutions: &[Resolution],
    manifest: &Manifest,
    existing: Option<Lockfile>,
) -> Lockfile {
    resolutions
        .iter()
        .fold(existing, |lockfile, resolution| {
            Some(record(resolution, manifest, lockfile))
        })
        .unwrap_or_else(|| Lockfile::new(manifest.project.name.as_str()))
}

fn platform_lock_for(resolution: &Resolution) -> crate::lockfile::PlatformLock {
    use crate::lockfile::{LockedArtifact, LockedRuntime, PlatformLock};

    let mut platform_lock = PlatformLock::default();
    for runtime in &resolution.runtimes {
        platform_lock.runtime.insert(
            runtime.id.clone(),
            LockedRuntime {
                provider: runtime.id.clone(),
                requirement: runtime.requirement.to_string(),
                version: runtime.version.clone(),
                artifact: LockedArtifact {
                    url: runtime.artifact.url.clone(),
                    digest: runtime.artifact.digest.clone(),
                    format: runtime.artifact.format,
                    size: runtime.artifact.size,
                },
            },
        );
    }
    platform_lock
}

/// The digests a resolution depends on, for cache reachability.
pub fn referenced_digests(resolution: &Resolution) -> Vec<Digest> {
    resolution
        .runtimes
        .iter()
        .map(|r| r.artifact.digest.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{LockedArtifact, LockedRuntime, PlatformLock};
    use kiln_core::{Arch, ArtifactFormat, HashAlgorithm, Os};
    use std::path::Path;

    fn manifest(text: &str) -> Manifest {
        kiln_config::parse_str(text, Path::new("kiln.toml")).expect("valid manifest")
    }

    fn macos() -> Platform {
        Platform::new(Os::MacOs, Arch::Aarch64, None)
    }

    fn digest() -> Digest {
        Digest::of_bytes(HashAlgorithm::Sha256, b"node-22.14.0")
    }

    /// A lockfile pinning `node = "22"` to 22.14.0 on macOS arm64.
    fn locked(requirement: &str) -> Lockfile {
        let mut lockfile = Lockfile::new("app");
        let mut platform = PlatformLock::default();
        platform.runtime.insert(
            "node".to_string(),
            LockedRuntime {
                provider: "node".to_string(),
                requirement: requirement.to_string(),
                version: Version::new(22, 14, 0),
                artifact: LockedArtifact {
                    url: "https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-arm64.tar.gz"
                        .to_string(),
                    digest: digest(),
                    format: ArtifactFormat::TarGz,
                    size: Some(45_678_901),
                },
            },
        );
        lockfile.platform.insert(macos().key(), platform);
        lockfile
    }

    const APP: &str = "[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\n";

    #[test]
    fn a_matching_lockfile_resolves_without_the_network() {
        // The client is offline: if resolution touched the network at all, this
        // would fail rather than pass.
        let resolution = resolve(
            &manifest(APP),
            &Registry::builtin(),
            &macos(),
            &Http::new(true),
            Some(&locked("22")),
        )
        .expect("a locked requirement needs no network");

        assert!(resolution.was_fully_locked());
        assert_eq!(resolution.runtimes.len(), 1);

        let node = &resolution.runtimes[0];
        assert_eq!(node.version, Version::new(22, 14, 0));
        assert_eq!(node.artifact.digest, digest());
        assert!(node.from_lockfile);
    }

    #[test]
    fn a_drifted_requirement_is_re_resolved() {
        // The lockfile says it locked `22`, but the manifest now asks for `20`.
        // Trusting the lockfile here would let it outrank a human's edit.
        let error = resolve(
            &manifest("[project]\nname = \"app\"\n[runtime]\nnode = \"20\"\n"),
            &Registry::builtin(),
            &macos(),
            &Http::new(true),
            Some(&locked("22")),
        )
        .unwrap_err();

        assert_eq!(error.kind(), kiln_core::ErrorKind::Network);
        assert!(error.reason().unwrap().contains("offline"));
    }

    #[test]
    fn a_lockfile_for_another_platform_is_not_used() {
        let linux = Platform::new(Os::Linux, Arch::X86_64, Some(kiln_core::Libc::Gnu));
        let error = resolve(
            &manifest(APP),
            &Registry::builtin(),
            &linux,
            &Http::new(true),
            Some(&locked("22")),
        )
        .unwrap_err();

        // It had to go to the network, which is exactly right: a macOS artifact
        // cannot satisfy a Linux install.
        assert_eq!(error.kind(), kiln_core::ErrorKind::Network);
    }

    #[test]
    fn validation_runs_before_anything_reaches_the_network() {
        let error = resolve(
            &manifest("[project]\nname = \"app\"\n[runtime]\nnodejs = \"22\"\n"),
            &Registry::builtin(),
            &macos(),
            &Http::new(true),
            None,
        )
        .unwrap_err();

        assert_eq!(error.kind(), kiln_core::ErrorKind::NotFound);
        assert!(error.summary().contains("nodejs"));
    }

    #[test]
    fn recording_preserves_other_platforms() {
        let resolution = resolve(
            &manifest(APP),
            &Registry::builtin(),
            &macos(),
            &Http::new(true),
            Some(&locked("22")),
        )
        .unwrap();

        // A colleague's Linux entry, already committed.
        let mut existing = locked("22");
        let mut linux_lock = PlatformLock::default();
        linux_lock.runtime.insert(
            "node".to_string(),
            LockedRuntime {
                provider: "node".to_string(),
                requirement: "22".to_string(),
                version: Version::new(22, 14, 0),
                artifact: LockedArtifact {
                    url: "https://nodejs.org/dist/v22.14.0/node-v22.14.0-linux-x64.tar.gz"
                        .to_string(),
                    digest: digest(),
                    format: ArtifactFormat::TarGz,
                    size: None,
                },
            },
        );
        existing
            .platform
            .insert("linux-x86_64-gnu".to_string(), linux_lock);

        let updated = record(&resolution, &manifest(APP), Some(existing));

        assert!(updated.platform.contains_key("macos-aarch64"));
        assert!(
            updated.platform.contains_key("linux-x86_64-gnu"),
            "a teammate's platform must survive someone else's install"
        );
    }

    #[test]
    fn recording_captures_what_is_needed_to_reproduce() {
        let resolution = resolve(
            &manifest(APP),
            &Registry::builtin(),
            &macos(),
            &Http::new(true),
            Some(&locked("22")),
        )
        .unwrap();

        let lockfile = record(&resolution, &manifest(APP), None);
        let entry = &lockfile.platform["macos-aarch64"].runtime["node"];

        assert_eq!(
            entry.requirement, "22",
            "the requirement is kept for drift detection"
        );
        assert_eq!(entry.version, Version::new(22, 14, 0));
        assert_eq!(entry.artifact.digest, digest());
        assert_eq!(lockfile.project.name, "app");
        assert_eq!(lockfile.version, crate::lockfile::LOCKFILE_VERSION);
    }

    #[test]
    fn a_lockfile_round_trips_through_resolution() {
        let first = record(
            &resolve(
                &manifest(APP),
                &Registry::builtin(),
                &macos(),
                &Http::new(true),
                Some(&locked("22")),
            )
            .unwrap(),
            &manifest(APP),
            None,
        );

        // Feeding the lockfile back in must resolve identically and offline.
        let second = resolve(
            &manifest(APP),
            &Registry::builtin(),
            &macos(),
            &Http::new(true),
            Some(&first),
        )
        .unwrap();

        assert!(second.was_fully_locked());
        assert_eq!(record(&second, &manifest(APP), None), first);
    }

    #[test]
    fn referenced_digests_cover_every_runtime() {
        let resolution = resolve(
            &manifest(APP),
            &Registry::builtin(),
            &macos(),
            &Http::new(true),
            Some(&locked("22")),
        )
        .unwrap();
        assert_eq!(referenced_digests(&resolution), vec![digest()]);
    }
}
