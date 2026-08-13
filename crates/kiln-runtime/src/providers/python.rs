//! Python, via the `python-build-standalone` project.
//!
//! python.org publishes source and macOS installers, but no relocatable binary
//! builds for Linux or macOS. Kiln uses `astral-sh/python-build-standalone`,
//! which is what every other version manager in this space uses, and which
//! publishes real musl builds — so Alpine works for Python even though it does
//! not for Node.js.
//!
//! One detail makes this provider simple: each release ships a single
//! `SHA256SUMS` file listing every asset. That one document is *both* the
//! version index and the source of every digest, so Kiln needs no GitHub API
//! calls and is not subject to their rate limit.

use std::path::Path;

use kiln_core::error::{Error, Result};
use kiln_core::{Arch, ArtifactFormat, Libc, Os, Platform, RuntimeLayout, Version, VersionReq};
use serde::Deserialize;

use crate::checksums;
use crate::detect::{read_capped, read_version_file, translate_requirement};
use crate::provider::{
    ArtifactSpec, Evidence, ProviderContext, Release, RuntimeKind, RuntimeProvider,
};

/// The project Kiln takes its Python builds from.
const REPO: &str = "https://github.com/astral-sh/python-build-standalone";

/// The asset flavour Kiln installs.
///
/// `install_only` is a relocatable prefix that is ready to run. The
/// `install_only_stripped` variant is smaller but has its debug symbols removed,
/// which turns a native-extension crash into an unreadable backtrace — a bad
/// trade for a *development* environment.
const FLAVOUR: &str = "install_only";

/// The release line `kiln init` proposes when a project does not say.
///
/// A `major.minor` pin, because that is the granularity at which Python is
/// actually compatible: 3.13 and 3.12 are different runtimes to a C extension.
const DEFAULT_REQUIREMENT: &str = "3.13";

/// Files that mean "this is a Python project" without naming a version.
const MARKERS: &[&str] = &[
    "pyproject.toml",
    ".python-version",
    "requirements.txt",
    "setup.py",
    "setup.cfg",
    "Pipfile",
];

/// The Python runtime provider.
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonProvider;

#[derive(Debug, Deserialize)]
struct PyProject {
    #[serde(default)]
    project: PyProjectTable,
}

#[derive(Debug, Default, Deserialize)]
struct PyProjectTable {
    #[serde(default, rename = "requires-python")]
    requires_python: Option<String>,
}

/// The Rust-style target triple python-build-standalone names its assets with.
fn triple_for(platform: &Platform) -> Option<&'static str> {
    match (platform.os, platform.arch, platform.libc) {
        (Os::MacOs, Arch::Aarch64, _) => Some("aarch64-apple-darwin"),
        (Os::MacOs, Arch::X86_64, _) => Some("x86_64-apple-darwin"),
        (Os::Linux, Arch::X86_64, Some(Libc::Gnu) | None) => Some("x86_64-unknown-linux-gnu"),
        (Os::Linux, Arch::Aarch64, Some(Libc::Gnu) | None) => Some("aarch64-unknown-linux-gnu"),
        (Os::Linux, Arch::X86_64, Some(Libc::Musl)) => Some("x86_64-unknown-linux-musl"),
        (Os::Linux, Arch::Aarch64, Some(Libc::Musl)) => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// The asset filename for one version, and the suffix that identifies it.
///
/// Matching on the whole suffix is what keeps the near-miss variants out:
/// `x86_64_v2-unknown-linux-gnu`, `-freethreaded-install_only` and
/// `-install_only_stripped` all fail to match `-x86_64-unknown-linux-gnu-install_only.tar.gz`.
fn asset_suffix(triple: &str) -> String {
    format!("-{triple}-{FLAVOUR}.tar.gz")
}

/// Recover the Python version from an asset filename, if it is one Kiln wants.
///
/// `cpython-3.13.15+20260807-aarch64-apple-darwin-install_only.tar.gz` → `3.13.15`
fn version_from_asset(name: &str, suffix: &str) -> Option<Version> {
    let middle = name.strip_prefix("cpython-")?.strip_suffix(suffix)?;
    // `3.13.15+20260807` — the part after `+` is the build tag, not the version.
    let version = middle.split('+').next()?;
    Version::parse(version).ok()
}

/// A release tag has to be safe to put in a URL path.
fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

impl RuntimeProvider for PythonProvider {
    fn id(&self) -> &'static str {
        "python"
    }

    fn display_name(&self) -> &'static str {
        "Python"
    }

    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Language
    }

    fn default_requirement(&self) -> VersionReq {
        VersionReq::parse(DEFAULT_REQUIREMENT)
            .expect("the built-in Python requirement is valid; asserted by unit test")
    }

    fn supports(&self, platform: &Platform) -> bool {
        triple_for(platform).is_some()
    }

    fn layout(&self) -> RuntimeLayout {
        // python/{bin,lib,include,share}
        RuntimeLayout::UNIX_PREFIX
    }

    fn releases(&self, ctx: &ProviderContext<'_>) -> Result<Vec<Release>> {
        let (_, sums, suffix) = self.index(ctx)?;

        let mut releases: Vec<Release> = checksums::entries(&sums)
            .filter_map(|(name, _)| version_from_asset(name, &suffix))
            .map(|version| Release {
                version,
                // python-build-standalone designates no LTS line, and neither
                // does CPython upstream.
                lts: None,
                available: true,
            })
            .collect();

        releases.sort_by(|a, b| a.version.cmp(&b.version));
        releases.dedup_by(|a, b| a.version == b.version);
        Ok(releases)
    }

    fn artifact(&self, version: &Version, ctx: &ProviderContext<'_>) -> Result<ArtifactSpec> {
        let (tag, sums, suffix) = self.index(ctx)?;

        let filename = format!("cpython-{version}+{tag}{suffix}");
        let digest = checksums::find(&sums, &filename)
            .ok_or_else(|| self.not_published(version, &sums, &suffix, ctx.platform))?;

        Ok(ArtifactSpec {
            url: format!("{REPO}/releases/download/{tag}/{filename}"),
            digest,
            format: ArtifactFormat::TarGz,
            size: None,
        })
    }

    fn detect(&self, project_root: &Path) -> Option<Evidence> {
        // `.python-version` is what pyenv writes, and it is unambiguous.
        if let Some(raw) = read_version_file(&project_root.join(".python-version"))
            && let Some(requirement) = translate_requirement(&raw)
        {
            return Some(Evidence::Pinned {
                requirement,
                source: ".python-version".to_string(),
            });
        }

        if let Some(text) = read_capped(&project_root.join("pyproject.toml"))
            && let Ok(parsed) = toml::from_str::<PyProject>(&text)
            && let Some(requires) = parsed.project.requires_python.as_deref()
            && let Some(requirement) = translate_requirement(requires)
        {
            return Some(Evidence::Pinned {
                requirement,
                source: "pyproject.toml (requires-python)".to_string(),
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
        "https://www.python.org"
    }
}

impl PythonProvider {
    /// The current release tag, its `SHA256SUMS`, and this platform's suffix.
    ///
    /// The tag comes from the redirect on `releases/latest`, which needs no
    /// authentication and is not rate-limited — unlike the GitHub API.
    fn index(&self, ctx: &ProviderContext<'_>) -> Result<(String, String, String)> {
        let platform = ctx.platform;
        let triple = triple_for(platform).ok_or_else(|| {
            Error::unsupported(format!("Python is not available for {platform}"))
                .because(self.unsupported_reason(platform))
                .hint(format!("see {REPO} for the platforms it builds"))
        })?;

        let location = ctx
            .http
            .redirect_target(&format!("{REPO}/releases/latest"), "the Python builds")?;

        let tag = location
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();

        if !valid_tag(&tag) {
            return Err(Error::new(
                kiln_core::ErrorKind::Network,
                "Could not determine the current Python build release",
            )
            .because(format!("`{location}` does not end in a usable release tag"))
            .hint("the upstream layout may have changed; please report this"));
        }

        let sums = ctx.http.get_text(
            &format!("{REPO}/releases/download/{tag}/SHA256SUMS"),
            "the Python build checksums",
        )?;

        Ok((tag, sums, asset_suffix(triple)))
    }

    /// Explain that a specific version is not in the current release.
    ///
    /// python-build-standalone rebuilds every supported line on each release and
    /// does not carry old patch versions forward, so "3.13.4 is not here" is
    /// common and needs a better answer than "not found".
    fn not_published(
        &self,
        version: &Version,
        sums: &str,
        suffix: &str,
        platform: &Platform,
    ) -> Error {
        let mut same_line: Vec<Version> = checksums::entries(sums)
            .filter_map(|(name, _)| version_from_asset(name, suffix))
            .filter(|v| v.major == version.major && v.minor == version.minor)
            .collect();
        same_line.sort();
        same_line.dedup();

        let error = Error::not_found(format!("Python {version} is not available for {platform}"))
            .because(
                "Kiln installs Python from python-build-standalone, which publishes the \
                 newest patch of each release line rather than every patch ever made.",
            );

        match same_line.last() {
            Some(newest) => error
                .expected(format!(
                    "the newest {}.{} build is {newest}",
                    version.major, version.minor
                ))
                .hint(format!(
                    "pin the line instead of the patch: python = \"{}.{}\"",
                    version.major, version.minor
                )),
            None => error.hint(format!(
                "no {}.{} builds are published at all; try a different release line",
                version.major, version.minor
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Project(std::path::PathBuf);

    impl Project {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("kiln-python-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Project(path)
        }

        fn write(&self, name: &str, contents: &str) -> &Self {
            std::fs::write(self.0.join(name), contents).unwrap();
            self
        }

        fn detect(&self) -> Option<Evidence> {
            PythonProvider.detect(&self.0)
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Asset names taken verbatim from a real `SHA256SUMS`, including the
    /// variants that must not be selected.
    const ASSETS: &[&str] = &[
        "cpython-3.13.15+20260807-aarch64-apple-darwin-install_only.tar.gz",
        "cpython-3.13.15+20260807-aarch64-apple-darwin-install_only_stripped.tar.gz",
        "cpython-3.13.15+20260807-aarch64-apple-darwin-freethreaded-install_only.tar.gz",
        "cpython-3.13.15+20260807-x86_64-unknown-linux-gnu-install_only.tar.gz",
        "cpython-3.13.15+20260807-x86_64_v3-unknown-linux-gnu-install_only.tar.gz",
        "cpython-3.13.15+20260807-x86_64-unknown-linux-musl-install_only.tar.gz",
        "cpython-3.12.14+20260807-aarch64-apple-darwin-install_only.tar.gz",
        "cpython-3.10.20+20260807-aarch64-apple-darwin-install_only.tar.gz",
        "cpython-3.14.1+20260807-aarch64-apple-darwin-install_only.tar.gz",
    ];

    fn sums_document() -> String {
        ASSETS
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{:064x}  {name}\n", i))
            .collect()
    }

    fn versions_for(platform: &Platform) -> Vec<String> {
        let suffix = asset_suffix(triple_for(platform).unwrap());
        let mut found: Vec<String> = ASSETS
            .iter()
            .filter_map(|name| version_from_asset(name, &suffix))
            .map(|v| v.to_string())
            .collect();
        found.sort();
        found
    }

    #[test]
    fn the_default_requirement_pins_a_release_line() {
        let requirement = PythonProvider.default_requirement();
        assert_eq!(requirement.to_string(), "3.13");
        assert!(requirement.alias().is_none());
    }

    #[test]
    fn triples_cover_the_platforms_upstream_builds() {
        let cases = [
            (
                Platform::new(Os::MacOs, Arch::Aarch64, None),
                "aarch64-apple-darwin",
            ),
            (
                Platform::new(Os::MacOs, Arch::X86_64, None),
                "x86_64-apple-darwin",
            ),
            (
                Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Gnu)),
                "x86_64-unknown-linux-gnu",
            ),
            (
                Platform::new(Os::Linux, Arch::Aarch64, Some(Libc::Gnu)),
                "aarch64-unknown-linux-gnu",
            ),
            (
                Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Musl)),
                "x86_64-unknown-linux-musl",
            ),
            (
                Platform::new(Os::Linux, Arch::Aarch64, Some(Libc::Musl)),
                "aarch64-unknown-linux-musl",
            ),
        ];
        for (platform, expected) in cases {
            assert_eq!(triple_for(&platform), Some(expected), "for {platform}");
            assert!(PythonProvider.supports(&platform));
        }
        assert!(!PythonProvider.supports(&Platform::new(Os::Windows, Arch::X86_64, None)));
    }

    #[test]
    fn python_supports_musl_even_though_node_does_not() {
        // Worth pinning down: an Alpine user gets Python but not Node.js, and
        // the two providers must be able to disagree.
        let alpine = Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Musl));
        assert!(PythonProvider.supports(&alpine));
        assert!(!crate::providers::NodeProvider.supports(&alpine));
    }

    #[test]
    fn only_the_plain_install_only_asset_is_selected() {
        let macos = versions_for(&Platform::new(Os::MacOs, Arch::Aarch64, None));
        // Not `_stripped`, not `freethreaded`, one entry per version.
        assert_eq!(macos, ["3.10.20", "3.12.14", "3.13.15", "3.14.1"]);
    }

    #[test]
    fn micro_architecture_variants_do_not_masquerade_as_the_base_triple() {
        // `x86_64_v3-unknown-linux-gnu` must not satisfy `x86_64-unknown-linux-gnu`.
        let linux = versions_for(&Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Gnu)));
        assert_eq!(linux, ["3.13.15"]);
    }

    #[test]
    fn musl_and_gnu_assets_are_kept_apart() {
        let musl = versions_for(&Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Musl)));
        assert_eq!(musl, ["3.13.15"]);
    }

    #[test]
    fn asset_names_that_are_not_ours_are_ignored() {
        let suffix = asset_suffix("aarch64-apple-darwin");
        for name in [
            "SHA256SUMS",
            "cpython-3.13.15+20260807-aarch64-apple-darwin-debug-full.tar.zst",
            "pypy-3.10-aarch64-apple-darwin-install_only.tar.gz",
            "cpython-not-a-version+20260807-aarch64-apple-darwin-install_only.tar.gz",
            "",
        ] {
            assert!(version_from_asset(name, &suffix).is_none(), "for {name}");
        }
    }

    #[test]
    fn the_artifact_url_is_built_from_the_tag_and_the_asset() {
        let suffix = asset_suffix("aarch64-apple-darwin");
        let filename = format!("cpython-3.13.15+20260807{suffix}");
        assert_eq!(
            filename,
            "cpython-3.13.15+20260807-aarch64-apple-darwin-install_only.tar.gz"
        );
        assert!(checksums::find(&sums_document(), &filename).is_some());
    }

    #[test]
    fn a_withdrawn_patch_release_points_at_the_line() {
        let error = PythonProvider.not_published(
            &Version::new(3, 13, 4),
            &sums_document(),
            &asset_suffix("aarch64-apple-darwin"),
            &Platform::new(Os::MacOs, Arch::Aarch64, None),
        );

        assert_eq!(error.kind(), kiln_core::ErrorKind::NotFound);
        assert!(error.expectation().unwrap().contains("3.13.15"));
        assert!(
            error
                .hints()
                .iter()
                .any(|h| h.text().contains("python = \"3.13\"")),
            "should suggest pinning the line: {:?}",
            error.hints()
        );
    }

    #[test]
    fn an_unpublished_release_line_says_so() {
        let error = PythonProvider.not_published(
            &Version::new(3, 7, 0),
            &sums_document(),
            &asset_suffix("aarch64-apple-darwin"),
            &Platform::new(Os::MacOs, Arch::Aarch64, None),
        );
        assert!(error.expectation().is_none());
        assert!(error.hints().iter().any(|h| h.text().contains("3.7")));
    }

    #[test]
    fn release_tags_are_validated_before_reaching_a_url() {
        assert!(valid_tag("20260807"));
        assert!(valid_tag("2026.08.07-1"));
        for hostile in [
            "",
            "../../etc",
            "a/b",
            "tag?x=1",
            "tag with space",
            &"x".repeat(65),
        ] {
            assert!(!valid_tag(hostile), "`{hostile}` should be rejected");
        }
    }

    #[test]
    fn the_layout_strips_the_python_directory() {
        let layout = PythonProvider.layout();
        assert_eq!(layout.strip_components, 1);
        assert_eq!(layout.bin_dirs, ["bin"]);
    }

    // --- detection -------------------------------------------------------

    #[test]
    fn detects_nothing_in_an_unrelated_directory() {
        let project = Project::new("empty");
        project.write("package.json", "{}");
        assert!(project.detect().is_none());
    }

    #[test]
    fn reads_the_pyenv_version_file() {
        let project = Project::new("pyenv");
        project.write(".python-version", "3.13.5\n");

        let evidence = project.detect().unwrap();
        assert_eq!(evidence.requirement().unwrap().to_string(), "3.13.5");
        assert_eq!(evidence.source(), ".python-version");
    }

    #[test]
    fn reads_requires_python_from_pyproject() {
        let project = Project::new("pyproject");
        project.write(
            "pyproject.toml",
            "[project]\nname = \"app\"\nrequires-python = \">=3.11\"\n",
        );

        let evidence = project.detect().unwrap();
        assert_eq!(evidence.requirement().unwrap().to_string(), ">=3.11");
        assert_eq!(evidence.source(), "pyproject.toml (requires-python)");
    }

    #[test]
    fn pep440_specifiers_collapse_to_the_version_they_mention() {
        let project = Project::new("pep440");
        project.write(
            "pyproject.toml",
            "[project]\nrequires-python = \"~=3.11\"\n",
        );
        assert_eq!(
            project.detect().unwrap().requirement().unwrap().to_string(),
            "3.11"
        );
    }

    #[test]
    fn the_version_file_outranks_pyproject() {
        let project = Project::new("precedence");
        project
            .write("pyproject.toml", "[project]\nrequires-python = \">=3.9\"\n")
            .write(".python-version", "3.13.5");

        assert_eq!(project.detect().unwrap().source(), ".python-version");
    }

    #[test]
    fn malformed_pyproject_does_not_fail_detection() {
        let project = Project::new("broken");
        project.write("pyproject.toml", "[project\nthis is not toml");

        let evidence = project.detect().expect("still a Python project");
        assert!(evidence.requirement().is_none());
        assert_eq!(evidence.source(), "pyproject.toml");
    }

    #[test]
    fn every_marker_identifies_a_python_project() {
        for marker in MARKERS {
            let project = Project::new("marker");
            project.write(marker, "");
            let evidence = project
                .detect()
                .unwrap_or_else(|| panic!("`{marker}` should identify a Python project"));
            assert!(evidence.requirement().is_none());
        }
    }
}
