//! Node.js.
//!
//! Node publishes a machine-readable release index at `/dist/index.json` and a
//! `SHASUMS256.txt` beside every release, which is everything Kiln needs: a list
//! of versions, which platforms each was built for, and an authenticated digest
//! for each artifact.

use std::path::Path;

use kiln_core::error::{Error, Result};
use kiln_core::{Arch, ArtifactFormat, Libc, Os, Platform, RuntimeLayout, Version, VersionReq};
use serde::Deserialize;

use crate::checksums;
use crate::detect::{read_capped, read_version_file, translate_requirement};
use crate::provider::{
    ArtifactSpec, Evidence, ProviderContext, Release, RuntimeKind, RuntimeProvider, ToolEvidence,
};

/// Where Node publishes releases.
const DIST_BASE: &str = "https://nodejs.org/dist";

/// The major release `kiln init` proposes when a project does not say.
///
/// A major pin, never an exact version: Kiln cannot know today's patch release
/// without the network, and the resolver will pin one into `kiln.lock` anyway.
/// Bump this when a new line becomes the active LTS.
const DEFAULT_REQUIREMENT: &str = "22";

/// Package managers worth noticing, and the lockfile that gives each away.
///
/// `npm` is absent on purpose: it ships inside Node.js, so pinning it separately
/// would describe an environment Kiln does not actually control.
const LOCKFILES: &[(&str, &str)] = &[
    ("pnpm", "pnpm-lock.yaml"),
    ("yarn", "yarn.lock"),
    ("bun", "bun.lockb"),
];

/// The Node.js runtime provider.
#[derive(Debug, Default, Clone, Copy)]
pub struct NodeProvider;

/// How a platform appears in Node's release metadata.
///
/// The two spellings differ and both are needed: the index says `osx-arm64-tar`,
/// while the file it refers to is called `node-v22.14.0-darwin-arm64.tar.gz`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeTarget {
    /// The key that appears in an index entry's `files` array.
    index_key: &'static str,
    /// The `<os>-<arch>` fragment of the artifact filename.
    file_slug: &'static str,
}

impl NodeTarget {
    fn for_platform(platform: &Platform) -> Option<Self> {
        match (platform.os, platform.arch, platform.libc) {
            (Os::MacOs, Arch::Aarch64, _) => Some(NodeTarget {
                index_key: "osx-arm64-tar",
                file_slug: "darwin-arm64",
            }),
            (Os::MacOs, Arch::X86_64, _) => Some(NodeTarget {
                index_key: "osx-x64-tar",
                file_slug: "darwin-x64",
            }),
            // Official Linux builds are glibc-only.
            (Os::Linux, Arch::X86_64, Some(Libc::Gnu) | None) => Some(NodeTarget {
                index_key: "linux-x64",
                file_slug: "linux-x64",
            }),
            (Os::Linux, Arch::Aarch64, Some(Libc::Gnu) | None) => Some(NodeTarget {
                index_key: "linux-arm64",
                file_slug: "linux-arm64",
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PackageJson {
    #[serde(default)]
    engines: Engines,
    #[serde(default, rename = "packageManager")]
    package_manager: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Engines {
    #[serde(default)]
    node: Option<String>,
}

/// One entry of `https://nodejs.org/dist/index.json`.
#[derive(Debug, Deserialize)]
struct IndexEntry {
    version: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    lts: LtsField,
}

/// Node writes `"lts": false` for ordinary releases and `"lts": "Jod"` for
/// long-term-support ones — one field with two types.
///
/// `Flag` has to carry the boolean rather than being a unit variant: an untagged
/// unit variant only matches JSON `null`, so a unit variant here would make
/// every ordinary release fail to parse, and with it the entire index.
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum LtsField {
    Name(String),
    // The boolean is never read — only its *shape* matters, so that `false`
    // deserialises instead of failing the whole document.
    Flag(#[allow(dead_code)] bool),
    #[default]
    Absent,
}

impl LtsField {
    fn name(&self) -> Option<String> {
        match self {
            LtsField::Name(name) => Some(name.clone()),
            LtsField::Flag(_) | LtsField::Absent => None,
        }
    }
}

impl RuntimeProvider for NodeProvider {
    fn id(&self) -> &'static str {
        "node"
    }

    fn display_name(&self) -> &'static str {
        "Node.js"
    }

    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Language
    }

    fn default_requirement(&self) -> VersionReq {
        VersionReq::parse(DEFAULT_REQUIREMENT)
            .expect("the built-in Node.js requirement is valid; asserted by unit test")
    }

    fn supports(&self, platform: &Platform) -> bool {
        NodeTarget::for_platform(platform).is_some()
    }

    fn unsupported_reason(&self, platform: &Platform) -> String {
        if platform.libc == Some(Libc::Musl) {
            return "Node.js publishes no official musl builds; the nodejs.org \
                    binaries are linked against glibc"
                .to_string();
        }
        format!("Node.js publishes no prebuilt artifacts for {platform}")
    }

    fn layout(&self) -> RuntimeLayout {
        // node-v22.14.0-darwin-arm64/{bin,lib,include,share}
        RuntimeLayout::UNIX_PREFIX
    }

    fn releases(&self, ctx: &ProviderContext<'_>) -> Result<Vec<Release>> {
        let target = self.target(ctx.platform)?;
        let url = format!("{DIST_BASE}/index.json");
        let body = ctx.http.get_text(&url, "the Node.js release index")?;

        let entries: Vec<IndexEntry> = serde_json::from_str(&body).map_err(|e| {
            Error::new(
                kiln_core::ErrorKind::Network,
                "Could not read the Node.js release index",
            )
            .because(format!("{url} did not contain the expected JSON: {e}"))
            .hint("if this persists, the upstream format may have changed; please report it")
            .with_source(e)
        })?;

        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                // Entries Kiln cannot parse are skipped rather than fatal: one
                // malformed historical release must not break every install.
                let version = Version::parse(&entry.version).ok()?;
                Some(Release {
                    version,
                    lts: entry.lts.name(),
                    available: entry.files.iter().any(|f| f == target.index_key),
                })
            })
            .collect())
    }

    fn artifact(&self, version: &Version, ctx: &ProviderContext<'_>) -> Result<ArtifactSpec> {
        let target = self.target(ctx.platform)?;
        let filename = format!("node-v{version}-{}.tar.gz", target.file_slug);
        let url = format!("{DIST_BASE}/v{version}/{filename}");

        let what = format!("the checksums for Node.js {version}");
        let sums_url = format!("{DIST_BASE}/v{version}/SHASUMS256.txt");
        let sums = ctx.http.get_text(&sums_url, &what)?;

        let digest = checksums::find(&sums, &filename).ok_or_else(|| {
            Error::not_found(format!(
                "Node.js {version} does not publish a build for {}",
                ctx.platform
            ))
            .because(format!("{filename} is not listed in {sums_url}"))
            .hint("choose a version that was built for your platform")
            .command("kiln doctor")
        })?;

        Ok(ArtifactSpec {
            url,
            digest,
            format: ArtifactFormat::TarGz,
            // nodejs.org does not publish sizes in the index; the response's
            // Content-Length supplies one at download time.
            size: None,
        })
    }

    fn detect(&self, project_root: &Path) -> Option<Evidence> {
        let package_json = read_capped(&project_root.join("package.json"));
        let parsed = package_json
            .as_deref()
            .and_then(|text| serde_json::from_str::<PackageJson>(text).ok());

        // Most specific source wins: an explicit version file beats a range in
        // package.json, because a developer who wrote `.nvmrc` meant it.
        for (file, label) in [(".nvmrc", ".nvmrc"), (".node-version", ".node-version")] {
            if let Some(raw) = read_version_file(&project_root.join(file))
                && let Some(requirement) = translate_requirement(&raw)
            {
                return Some(Evidence::Pinned {
                    requirement,
                    source: label.to_string(),
                });
            }
        }

        if let Some(engines_node) = parsed.as_ref().and_then(|p| p.engines.node.as_deref())
            && let Some(requirement) = translate_requirement(engines_node)
        {
            return Some(Evidence::Pinned {
                requirement,
                source: "package.json (engines.node)".to_string(),
            });
        }

        // No usable version anywhere, but the project is clearly a Node project.
        let marker = [
            "package.json",
            ".nvmrc",
            ".node-version",
            "pnpm-lock.yaml",
            "yarn.lock",
            "package-lock.json",
        ]
        .into_iter()
        .find(|file| project_root.join(file).exists())?;

        Some(Evidence::Present {
            source: marker.to_string(),
        })
    }

    fn detect_tools(&self, project_root: &Path) -> Vec<ToolEvidence> {
        let mut tools = Vec::new();

        // `packageManager: "pnpm@10.12.1"` is the only place a JavaScript
        // project states its package manager version exactly.
        if let Some(text) = read_capped(&project_root.join("package.json"))
            && let Ok(parsed) = serde_json::from_str::<PackageJson>(&text)
            && let Some(field) = parsed.package_manager.as_deref()
            && let Some((name, version)) = split_package_manager(field)
        {
            tools.push(ToolEvidence {
                name,
                requirement: translate_requirement(version),
                source: "package.json (packageManager)".to_string(),
            });
        }

        for (tool, lockfile) in LOCKFILES {
            if tools.iter().any(|t| t.name == *tool) {
                continue;
            }
            if project_root.join(lockfile).exists() {
                tools.push(ToolEvidence {
                    name: (*tool).to_string(),
                    requirement: None,
                    source: (*lockfile).to_string(),
                });
            }
        }

        tools
    }

    fn homepage(&self) -> &'static str {
        "https://nodejs.org"
    }
}

impl NodeProvider {
    fn target(&self, platform: &Platform) -> Result<NodeTarget> {
        NodeTarget::for_platform(platform).ok_or_else(|| {
            Error::unsupported(format!("Node.js is not available for {platform}"))
                .because(self.unsupported_reason(platform))
                .hint("see https://nodejs.org/en/download for what upstream publishes")
        })
    }
}

/// Split `pnpm@10.12.1+sha512.abc` into its name and version.
///
/// The integrity suffix Corepack appends is dropped; it identifies the download,
/// not the version.
fn split_package_manager(field: &str) -> Option<(String, &str)> {
    let field = field.trim();
    let (name, rest) = field.split_once('@')?;
    if name.is_empty() {
        return None;
    }
    let version = rest.split('+').next().unwrap_or(rest);
    if version.is_empty() {
        return None;
    }
    Some((name.to_ascii_lowercase(), version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::{Digest, HashAlgorithm};

    struct Project(std::path::PathBuf);

    impl Project {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("kiln-node-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Project(path)
        }

        fn write(&self, name: &str, contents: &str) -> &Self {
            std::fs::write(self.0.join(name), contents).unwrap();
            self
        }

        fn detect(&self) -> Option<Evidence> {
            NodeProvider.detect(&self.0)
        }

        fn tools(&self) -> Vec<ToolEvidence> {
            NodeProvider.detect_tools(&self.0)
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A slice of a real `index.json`, trimmed to the fields Kiln reads.
    const INDEX: &str = r#"[
      { "version": "v24.0.0", "files": ["osx-arm64-tar", "linux-x64"], "lts": false },
      { "version": "v22.14.0", "files": ["osx-arm64-tar", "osx-x64-tar", "linux-x64", "linux-arm64"], "lts": "Jod" },
      { "version": "v22.13.0", "files": ["osx-arm64-tar", "linux-x64"], "lts": "Jod" },
      { "version": "v20.18.0", "files": ["linux-x64"], "lts": "Iron" },
      { "version": "v23.0.0-rc.1", "files": ["osx-arm64-tar"], "lts": false },
      { "version": "not-a-version", "files": ["osx-arm64-tar"], "lts": false }
    ]"#;

    fn parse_index(platform: &Platform) -> Vec<Release> {
        let target = NodeTarget::for_platform(platform).unwrap();
        serde_json::from_str::<Vec<IndexEntry>>(INDEX)
            .unwrap()
            .into_iter()
            .filter_map(|entry| {
                let version = Version::parse(&entry.version).ok()?;
                Some(Release {
                    version,
                    lts: entry.lts.name(),
                    available: entry.files.iter().any(|f| f == target.index_key),
                })
            })
            .collect()
    }

    #[test]
    fn the_default_requirement_is_a_valid_pin() {
        let requirement = NodeProvider.default_requirement();
        assert!(
            requirement.is_floating(),
            "a default must not be an exact pin"
        );
        assert!(
            requirement.alias().is_none(),
            "a default must not float to `latest`"
        );
    }

    #[test]
    fn platform_targets_use_both_upstream_spellings() {
        // The index says `osx-arm64-tar`; the file is called `darwin-arm64`.
        let macos =
            NodeTarget::for_platform(&Platform::new(Os::MacOs, Arch::Aarch64, None)).unwrap();
        assert_eq!(macos.index_key, "osx-arm64-tar");
        assert_eq!(macos.file_slug, "darwin-arm64");

        let linux =
            NodeTarget::for_platform(&Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Gnu)))
                .unwrap();
        assert_eq!(linux.index_key, "linux-x64");
        assert_eq!(linux.file_slug, "linux-x64");
    }

    #[test]
    fn musl_is_refused_with_the_actual_reason() {
        let musl = Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Musl));
        assert!(!NodeProvider.supports(&musl));

        let reason = NodeProvider.unsupported_reason(&musl);
        assert!(reason.contains("musl"), "{reason}");
        assert!(reason.contains("glibc"), "{reason}");
    }

    #[test]
    fn supports_the_platforms_node_publishes_for() {
        assert!(NodeProvider.supports(&Platform::new(Os::MacOs, Arch::Aarch64, None)));
        assert!(NodeProvider.supports(&Platform::new(Os::MacOs, Arch::X86_64, None)));
        assert!(NodeProvider.supports(&Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Gnu))));
        assert!(NodeProvider.supports(&Platform::new(Os::Linux, Arch::Aarch64, Some(Libc::Gnu))));
        assert!(!NodeProvider.supports(&Platform::new(Os::Windows, Arch::X86_64, None)));
    }

    #[test]
    fn both_spellings_of_the_lts_field_parse() {
        // `false` and a codename appear in the same array upstream. Getting this
        // wrong does not degrade gracefully — serde fails the whole document.
        let entries: Vec<IndexEntry> = serde_json::from_str(INDEX).expect("index must parse");
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[1].lts.name().as_deref(), Some("Jod"));
        assert!(entries[0].lts.name().is_none());

        // A missing field is tolerated too.
        let bare: IndexEntry =
            serde_json::from_str(r#"{ "version": "v1.0.0", "files": [] }"#).unwrap();
        assert!(bare.lts.name().is_none());
    }

    #[test]
    fn the_index_marks_availability_per_platform() {
        let macos = parse_index(&Platform::new(Os::MacOs, Arch::Aarch64, None));

        // A malformed version is skipped, not fatal.
        assert_eq!(macos.len(), 5);

        let find = |v: &str| {
            macos
                .iter()
                .find(|r| r.version == Version::parse(v).unwrap())
                .unwrap()
        };
        assert!(find("22.14.0").available);
        assert_eq!(find("22.14.0").lts.as_deref(), Some("Jod"));
        // 20.18.0 is linux-only in this fixture.
        assert!(!find("20.18.0").available);
        assert!(find("24.0.0").lts.is_none());
    }

    #[test]
    fn arm64_linux_sees_only_what_was_built_for_it() {
        let linux = parse_index(&Platform::new(Os::Linux, Arch::Aarch64, Some(Libc::Gnu)));
        let available: Vec<String> = linux
            .iter()
            .filter(|r| r.available)
            .map(|r| r.version.to_string())
            .collect();
        assert_eq!(available, ["22.14.0"]);
    }

    #[test]
    fn checksums_are_matched_by_exact_filename() {
        // Real lines from a SHASUMS256.txt, plus a decoy that shares a prefix.
        let sums = "\
e9404633bc02a5162c5c573b1e2490f5fb44648345d64a958b17e325729a5e42  node-v22.14.0-darwin-arm64.tar.gz
4e845cb71b4e897289312743b2e31c405a8a48720655404d82a4dce23fc43527  node-v22.14.0-darwin-arm64.tar.xz
9d942932535988091034dc94cc5f42b6dc8784d6366df3a36c4c9ccb3996f0c2  node-v22.14.0-linux-x64.tar.gz
";
        let found = checksums::find(sums, "node-v22.14.0-darwin-arm64.tar.gz").unwrap();
        assert_eq!(
            found,
            Digest::parse(
                "sha256:e9404633bc02a5162c5c573b1e2490f5fb44648345d64a958b17e325729a5e42"
            )
            .unwrap()
        );
        assert_eq!(found.algorithm(), HashAlgorithm::Sha256);

        // The `.tar.xz` line must not satisfy a `.tar.gz` request.
        assert_ne!(
            checksums::find(sums, "node-v22.14.0-darwin-arm64.tar.xz").unwrap(),
            found
        );
        assert!(checksums::find(sums, "node-v99.0.0-darwin-arm64.tar.gz").is_none());
    }

    #[test]
    fn the_layout_strips_the_release_directory() {
        // node-v22.14.0-darwin-arm64/bin/node → bin/node
        let layout = NodeProvider.layout();
        assert_eq!(layout.strip_components, 1);
        assert_eq!(layout.bin_dirs, ["bin"]);
    }

    // --- detection -------------------------------------------------------

    #[test]
    fn detects_nothing_in_an_unrelated_directory() {
        let project = Project::new("empty");
        project.write("Cargo.toml", "[package]\nname = \"x\"\n");
        assert!(project.detect().is_none());
        assert!(project.tools().is_empty());
    }

    #[test]
    fn reads_the_engines_field() {
        let project = Project::new("engines");
        project.write(
            "package.json",
            r#"{ "name": "app", "engines": { "node": ">=22, <23" } }"#,
        );

        let evidence = project.detect().expect("Node detected");
        assert_eq!(evidence.requirement().unwrap().to_string(), ">=22, <23");
        assert_eq!(evidence.source(), "package.json (engines.node)");
    }

    #[test]
    fn a_version_file_outranks_the_engines_field() {
        let project = Project::new("precedence");
        project
            .write("package.json", r#"{ "engines": { "node": "20" } }"#)
            .write(".nvmrc", "22.14.0\n");

        let evidence = project.detect().unwrap();
        assert_eq!(evidence.requirement().unwrap().to_string(), "22.14.0");
        assert_eq!(evidence.source(), ".nvmrc");
    }

    #[test]
    fn node_version_files_are_read_too() {
        let project = Project::new("node-version");
        project.write(".node-version", "v20.11.1");
        assert_eq!(
            project.detect().unwrap().requirement().unwrap().to_string(),
            "20.11.1"
        );
    }

    #[test]
    fn an_untranslatable_version_still_identifies_the_project() {
        let project = Project::new("lts-alias");
        project
            .write("package.json", "{}")
            .write(".nvmrc", "lts/iron");

        let evidence = project.detect().unwrap();
        assert!(evidence.requirement().is_none());
        assert!(matches!(evidence, Evidence::Present { .. }));
    }

    #[test]
    fn malformed_package_json_does_not_fail_detection() {
        let project = Project::new("broken");
        project.write("package.json", "{ this is not json");

        let evidence = project.detect().expect("still recognisably a Node project");
        assert!(evidence.requirement().is_none());
        assert_eq!(evidence.source(), "package.json");
        assert!(project.tools().is_empty());
    }

    #[test]
    fn a_lockfile_alone_identifies_the_project() {
        let project = Project::new("lockfile-only");
        project.write("pnpm-lock.yaml", "lockfileVersion: 9\n");
        assert_eq!(project.detect().unwrap().source(), "pnpm-lock.yaml");
    }

    #[test]
    fn the_package_manager_field_pins_a_tool() {
        let project = Project::new("corepack");
        project.write(
            "package.json",
            r#"{ "packageManager": "pnpm@10.12.1+sha512.abc123" }"#,
        );

        let tools = project.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "pnpm");
        assert_eq!(
            tools[0].requirement.as_ref().unwrap().to_string(),
            "10.12.1"
        );
        assert_eq!(tools[0].source, "package.json (packageManager)");
    }

    #[test]
    fn lockfiles_name_a_tool_without_a_version() {
        let project = Project::new("lockfiles");
        project
            .write("package.json", "{}")
            .write("yarn.lock", "")
            .write("bun.lockb", "");

        let tools = project.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["yarn", "bun"]);
        assert!(tools.iter().all(|t| t.requirement.is_none()));
    }

    #[test]
    fn the_package_manager_field_wins_over_the_lockfile() {
        let project = Project::new("both");
        project
            .write("package.json", r#"{ "packageManager": "pnpm@10.12.1" }"#)
            .write("pnpm-lock.yaml", "");

        let tools = project.tools();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].requirement.is_some());
    }

    #[test]
    fn npm_is_never_proposed_as_a_separate_tool() {
        let project = Project::new("npm");
        project
            .write("package.json", "{}")
            .write("package-lock.json", "{}");
        assert!(project.tools().is_empty());
    }

    #[test]
    fn package_manager_fields_that_make_no_sense_are_ignored() {
        for field in ["", "pnpm", "@10.0.0", "pnpm@"] {
            assert!(
                split_package_manager(field).is_none(),
                "`{field}` should be ignored"
            );
        }
    }
}
