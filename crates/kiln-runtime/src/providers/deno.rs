//! Deno.
//!
//! Deno is the first runtime Kiln installs from a **zip** rather than a tarball,
//! and the first that is a single executable rather than a tree — the archive
//! contains one file, `deno`, and nothing else. Hence [`RuntimeLayout::FLAT`]:
//! nothing to strip, and the content root itself goes on `PATH`.
//!
//! The distribution is unusually well shaped for this:
//!
//! - `https://dl.deno.land/versions.json` lists every released version, so
//!   enumerating them needs no GitHub API and therefore has no rate limit. That
//!   matters more than it sounds: an unauthenticated GitHub runs out after sixty
//!   requests an hour, which a CI fleet on one NAT reaches immediately.
//! - Every artifact has a `.sha256sum` beside it, so a digest is one small
//!   request away and never has to be inferred.
//!
//! Deno publishes no musl build, exactly like Node.js, so `supports` reports
//! four platforms rather than six.

use std::path::Path;

use kiln_core::error::{Error, Result};
use kiln_core::{
    Arch, ArtifactFormat, Digest, Libc, Os, Platform, RuntimeLayout, Version, VersionReq,
};
use serde::Deserialize;

use crate::detect::{read_version_file, translate_requirement};
use crate::provider::{
    ArtifactSpec, Evidence, ProviderContext, Release, RuntimeKind, RuntimeProvider,
};

/// Every released version, newest first.
const INDEX_URL: &str = "https://dl.deno.land/versions.json";

/// Where the artifacts live.
const DOWNLOAD_BASE: &str = "https://dl.deno.land/release";

/// The release line `kiln init` proposes when a project does not say.
const DEFAULT_REQUIREMENT: &str = "2";

/// Files that mean "this is a Deno project" without naming a version.
const MARKERS: &[&str] = &["deno.json", "deno.jsonc", "deno.lock"];

/// The Deno runtime provider.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenoProvider;

#[derive(Debug, Deserialize)]
struct VersionIndex {
    /// Tags like `v2.9.5`, newest first. Named `cli` upstream because the same
    /// index once carried the standard library too.
    #[serde(default)]
    cli: Vec<String>,
}

/// How a platform is spelled in Deno's artifact names.
fn target_for(platform: &Platform) -> Option<&'static str> {
    match (platform.os, platform.arch, platform.libc) {
        (Os::MacOs, Arch::Aarch64, _) => Some("aarch64-apple-darwin"),
        (Os::MacOs, Arch::X86_64, _) => Some("x86_64-apple-darwin"),
        // Only glibc. Deno publishes no musl build, so a musl platform must be
        // reported as unsupported rather than handed a binary that cannot load.
        (Os::Linux, Arch::Aarch64, Some(Libc::Gnu) | None) => Some("aarch64-unknown-linux-gnu"),
        (Os::Linux, Arch::X86_64, Some(Libc::Gnu) | None) => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

/// Normalise a Deno tag into a [`Version`].
///
/// `v2.9.5` → 2.9.5, `v2.0.0-rc.10` → 2.0.0-rc.10. Unlike Go's, these are
/// ordinary semantic versions once the `v` is gone.
fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v')?).ok()
}

/// The artifact filename for a target.
fn filename(target: &str) -> String {
    format!("deno-{target}.zip")
}

impl RuntimeProvider for DenoProvider {
    fn id(&self) -> &'static str {
        "deno"
    }

    fn display_name(&self) -> &'static str {
        "Deno"
    }

    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Language
    }

    fn default_requirement(&self) -> VersionReq {
        VersionReq::parse(DEFAULT_REQUIREMENT)
            .expect("the built-in Deno requirement is valid; asserted by unit test")
    }

    fn supports(&self, platform: &Platform) -> bool {
        target_for(platform).is_some()
    }

    fn layout(&self) -> RuntimeLayout {
        // The zip holds one file, `deno`, with no wrapper directory.
        RuntimeLayout::FLAT
    }

    fn releases(&self, ctx: &ProviderContext<'_>) -> Result<Vec<Release>> {
        // Every listed version is built for every platform Deno supports, so
        // availability is a property of the platform, not of the release.
        let available = self.supports(ctx.platform);

        Ok(self
            .index(ctx)?
            .into_iter()
            .filter_map(|tag| {
                Some(Release {
                    // A tag Kiln cannot parse is skipped rather than fatal.
                    version: parse_tag(&tag)?,
                    // Deno designates no long-term-support line.
                    lts: None,
                    available,
                })
            })
            .collect())
    }

    fn artifact(&self, version: &Version, ctx: &ProviderContext<'_>) -> Result<ArtifactSpec> {
        let platform = ctx.platform;
        let target = self.target(platform)?;
        let name = filename(target);
        let url = format!("{DOWNLOAD_BASE}/v{version}/{name}");

        // The digest sits beside the artifact, one small request away. Fetching
        // it by the artifact's own URL means the two cannot be mismatched.
        let document = ctx
            .http
            .get_text(&format!("{url}.sha256sum"), "the Deno checksum")?;

        let digest = crate::checksums::find(&document, &name)
            .or_else(|| {
                // The sidecar describes exactly one file, so a bare digest with
                // no filename beside it is still unambiguous.
                document
                    .split_whitespace()
                    .next()
                    .and_then(|hex| Digest::parse(&format!("sha256:{hex}")).ok())
            })
            .ok_or_else(|| {
                Error::new(
                    kiln_core::ErrorKind::Network,
                    format!("Kiln could not read the checksum for Deno {version}"),
                )
                .because(format!("{url}.sha256sum did not contain a sha256 digest."))
                .hint("if this persists, the upstream format may have changed; please report it")
            })?;

        Ok(ArtifactSpec {
            url,
            digest,
            format: ArtifactFormat::Zip,
            // The index publishes no sizes, and guessing one would make the
            // progress bar lie.
            size: None,
        })
    }

    fn detect(&self, project_root: &Path) -> Option<Evidence> {
        // `.dvmrc` is what Deno's own version managers read.
        if let Some(raw) = read_version_file(&project_root.join(".dvmrc"))
            && let Some(requirement) = translate_requirement(raw.trim_start_matches('v'))
        {
            return Some(Evidence::Pinned {
                requirement,
                source: ".dvmrc".to_string(),
            });
        }

        let marker = MARKERS
            .iter()
            .find(|file| project_root.join(file).exists())?;

        Some(Evidence::Present {
            source: (*marker).to_string(),
        })
    }

    fn homepage(&self) -> &'static str {
        "https://deno.com"
    }
}

impl DenoProvider {
    fn target(&self, platform: &Platform) -> Result<&'static str> {
        target_for(platform).ok_or_else(|| {
            Error::unsupported(format!("Deno is not available for {platform}"))
                .because(self.unsupported_reason(platform))
                .hint("see https://github.com/denoland/deno/releases for what upstream publishes")
        })
    }

    /// Why this platform has no build, in terms a user can act on.
    fn unsupported_reason(&self, platform: &Platform) -> String {
        if platform.os == Os::Linux && platform.libc == Some(Libc::Musl) {
            "Deno publishes glibc builds only; there is no musl artifact.".to_string()
        } else {
            format!("Deno publishes no {platform} build.")
        }
    }

    fn index(&self, ctx: &ProviderContext<'_>) -> Result<Vec<String>> {
        let body = ctx.http.get_text(INDEX_URL, "the Deno release index")?;
        let index: VersionIndex = serde_json::from_str(&body).map_err(|e| {
            Error::new(
                kiln_core::ErrorKind::Network,
                "Could not read the Deno release index",
            )
            .because(format!(
                "{INDEX_URL} did not contain the expected JSON: {e}"
            ))
            .hint("if this persists, the upstream format may have changed; please report it")
            .with_source(e)
        })?;
        Ok(index.cli)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice of the real index, including the awkward entries.
    ///
    /// The pre-release tags are the point: `v2.0.0-rc.10` sorts *below*
    /// `v2.0.0` in semver, and a naive string comparison gets that backwards.
    const INDEX: &str = r#"{
      "cli": [
        "v2.9.5",
        "v2.9.4",
        "v2.0.0",
        "v2.0.0-rc.10",
        "v2.0.0-rc.9",
        "v1.46.3",
        "v1.0.0"
      ]
    }"#;

    struct Project(std::path::PathBuf);

    impl Project {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("kiln-deno-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Project(path)
        }

        fn write(&self, name: &str, contents: &str) -> &Self {
            std::fs::write(self.0.join(name), contents).unwrap();
            self
        }

        fn detect(&self) -> Option<Evidence> {
            DenoProvider.detect(&self.0)
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn platform(os: Os, arch: Arch, libc: Option<Libc>) -> Platform {
        Platform { os, arch, libc }
    }

    #[test]
    fn the_built_in_requirement_parses() {
        assert_eq!(
            DenoProvider.default_requirement(),
            VersionReq::parse(DEFAULT_REQUIREMENT).unwrap()
        );
    }

    #[test]
    fn the_real_index_parses() {
        let index: VersionIndex = serde_json::from_str(INDEX).unwrap();
        assert_eq!(index.cli.len(), 7);
        assert_eq!(index.cli[0], "v2.9.5");
    }

    #[test]
    fn tags_lose_their_v_and_keep_their_pre_release() {
        assert_eq!(parse_tag("v2.9.5"), Some(Version::new(2, 9, 5)));
        assert_eq!(
            parse_tag("v2.0.0-rc.10"),
            Some(Version::parse("2.0.0-rc.10").unwrap())
        );
        // Not a tag at all.
        assert_eq!(parse_tag("2.9.5"), None);
        assert_eq!(parse_tag("nightly"), None);
    }

    #[test]
    fn a_release_candidate_sorts_below_its_release() {
        // The reason pre-releases are worth parsing rather than skipping: a
        // requirement of `2.0.0` must not resolve to `2.0.0-rc.10`.
        let release = parse_tag("v2.0.0").unwrap();
        let candidate = parse_tag("v2.0.0-rc.10").unwrap();
        assert!(candidate < release);
    }

    #[test]
    fn every_supported_platform_has_a_target() {
        for platform in Platform::all_supported() {
            let supported = DenoProvider.supports(&platform);
            assert_eq!(supported, target_for(&platform).is_some());
        }
    }

    #[test]
    fn musl_is_reported_as_unsupported_rather_than_given_a_glibc_binary() {
        let musl = platform(Os::Linux, Arch::X86_64, Some(Libc::Musl));
        assert!(!DenoProvider.supports(&musl));

        // A glibc binary on musl fails at exec with a message about a missing
        // loader, which is nobody's idea of a good diagnostic.
        let error = DenoProvider.target(&musl).unwrap_err();
        assert!(error.reason().unwrap().contains("musl"));
    }

    #[test]
    fn artifact_names_match_what_upstream_publishes() {
        // Checked against the real release listing.
        assert_eq!(
            filename(target_for(&platform(Os::MacOs, Arch::Aarch64, None)).unwrap()),
            "deno-aarch64-apple-darwin.zip"
        );
        assert_eq!(
            filename(target_for(&platform(Os::Linux, Arch::X86_64, Some(Libc::Gnu))).unwrap()),
            "deno-x86_64-unknown-linux-gnu.zip"
        );
    }

    #[test]
    fn the_layout_is_a_bare_binary() {
        let layout = DenoProvider.layout();
        // The zip has no wrapper directory to strip, and the binary sits at the
        // root rather than in `bin/`.
        assert_eq!(layout.strip_components, 0);
        assert_eq!(
            layout.bin_paths(Path::new("/store/x/content")),
            [std::path::PathBuf::from("/store/x/content")]
        );
    }

    #[test]
    fn a_dvmrc_pins_the_version() {
        let project = Project::new("dvmrc");
        project.write(".dvmrc", "v2.9.5\n");

        match project.detect() {
            Some(Evidence::Pinned {
                requirement,
                source,
            }) => {
                assert_eq!(source, ".dvmrc");
                assert!(requirement.matches(&Version::new(2, 9, 5)));
            }
            other => panic!("expected a pin, got {other:?}"),
        }
    }

    #[test]
    fn a_dvmrc_without_the_v_works_too() {
        let project = Project::new("dvmrc-bare");
        project.write(".dvmrc", "2.9.5");
        assert!(matches!(project.detect(), Some(Evidence::Pinned { .. })));
    }

    #[test]
    fn config_files_mark_a_deno_project_without_a_version() {
        for marker in MARKERS {
            let project = Project::new("marker");
            project.write(marker, "{}");
            match project.detect() {
                Some(Evidence::Present { source }) => assert_eq!(&source, marker),
                other => panic!("expected presence for {marker}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_node_project_is_not_mistaken_for_a_deno_one() {
        let project = Project::new("node");
        project.write("package.json", "{}");
        assert!(project.detect().is_none());
    }

    #[test]
    fn a_checksum_sidecar_is_read_either_way_it_is_written() {
        let hex = "b796aadd131f6930560c1ee040cf0d6f53933fbb987464e9ff46bd7ea4830615";
        let name = "deno-aarch64-apple-darwin.zip";

        // The real format: digest, two spaces, filename.
        assert!(crate::checksums::find(&format!("{hex}  {name}\n"), name).is_some());
    }
}
