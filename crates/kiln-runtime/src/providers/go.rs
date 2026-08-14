//! Go.
//!
//! The friendliest feed of the three. `https://go.dev/dl/?mode=json` lists every
//! release with its files, and each file carries its **`sha256` inline** — so
//! unlike Node.js and Python, Go needs no second request to learn what an
//! artifact must hash to.
//!
//! Two details of Go's versioning shape this provider:
//!
//! - Releases are named `go1`, `go1.20`, `go1.26.6`, `go1.27rc3` — one, two or
//!   three components, with a pre-release suffix that has no separator before
//!   it. Kiln normalises all of that into a [`Version`].
//! - Artifact filenames use the *upstream* string verbatim: `go1.20`'s tarball
//!   is `go1.20.darwin-arm64.tar.gz`, not `go1.20.0...`. So Kiln takes filenames
//!   from the index rather than rebuilding them from a normalised version, which
//!   would be wrong for exactly the releases that are easiest to get wrong.

use std::path::Path;

use kiln_core::error::{Error, Result};
use kiln_core::{
    Arch, ArtifactFormat, Digest, HashAlgorithm, Os, Platform, RuntimeLayout, Version, VersionReq,
};
use serde::Deserialize;

use crate::detect::{read_capped, read_version_file, translate_requirement};
use crate::provider::{
    ArtifactSpec, Evidence, ProviderContext, Release, RuntimeKind, RuntimeProvider,
};

/// The release index. `include=all` is needed: without it the feed lists only
/// the two newest release lines, which cannot satisfy a pin like `1.22`.
const INDEX_URL: &str = "https://go.dev/dl/?mode=json&include=all";

/// Where the tarballs live. Redirects to Google's CDN.
const DOWNLOAD_BASE: &str = "https://go.dev/dl";

/// The release line `kiln init` proposes when a project does not say.
const DEFAULT_REQUIREMENT: &str = "1.26";

/// Files that mean "this is a Go project" without naming a version.
const MARKERS: &[&str] = &["go.mod", "go.work", ".go-version", "go.sum"];

/// The Go runtime provider.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoProvider;

#[derive(Debug, Deserialize)]
struct GoRelease {
    version: String,
    #[serde(default)]
    files: Vec<GoFile>,
}

#[derive(Debug, Deserialize)]
struct GoFile {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    arch: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    size: u64,
}

/// How a platform is spelled in Go's index.
fn target_for(platform: &Platform) -> Option<(&'static str, &'static str)> {
    match (platform.os, platform.arch) {
        (Os::MacOs, Arch::Aarch64) => Some(("darwin", "arm64")),
        (Os::MacOs, Arch::X86_64) => Some(("darwin", "amd64")),
        // Go publishes one Linux build per architecture, with no libc
        // qualifier. The toolchain binaries are statically linked, so the same
        // artifact serves glibc and musl alike.
        (Os::Linux, Arch::Aarch64) => Some(("linux", "arm64")),
        (Os::Linux, Arch::X86_64) => Some(("linux", "amd64")),
        _ => None,
    }
}

/// Normalise a Go release string into a [`Version`].
///
/// `go1` → 1.0.0, `go1.20` → 1.20.0, `go1.26.6` → 1.26.6, `go1.27rc3` →
/// 1.27.0-rc3. The pre-release suffix runs straight into the number with no
/// separator, which is why this cannot be handed to a general version parser.
fn parse_go_version(raw: &str) -> Option<Version> {
    let text = raw.strip_prefix("go")?;

    let (core, pre) = match text.find(|c: char| c.is_ascii_alphabetic()) {
        Some(index) => (&text[..index], Some(&text[index..])),
        None => (text, None),
    };

    let mut components: Vec<&str> = core.split('.').collect();
    if components.is_empty() || components.len() > 3 {
        return None;
    }
    while components.len() < 3 {
        components.push("0");
    }

    let normalised = match pre {
        Some(pre) => format!("{}-{pre}", components.join(".")),
        None => components.join("."),
    };
    Version::parse(&normalised).ok()
}

/// The `go` directive from a `go.mod`.
///
/// ```text
/// module example.com/app
///
/// go 1.22.0
/// ```
fn go_directive(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("go "))
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

impl RuntimeProvider for GoProvider {
    fn id(&self) -> &'static str {
        "go"
    }

    fn display_name(&self) -> &'static str {
        "Go"
    }

    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Language
    }

    fn default_requirement(&self) -> VersionReq {
        VersionReq::parse(DEFAULT_REQUIREMENT)
            .expect("the built-in Go requirement is valid; asserted by unit test")
    }

    fn supports(&self, platform: &Platform) -> bool {
        target_for(platform).is_some()
    }

    fn layout(&self) -> RuntimeLayout {
        // The tarball unpacks to `go/{bin,pkg,src,lib}`.
        RuntimeLayout::UNIX_PREFIX
    }

    fn releases(&self, ctx: &ProviderContext<'_>) -> Result<Vec<Release>> {
        let (os, arch) = self.target(ctx.platform)?;
        let releases = self.index(ctx)?;

        Ok(releases
            .into_iter()
            .filter_map(|release| {
                // A release Kiln cannot parse is skipped, not fatal: one odd
                // historical tag must not break every install.
                let version = parse_go_version(&release.version)?;
                Some(Release {
                    version,
                    // Go designates no long-term-support line.
                    lts: None,
                    available: release.files.iter().any(|f| archive_matches(f, os, arch)),
                })
            })
            .collect())
    }

    fn artifact(&self, version: &Version, ctx: &ProviderContext<'_>) -> Result<ArtifactSpec> {
        let (os, arch) = self.target(ctx.platform)?;
        let platform = ctx.platform;

        let release = self
            .index(ctx)?
            .into_iter()
            .find(|release| parse_go_version(&release.version).as_ref() == Some(version))
            .ok_or_else(|| {
                Error::not_found(format!("Go {version} was not published"))
                    .because("It does not appear in the go.dev release index.")
                    .command("kiln list")
            })?;

        let file = release
            .files
            .into_iter()
            .find(|f| archive_matches(f, os, arch))
            .ok_or_else(|| {
                Error::not_found(format!("Go {version} is not built for {platform}"))
                    .because(format!(
                        "The go.dev index lists no {os}/{arch} archive for this release."
                    ))
                    .hint("choose a version with a build for your platform")
            })?;

        // The digest travels in the index itself, so there is no second request
        // and no chance of pairing an artifact with another release's checksum.
        let digest = Digest::parse(&format!("{}:{}", HashAlgorithm::Sha256, file.sha256)).map_err(
            |error| {
                Error::new(
                    kiln_core::ErrorKind::Network,
                    format!("The go.dev index has no usable checksum for Go {version}"),
                )
                .because(error.summary().to_string())
                .hint("if this persists, the upstream format may have changed; please report it")
            },
        )?;

        Ok(ArtifactSpec {
            url: format!("{DOWNLOAD_BASE}/{}", file.filename),
            digest,
            format: ArtifactFormat::TarGz,
            size: (file.size > 0).then_some(file.size),
        })
    }

    fn detect(&self, project_root: &Path) -> Option<Evidence> {
        if let Some(raw) = read_version_file(&project_root.join(".go-version"))
            && let Some(requirement) = translate_requirement(&raw)
        {
            return Some(Evidence::Pinned {
                requirement,
                source: ".go-version".to_string(),
            });
        }

        if let Some(text) = read_capped(&project_root.join("go.mod"))
            && let Some(directive) = go_directive(&text)
            && let Some(requirement) = translate_requirement(&directive)
        {
            return Some(Evidence::Pinned {
                requirement,
                source: "go.mod (go directive)".to_string(),
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
        "https://go.dev"
    }
}

impl GoProvider {
    fn target(&self, platform: &Platform) -> Result<(&'static str, &'static str)> {
        target_for(platform).ok_or_else(|| {
            Error::unsupported(format!("Go is not available for {platform}"))
                .because(self.unsupported_reason(platform))
                .hint("see https://go.dev/dl for what upstream publishes")
        })
    }

    fn index(&self, ctx: &ProviderContext<'_>) -> Result<Vec<GoRelease>> {
        let body = ctx.http.get_text(INDEX_URL, "the Go release index")?;
        serde_json::from_str(&body).map_err(|e| {
            Error::new(
                kiln_core::ErrorKind::Network,
                "Could not read the Go release index",
            )
            .because(format!(
                "{INDEX_URL} did not contain the expected JSON: {e}"
            ))
            .hint("if this persists, the upstream format may have changed; please report it")
            .with_source(e)
        })
    }
}

/// Whether this index entry is the tarball Kiln wants.
fn archive_matches(file: &GoFile, os: &str, arch: &str) -> bool {
    file.kind == "archive"
        && file.os == os
        && file.arch == arch
        && file.filename.ends_with(".tar.gz")
        && !file.sha256.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::Libc;

    struct Project(std::path::PathBuf);

    impl Project {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("kiln-go-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Project(path)
        }

        fn write(&self, name: &str, contents: &str) -> &Self {
            std::fs::write(self.0.join(name), contents).unwrap();
            self
        }

        fn detect(&self) -> Option<Evidence> {
            GoProvider.detect(&self.0)
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A slice of the real index, including every awkward version shape.
    const INDEX: &str = r#"[
      { "version": "go1.27rc3", "stable": false, "files": [
        { "filename": "go1.27rc3.darwin-arm64.tar.gz", "os": "darwin", "arch": "arm64",
          "kind": "archive", "sha256": "205d5695db35df3200000000000000000000000000000000000000000000aaaa", "size": 68300849 }
      ]},
      { "version": "go1.26.6", "stable": true, "files": [
        { "filename": "go1.26.6.darwin-arm64.tar.gz", "os": "darwin", "arch": "arm64",
          "kind": "archive", "sha256": "1111111111111111111111111111111111111111111111111111111111111111", "size": 100 },
        { "filename": "go1.26.6.linux-amd64.tar.gz", "os": "linux", "arch": "amd64",
          "kind": "archive", "sha256": "2222222222222222222222222222222222222222222222222222222222222222", "size": 200 },
        { "filename": "go1.26.6.darwin-arm64.pkg", "os": "darwin", "arch": "arm64",
          "kind": "installer", "sha256": "3333333333333333333333333333333333333333333333333333333333333333", "size": 300 }
      ]},
      { "version": "go1.20", "stable": true, "files": [
        { "filename": "go1.20.darwin-arm64.tar.gz", "os": "darwin", "arch": "arm64",
          "kind": "archive", "sha256": "4444444444444444444444444444444444444444444444444444444444444444", "size": 400 }
      ]},
      { "version": "go1.19beta1", "stable": false, "files": [] },
      { "version": "go1", "stable": true, "files": [] },
      { "version": "weird-tag", "stable": false, "files": [] }
    ]"#;

    fn parsed() -> Vec<GoRelease> {
        serde_json::from_str(INDEX).expect("index fixture must parse")
    }

    #[test]
    fn the_default_requirement_is_a_valid_pin() {
        let requirement = GoProvider.default_requirement();
        assert!(requirement.is_floating());
        assert!(requirement.alias().is_none());
    }

    #[test]
    fn every_go_version_shape_normalises() {
        let cases = [
            ("go1", "1.0.0"),
            ("go1.20", "1.20.0"),
            ("go1.26.6", "1.26.6"),
            ("go1.27rc3", "1.27.0-rc3"),
            ("go1.19beta1", "1.19.0-beta1"),
            ("go1.9.2rc2", "1.9.2-rc2"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                parse_go_version(raw).map(|v| v.to_string()).as_deref(),
                Some(expected),
                "for {raw}"
            );
        }
    }

    #[test]
    fn release_candidates_become_prereleases() {
        // Which means an ordinary pin will never select one.
        let rc = parse_go_version("go1.27rc3").unwrap();
        assert!(rc.is_prerelease());
        assert!(!VersionReq::parse("1.27").unwrap().matches(&rc));
        assert!(!VersionReq::parse("1").unwrap().matches(&rc));
    }

    #[test]
    fn tags_that_are_not_versions_are_skipped() {
        for raw in ["weird-tag", "1.26.6", "gorilla", "go1.2.3.4", ""] {
            assert!(parse_go_version(raw).is_none(), "`{raw}` should not parse");
        }
    }

    #[test]
    fn availability_follows_the_index_files() {
        let releases = parsed();
        let macos = |r: &GoRelease| {
            r.files
                .iter()
                .any(|f| archive_matches(f, "darwin", "arm64"))
        };

        assert!(macos(&releases[1]), "1.26.6 has a darwin arm64 archive");
        assert!(macos(&releases[2]), "1.20 has one");
        assert!(!macos(&releases[4]), "go1 lists no files");
    }

    #[test]
    fn installers_are_not_mistaken_for_archives() {
        // The 1.26.6 entry has both a .tar.gz and a .pkg for darwin/arm64.
        let release = &parsed()[1];
        let chosen: Vec<&str> = release
            .files
            .iter()
            .filter(|f| archive_matches(f, "darwin", "arm64"))
            .map(|f| f.filename.as_str())
            .collect();
        assert_eq!(chosen, ["go1.26.6.darwin-arm64.tar.gz"]);
    }

    #[test]
    fn the_filename_comes_from_the_index_not_from_the_version() {
        // `go1.20` normalises to 1.20.0, but its tarball is `go1.20...`.
        // Rebuilding the name from the version would produce a 404.
        let release = &parsed()[2];
        assert_eq!(
            parse_go_version(&release.version).unwrap().to_string(),
            "1.20.0"
        );
        assert_eq!(release.files[0].filename, "go1.20.darwin-arm64.tar.gz");
    }

    #[test]
    fn an_entry_without_a_checksum_is_not_usable() {
        // Kiln will not install what it cannot verify.
        let file = GoFile {
            filename: "go1.26.6.linux-amd64.tar.gz".into(),
            os: "linux".into(),
            arch: "amd64".into(),
            kind: "archive".into(),
            sha256: String::new(),
            size: 1,
        };
        assert!(!archive_matches(&file, "linux", "amd64"));
    }

    #[test]
    fn platform_targets_use_gos_spelling() {
        assert_eq!(
            target_for(&Platform::new(Os::MacOs, Arch::Aarch64, None)),
            Some(("darwin", "arm64"))
        );
        assert_eq!(
            target_for(&Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Gnu))),
            Some(("linux", "amd64"))
        );
        assert!(target_for(&Platform::new(Os::Windows, Arch::X86_64, None)).is_none());
    }

    #[test]
    fn one_linux_build_serves_both_libc_flavours() {
        // Go's toolchain binaries are statically linked, so unlike Node.js
        // there is no separate musl artifact and none is needed.
        let gnu = Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Gnu));
        let musl = Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Musl));
        assert_eq!(target_for(&gnu), target_for(&musl));
        assert!(GoProvider.supports(&musl));
    }

    #[test]
    fn the_layout_strips_the_go_directory() {
        let layout = GoProvider.layout();
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
    fn reads_the_go_directive_from_go_mod() {
        let project = Project::new("gomod");
        project.write(
            "go.mod",
            "module example.com/app\n\ngo 1.22.0\n\nrequire (\n)\n",
        );

        let evidence = project.detect().unwrap();
        assert_eq!(evidence.requirement().unwrap().to_string(), "1.22.0");
        assert_eq!(evidence.source(), "go.mod (go directive)");
    }

    #[test]
    fn a_two_component_directive_is_read_as_a_line_pin() {
        let project = Project::new("gomod-minor");
        project.write("go.mod", "module x\n\ngo 1.22\n");
        assert_eq!(
            project.detect().unwrap().requirement().unwrap().to_string(),
            "1.22"
        );
    }

    #[test]
    fn a_version_file_outranks_go_mod() {
        let project = Project::new("precedence");
        project
            .write("go.mod", "module x\n\ngo 1.20\n")
            .write(".go-version", "1.26.6");

        let evidence = project.detect().unwrap();
        assert_eq!(evidence.requirement().unwrap().to_string(), "1.26.6");
        assert_eq!(evidence.source(), ".go-version");
    }

    #[test]
    fn a_toolchain_line_is_not_the_go_directive() {
        // `toolchain go1.22.1` must not be mistaken for `go 1.22.1`.
        let project = Project::new("toolchain");
        project.write("go.mod", "module x\n\ntoolchain go1.22.1\n");
        let evidence = project.detect().unwrap();
        assert!(
            evidence.requirement().is_none(),
            "should not read a version"
        );
        assert_eq!(evidence.source(), "go.mod");
    }

    #[test]
    fn a_go_mod_without_a_directive_still_identifies_the_project() {
        let project = Project::new("bare");
        project.write("go.mod", "module example.com/app\n");

        let evidence = project.detect().unwrap();
        assert!(evidence.requirement().is_none());
        assert_eq!(evidence.source(), "go.mod");
    }

    #[test]
    fn every_marker_identifies_a_go_project() {
        for marker in MARKERS {
            let project = Project::new("marker");
            project.write(marker, "");
            assert!(
                project.detect().is_some(),
                "`{marker}` should identify a Go project"
            );
        }
    }

    #[test]
    fn go_directive_parsing_is_precise() {
        assert_eq!(go_directive("go 1.22.0\n").as_deref(), Some("1.22.0"));
        assert_eq!(
            go_directive("module x\n\ngo 1.22\n").as_deref(),
            Some("1.22")
        );
        assert_eq!(go_directive("  go   1.22  \n").as_deref(), Some("1.22"));
        assert!(go_directive("toolchain go1.22.1\n").is_none());
        assert!(go_directive("module example.com/go\n").is_none());
        assert!(go_directive("").is_none());
    }
}
