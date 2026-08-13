//! The runtime provider interface.

use std::path::Path;

use kiln_core::error::{Error, Result};
use kiln_core::{
    ArtifactFormat, Digest, Platform, RuntimeLayout, Version, VersionAlias, VersionReq,
};
use kiln_net::Http;

/// What role a provider plays in an environment.
///
/// The distinction is documentation for humans — it decides whether `kiln init`
/// writes an entry under `[runtime]` or `[tools]` — and never a resolution rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    /// A language runtime, such as Node.js or Python.
    Language,
    /// A tool that runs on top of a language runtime, such as pnpm.
    PackageManager,
}

/// What a repository revealed about a runtime it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// The project uses this runtime and says which version it wants.
    Pinned {
        /// The requirement, translated into Kiln's grammar.
        requirement: VersionReq,
        /// Where it was found, phrased for display: `.nvmrc`, `package.json (engines.node)`.
        source: String,
    },
    /// The project clearly uses this runtime but never says which version.
    Present {
        /// The file that gave it away.
        source: String,
    },
}

impl Evidence {
    /// The requirement, if the project named one.
    pub fn requirement(&self) -> Option<&VersionReq> {
        match self {
            Evidence::Pinned { requirement, .. } => Some(requirement),
            Evidence::Present { .. } => None,
        }
    }

    /// Where the evidence came from.
    pub fn source(&self) -> &str {
        match self {
            Evidence::Pinned { source, .. } | Evidence::Present { source } => source,
        }
    }
}

/// A tool a project expects, discovered while inspecting a runtime's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEvidence {
    /// The tool's name, e.g. `pnpm`.
    pub name: String,
    /// The version the project pins, if it pins one.
    pub requirement: Option<VersionReq>,
    /// Where it was found.
    pub source: String,
}

/// One published release of a runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The version published.
    pub version: Version,
    /// The long-term-support line's name, when the vendor designates one.
    pub lts: Option<String>,
    /// Whether the vendor publishes a build for the platform being resolved.
    pub available: bool,
}

/// Everything needed to fetch and verify one artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSpec {
    /// Where the artifact is published. Advisory: the digest is what is trusted.
    pub url: String,
    /// The digest the bytes must have.
    pub digest: Digest,
    /// How the artifact is packed.
    pub format: ArtifactFormat,
    /// Size in bytes, when the publisher states one.
    pub size: Option<u64>,
}

/// What a provider needs in order to reach the outside world.
///
/// Passing this rather than letting providers construct their own client is what
/// keeps `--offline` honest: there is no other way for a provider to make a
/// request.
pub struct ProviderContext<'a> {
    /// The HTTP client.
    pub http: &'a Http,
    /// The platform being resolved for, which is not always the host — a
    /// lockfile can be filled in for several platforms at once.
    pub platform: &'a Platform,
}

/// Everything Kiln knows about one runtime.
///
/// Implementations are stateless and cheap to construct; all of them live for
/// the whole process inside a [`crate::Registry`].
pub trait RuntimeProvider: Send + Sync {
    /// Stable identifier, matching the key used in `kiln.toml` — `node`, `python`.
    fn id(&self) -> &'static str;

    /// Human-facing name, e.g. `Node.js`.
    fn display_name(&self) -> &'static str;

    /// Whether this is a language runtime or a tool.
    fn kind(&self) -> RuntimeKind;

    /// The requirement `kiln init` proposes when a project uses this runtime but
    /// does not say which version.
    ///
    /// Always a *pin* rather than an exact version or `latest`: Kiln has no way
    /// to know today's patch release offline, and proposing `latest` would write
    /// a manifest whose meaning changes over time. The resolver turns the pin
    /// into an exact version and records it in `kiln.lock`.
    fn default_requirement(&self) -> VersionReq;

    /// Whether prebuilt artifacts exist for this platform.
    fn supports(&self, platform: &Platform) -> bool;

    /// Why this platform is unsupported, when [`Self::supports`] says no.
    ///
    /// "Node.js publishes no musl builds" is actionable; "unsupported" is not.
    fn unsupported_reason(&self, platform: &Platform) -> String {
        format!(
            "{} publishes no prebuilt artifacts for {platform}",
            self.display_name()
        )
    }

    /// Inspect a project directory for signs this runtime is used.
    ///
    /// Detection is lenient by design: an `engines.node` field Kiln cannot
    /// translate is treated as "uses Node, version unknown" rather than as an
    /// error. The manifest that comes out of `kiln init` is a proposal a human
    /// approves, so a missed version costs a keystroke, while a refusal to
    /// proceed costs the whole command.
    fn detect(&self, project_root: &Path) -> Option<Evidence>;

    /// Tools this runtime's project files mention.
    fn detect_tools(&self, _project_root: &Path) -> Vec<ToolEvidence> {
        Vec::new()
    }

    /// Where a user can read about this runtime.
    fn homepage(&self) -> &'static str;

    // -----------------------------------------------------------------------
    // Network-backed operations
    // -----------------------------------------------------------------------

    /// Every release the vendor publishes, newest first is not required —
    /// [`Self::resolve`] sorts.
    ///
    /// This is the expensive call: it fetches and parses a release index. An
    /// exact requirement never reaches it.
    fn releases(&self, ctx: &ProviderContext<'_>) -> Result<Vec<Release>>;

    /// Where to get one version, and what it must hash to.
    fn artifact(&self, version: &Version, ctx: &ProviderContext<'_>) -> Result<ArtifactSpec>;

    /// Where the executables sit inside the unpacked archive.
    fn layout(&self) -> RuntimeLayout;

    /// Choose the version that satisfies `requirement`.
    ///
    /// An exact requirement short-circuits: Kiln can build its artifact URL
    /// directly, so pinning a version means one small request for a checksum
    /// rather than a quarter-megabyte release index. That is not just a speed
    /// trick — it means a fully pinned project depends on less upstream
    /// infrastructure staying up.
    fn resolve(&self, requirement: &VersionReq, ctx: &ProviderContext<'_>) -> Result<Version> {
        if let VersionReq::Exact(version) = requirement {
            return Ok(version.clone());
        }

        let releases = self.releases(ctx)?;
        let usable: Vec<&Release> = releases.iter().filter(|r| r.available).collect();

        let chosen = match requirement.alias() {
            Some(VersionAlias::Latest) => usable
                .iter()
                .filter(|r| !r.version.is_prerelease())
                .max_by(|a, b| a.version.cmp(&b.version)),
            Some(VersionAlias::Lts) => {
                if !usable.iter().any(|r| r.lts.is_some()) {
                    return Err(Error::not_found(format!(
                        "{} does not have long-term-support releases",
                        self.display_name()
                    ))
                    .because(format!(
                        "`lts` has no meaning for {}, so Kiln cannot resolve it.",
                        self.display_name()
                    ))
                    .hint("pin a release line instead, for example a major or minor version")
                    .hint("or use `latest` for the newest stable release"));
                }
                usable
                    .iter()
                    .filter(|r| r.lts.is_some() && !r.version.is_prerelease())
                    .max_by(|a, b| a.version.cmp(&b.version))
            }
            None => usable
                .iter()
                .filter(|r| requirement.matches(&r.version))
                .max_by(|a, b| a.version.cmp(&b.version)),
        };

        match chosen {
            Some(release) => Ok(release.version.clone()),
            None => Err(self.no_match(requirement, &releases, ctx.platform)),
        }
    }

    /// Explain that nothing satisfies the requirement, and say what would.
    fn no_match(
        &self,
        requirement: &VersionReq,
        releases: &[Release],
        platform: &Platform,
    ) -> Error {
        let name = self.display_name();

        // A version that exists but not for this platform is a different
        // problem from one that does not exist at all.
        let exists_elsewhere = releases
            .iter()
            .any(|r| !r.available && requirement.matches(&r.version));
        if exists_elsewhere {
            return Error::not_found(format!(
                "No {name} {requirement} is available for {platform}"
            ))
            .because(format!(
                "{name} {requirement} exists, but the vendor publishes no build for this platform."
            ))
            .hint("choose a version with a build for your platform")
            .command("kiln doctor");
        }

        let mut newest: Vec<String> = releases
            .iter()
            .filter(|r| r.available && !r.version.is_prerelease())
            .map(|r| r.version.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .rev()
            .take(5)
            .map(|v| v.to_string())
            .collect();
        newest.reverse();

        let error = Error::not_found(format!("No {name} release matches `{requirement}`")).because(
            format!("Kiln checked every {name} release published for {platform}."),
        );

        if newest.is_empty() {
            error.hint("check the requirement in kiln.toml")
        } else {
            error.expected(format!(
                "the most recent releases are:\n{}",
                newest
                    .iter()
                    .map(|v| format!("  {v}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
    }
}
