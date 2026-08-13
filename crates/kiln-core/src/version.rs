//! Versions and version requirements.
//!
//! Kiln does not reuse Cargo's requirement grammar, because Cargo's defaults are
//! wrong for this problem: `node = "22.14.0"` in a Cargo manifest means
//! "22.14.0 or any compatible later release", but in `kiln.toml` it must mean
//! *exactly* 22.14.0. Reproducibility beats convenience, so the mapping from
//! text to meaning is explicit and owned here.
//!
//! | Written        | Meaning                                  |
//! |----------------|------------------------------------------|
//! | `22.14.0`      | exactly 22.14.0                          |
//! | `22`           | any 22.x.x                               |
//! | `22.14`        | any 22.14.x                              |
//! | `^22.14`       | `>=22.14.0`, `<23.0.0`                   |
//! | `~22.14`       | `>=22.14.0`, `<22.15.0`                  |
//! | `>=22, <23`    | every comparator must hold               |
//! | `lts`, `latest`| resolved by the runtime provider         |
//!
//! Pre-release versions are never selected unless the requirement itself names a
//! pre-release with the same `major.minor.patch`. Nobody wants `kiln install` to
//! quietly hand them a release candidate.
//!
//! Comparators zero-fill omitted components, so `>=22` means `>=22.0.0` and
//! `<=22` means `<=22.0.0`. Prefer the unambiguous `>=22, <23` form for ranges.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};

use crate::error::{Error, Result};

/// The accepted spellings, shown whenever a requirement fails to parse.
pub const EXPECTED_REQUIREMENT_FORMS: &str = "\
an exact version        22.14.0
a major or minor pin    22          22.14
a caret range           ^22.14
a tilde range           ~22.14
a comparator range      >=22, <23
a supported alias       lts         latest";

// ---------------------------------------------------------------------------
// Pre-release identifiers
// ---------------------------------------------------------------------------

/// The pre-release portion of a version, e.g. the `rc.1` in `23.0.0-rc.1`.
///
/// Ordering follows the Semantic Versioning specification: dot-separated
/// identifiers compare left to right, numeric identifiers compare numerically
/// and sort below alphanumeric ones, and a shorter identifier list sorts below a
/// longer one when all preceding identifiers are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Prerelease(String);

impl Prerelease {
    /// Validate and wrap a pre-release string (without the leading `-`).
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() {
            return Err(invalid_version(raw, "the pre-release identifier is empty"));
        }
        for part in raw.split('.') {
            if part.is_empty() {
                return Err(invalid_version(
                    raw,
                    "pre-release identifiers must not be empty",
                ));
            }
            if !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(invalid_version(
                    raw,
                    "pre-release identifiers may only contain letters, digits and hyphens",
                ));
            }
        }
        Ok(Prerelease(raw.to_string()))
    }

    /// The raw identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Prerelease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Ord for Prerelease {
    fn cmp(&self, other: &Self) -> Ordering {
        let mut left = self.0.split('.');
        let mut right = other.0.split('.');
        loop {
            match (left.next(), right.next()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(a), Some(b)) => {
                    let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(a), Ok(b)) => a.cmp(&b),
                        // Numeric identifiers always have lower precedence.
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => a.cmp(b),
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
            }
        }
    }
}

impl PartialOrd for Prerelease {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// A concrete, fully specified version such as `22.14.0`.
///
/// Build metadata is preserved for display but ignored for equality and
/// ordering, as the Semantic Versioning specification requires.
#[derive(Debug, Clone, Eq)]
pub struct Version {
    /// Major component.
    pub major: u64,
    /// Minor component.
    pub minor: u64,
    /// Patch component.
    pub patch: u64,
    /// Pre-release identifiers, if any.
    pub pre: Option<Prerelease>,
    /// Build metadata, if any. Ignored when comparing.
    pub build: Option<String>,
}

impl Version {
    /// A release version with no pre-release or build metadata.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version {
            major,
            minor,
            patch,
            pre: None,
            build: None,
        }
    }

    /// Parse a fully specified version. A leading `v` is accepted and dropped,
    /// because upstream release feeds are inconsistent about it.
    pub fn parse(raw: &str) -> Result<Self> {
        let partial = PartialVersion::parse(raw)?;
        match (partial.minor, partial.patch) {
            (Some(minor), Some(patch)) => Ok(Version {
                major: partial.major,
                minor,
                patch,
                pre: partial.pre,
                build: partial.build,
            }),
            _ => Err(invalid_version(
                raw,
                "a complete version needs all three components, such as 22.14.0",
            )),
        }
    }

    /// Whether this is a pre-release such as `23.0.0-rc.1`.
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }

    /// The `major.minor.patch` triple, ignoring pre-release and build metadata.
    pub fn core(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.core() == other.core() && self.pre == other.pre
    }
}

impl std::hash::Hash for Version {
    /// Hashes exactly the fields [`PartialEq`] compares.
    ///
    /// Build metadata is excluded, because `1.0.0+a == 1.0.0+b`. A derived
    /// `Hash` would include it and quietly break every hash map keyed by a
    /// version: two equal versions would land in different buckets.
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.core().hash(state);
        self.pre.hash(state);
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core().cmp(&other.core()).then_with(|| {
            match (&self.pre, &other.pre) {
                // A release outranks any pre-release of the same triple.
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        if let Some(build) = &self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Version::parse(s)
    }
}

// ---------------------------------------------------------------------------
// PartialVersion
// ---------------------------------------------------------------------------

/// A version with optional trailing components, such as `22` or `22.14`.
///
/// Trailing `x`, `X` and `*` components are accepted and treated as omitted, so
/// `22.x` and `22` mean the same thing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PartialVersion {
    /// Major component. Always present.
    pub major: u64,
    /// Minor component, if specified.
    pub minor: Option<u64>,
    /// Patch component, if specified.
    pub patch: Option<u64>,
    /// Pre-release identifiers, if any.
    pub pre: Option<Prerelease>,
    /// Build metadata, if any.
    pub build: Option<String>,
}

impl PartialVersion {
    /// Parse `major[.minor[.patch]][-pre][+build]`.
    pub fn parse(raw: &str) -> Result<Self> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(invalid_version(raw, "the version is empty"));
        }
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);

        let (text, build) = match text.split_once('+') {
            Some((head, tail)) if !tail.is_empty() => (head, Some(tail.to_string())),
            Some(_) => return Err(invalid_version(raw, "the build metadata is empty")),
            None => (text, None),
        };

        // The first `-` after the numeric core starts the pre-release. Splitting
        // on the first `-` is safe because numeric components cannot contain one.
        let (core, pre) = match text.split_once('-') {
            Some((head, tail)) => (head, Some(Prerelease::parse(tail)?)),
            None => (text, None),
        };

        let mut components: [Option<u64>; 3] = [None, None, None];
        let mut wildcard_seen = false;
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() > 3 {
            return Err(invalid_version(
                raw,
                "a version has at most three components",
            ));
        }
        for (index, part) in parts.iter().enumerate() {
            if matches!(*part, "x" | "X" | "*") {
                wildcard_seen = true;
                continue;
            }
            if wildcard_seen {
                return Err(invalid_version(
                    raw,
                    "a wildcard component must not be followed by a number",
                ));
            }
            if part.is_empty() {
                return Err(invalid_version(raw, "a version component is empty"));
            }
            if !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid_version(raw, format!("`{part}` is not a number")));
            }
            components[index] = Some(part.parse::<u64>().map_err(|_| {
                invalid_version(
                    raw,
                    format!("`{part}` is too large to be a version component"),
                )
            })?);
        }

        let Some(major) = components[0] else {
            return Err(invalid_version(
                raw,
                "the major version must be a number, not a wildcard",
            ));
        };
        if components[1].is_none() && components[2].is_some() {
            return Err(invalid_version(
                raw,
                "the minor version must be given before the patch version",
            ));
        }
        if pre.is_some() && (components[1].is_none() || components[2].is_none()) {
            return Err(invalid_version(
                raw,
                "a pre-release needs a complete version, such as 23.0.0-rc.1",
            ));
        }

        Ok(PartialVersion {
            major,
            minor: components[1],
            patch: components[2],
            pre,
            build,
        })
    }

    /// Whether all three numeric components were specified.
    pub fn is_complete(&self) -> bool {
        self.minor.is_some() && self.patch.is_some()
    }

    /// Promote to a [`Version`], zero-filling omitted components.
    pub fn to_version(&self) -> Version {
        Version {
            major: self.major,
            minor: self.minor.unwrap_or(0),
            patch: self.patch.unwrap_or(0),
            pre: self.pre.clone(),
            build: self.build.clone(),
        }
    }

    /// The smallest version this partial version can denote.
    pub fn lower_bound(&self) -> Version {
        Version {
            major: self.major,
            minor: self.minor.unwrap_or(0),
            patch: self.patch.unwrap_or(0),
            pre: self.pre.clone(),
            build: None,
        }
    }

    /// Exclusive upper bound for caret semantics.
    pub fn caret_upper(&self) -> Version {
        match (self.major, self.minor, self.patch) {
            (0, None, _) => Version::new(1, 0, 0),
            (0, Some(0), None) => Version::new(0, 1, 0),
            (0, Some(0), Some(patch)) => Version::new(0, 0, patch + 1),
            (0, Some(minor), _) => Version::new(0, minor + 1, 0),
            (major, _, _) => Version::new(major + 1, 0, 0),
        }
    }

    /// Exclusive upper bound for tilde semantics.
    pub fn tilde_upper(&self) -> Version {
        match self.minor {
            None => Version::new(self.major + 1, 0, 0),
            Some(minor) => Version::new(self.major, minor + 1, 0),
        }
    }

    /// Whether `version` starts with every component this partial version pins.
    pub fn is_prefix_of(&self, version: &Version) -> bool {
        version.major == self.major
            && self.minor.is_none_or(|m| version.minor == m)
            && self.patch.is_none_or(|p| version.patch == p)
            && (self.pre.is_none() || self.pre == version.pre)
    }
}

impl fmt::Display for PartialVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.major)?;
        if let Some(minor) = self.minor {
            write!(f, ".{minor}")?;
        }
        if let Some(patch) = self.patch {
            write!(f, ".{patch}")?;
        }
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        if let Some(build) = &self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Comparators
// ---------------------------------------------------------------------------

/// The relational operators accepted inside a comparator range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareOp {
    /// `>`
    Greater,
    /// `>=`
    GreaterEq,
    /// `<`
    Less,
    /// `<=`
    LessEq,
}

impl CompareOp {
    const fn as_str(self) -> &'static str {
        match self {
            CompareOp::Greater => ">",
            CompareOp::GreaterEq => ">=",
            CompareOp::Less => "<",
            CompareOp::LessEq => "<=",
        }
    }
}

/// One `<operator><version>` term of a comparator range.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Comparator {
    /// The relational operator.
    pub op: CompareOp,
    /// The version on the right-hand side, with omitted components zero-filled
    /// at comparison time.
    pub version: PartialVersion,
}

impl Comparator {
    fn matches(&self, candidate: &Version) -> bool {
        let bound = self.version.lower_bound();
        match self.op {
            CompareOp::Greater => *candidate > bound,
            CompareOp::GreaterEq => *candidate >= bound,
            CompareOp::Less => *candidate < bound,
            CompareOp::LessEq => *candidate <= bound,
        }
    }
}

impl fmt::Display for Comparator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.op.as_str(), self.version)
    }
}

// ---------------------------------------------------------------------------
// Aliases
// ---------------------------------------------------------------------------

/// A symbolic version that only a runtime provider can resolve.
///
/// Aliases are always written explicitly. Kiln never treats a missing or empty
/// requirement as "latest"; a floating requirement is a deliberate choice, and
/// `kiln.lock` records what it resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VersionAlias {
    /// The newest stable release the provider offers.
    Latest,
    /// The newest long-term-support release, where the provider has the concept.
    Lts,
}

impl VersionAlias {
    fn from_keyword(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "latest" => Some(VersionAlias::Latest),
            "lts" => Some(VersionAlias::Lts),
            _ => None,
        }
    }

    /// The canonical spelling of this alias.
    pub const fn as_str(self) -> &'static str {
        match self {
            VersionAlias::Latest => "latest",
            VersionAlias::Lts => "lts",
        }
    }
}

impl fmt::Display for VersionAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// VersionReq
// ---------------------------------------------------------------------------

/// A requirement written by a human in `kiln.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VersionReq {
    /// Exactly this version, e.g. `22.14.0`.
    Exact(Version),
    /// Every version sharing the given prefix, e.g. `22` or `22.14`.
    Pinned(PartialVersion),
    /// Caret range, e.g. `^22.14`.
    Caret(PartialVersion),
    /// Tilde range, e.g. `~22.14`.
    Tilde(PartialVersion),
    /// Conjunction of comparators, e.g. `>=22, <23`.
    Range(Vec<Comparator>),
    /// A provider-resolved alias, e.g. `lts`.
    Alias(VersionAlias),
}

impl VersionReq {
    /// Parse a requirement string from `kiln.toml`.
    pub fn parse(raw: &str) -> Result<Self> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(unsupported_requirement(raw)
                .because("the version requirement is empty")
                .hint("every runtime and tool must pin a version"));
        }

        if let Some(alias) = VersionAlias::from_keyword(text) {
            return Ok(VersionReq::Alias(alias));
        }

        if text == "*" {
            return Err(unsupported_requirement(raw)
                .because("`*` would let the resolved version drift between machines")
                .hint("write `latest` if you really want the newest release; `kiln.lock` will record what it resolved to"));
        }

        if text.contains(',') || text.starts_with(['>', '<']) || text.contains("!=") {
            return parse_range(raw, text);
        }

        if let Some(rest) = text.strip_prefix('^') {
            return Ok(VersionReq::Caret(parse_operand(raw, rest, "^")?));
        }
        if let Some(rest) = text.strip_prefix('~') {
            return Ok(VersionReq::Tilde(parse_operand(raw, rest, "~")?));
        }

        let bare = text.strip_prefix('=').unwrap_or(text);
        let partial = PartialVersion::parse(bare).map_err(|e| {
            unsupported_requirement(raw)
                .because(e.reason().unwrap_or("it is not a version").to_string())
        })?;

        if partial.is_complete() {
            Ok(VersionReq::Exact(partial.to_version()))
        } else {
            Ok(VersionReq::Pinned(partial))
        }
    }

    /// Whether `candidate` satisfies this requirement.
    ///
    /// Always `false` for [`VersionReq::Alias`]: aliases need the provider's
    /// release list to have any meaning. Use [`VersionReq::is_floating`] to
    /// detect that case before matching.
    pub fn matches(&self, candidate: &Version) -> bool {
        if candidate.is_prerelease() && !self.admits_prerelease_of(candidate) {
            return false;
        }
        match self {
            VersionReq::Exact(version) => version == candidate,
            VersionReq::Pinned(partial) => partial.is_prefix_of(candidate),
            VersionReq::Caret(partial) => {
                *candidate >= partial.lower_bound() && *candidate < partial.caret_upper()
            }
            VersionReq::Tilde(partial) => {
                *candidate >= partial.lower_bound() && *candidate < partial.tilde_upper()
            }
            VersionReq::Range(comparators) => comparators.iter().all(|c| c.matches(candidate)),
            VersionReq::Alias(_) => false,
        }
    }

    /// Whether resolving this requirement needs the provider's release list.
    ///
    /// Floating requirements are legal but must be recorded in `kiln.lock` for
    /// the environment to stay reproducible.
    pub fn is_floating(&self) -> bool {
        !matches!(self, VersionReq::Exact(_))
    }

    /// The alias this requirement names, if any.
    pub fn alias(&self) -> Option<VersionAlias> {
        match self {
            VersionReq::Alias(alias) => Some(*alias),
            _ => None,
        }
    }

    /// Whether this requirement admits arbitrarily new versions.
    ///
    /// `>=22` is open-ended; `22`, `^22` and `>=22, <23` are not. The
    /// distinction matters because an open-ended requirement is a statement of
    /// *compatibility* — the shape `engines.node` and `requires-python` use —
    /// rather than a pin, and resolving one gives a different answer every time
    /// a new major release ships.
    pub fn is_open_ended(&self) -> bool {
        match self {
            VersionReq::Range(comparators) => !comparators
                .iter()
                .any(|c| matches!(c.op, CompareOp::Less | CompareOp::LessEq)),
            _ => false,
        }
    }

    /// The smallest version this requirement could select.
    ///
    /// `None` for aliases, whose bounds only the provider knows.
    pub fn lower_bound(&self) -> Option<PartialVersion> {
        match self {
            VersionReq::Exact(version) => Some(PartialVersion {
                major: version.major,
                minor: Some(version.minor),
                patch: Some(version.patch),
                pre: version.pre.clone(),
                build: None,
            }),
            VersionReq::Pinned(p) | VersionReq::Caret(p) | VersionReq::Tilde(p) => Some(p.clone()),
            VersionReq::Range(comparators) => comparators
                .iter()
                .filter(|c| matches!(c.op, CompareOp::Greater | CompareOp::GreaterEq))
                .map(|c| c.version.clone())
                .max_by_key(|v| v.lower_bound()),
            VersionReq::Alias(_) => None,
        }
    }

    /// A pre-release candidate is only eligible when the requirement itself
    /// names a pre-release with the same `major.minor.patch`.
    fn admits_prerelease_of(&self, candidate: &Version) -> bool {
        let mentions = |partial: &PartialVersion| {
            partial.pre.is_some()
                && partial.major == candidate.major
                && partial.minor == Some(candidate.minor)
                && partial.patch == Some(candidate.patch)
        };
        match self {
            VersionReq::Exact(version) => {
                version.is_prerelease() && version.core() == candidate.core()
            }
            VersionReq::Pinned(p) | VersionReq::Caret(p) | VersionReq::Tilde(p) => mentions(p),
            VersionReq::Range(comparators) => comparators.iter().any(|c| mentions(&c.version)),
            VersionReq::Alias(_) => false,
        }
    }
}

fn parse_operand(raw: &str, operand: &str, sigil: &str) -> Result<PartialVersion> {
    PartialVersion::parse(operand).map_err(|e| {
        unsupported_requirement(raw).because(format!(
            "`{sigil}` must be followed by a version, but {}",
            e.reason().unwrap_or("the operand is not a version")
        ))
    })
}

fn parse_range(raw: &str, text: &str) -> Result<VersionReq> {
    let mut comparators = Vec::new();
    for term in text.split(',') {
        let term = term.trim();
        if term.is_empty() {
            return Err(unsupported_requirement(raw)
                .because("a comparator range has an empty term")
                .hint("write ranges as `>=22, <23`"));
        }
        let (op, rest) = if let Some(rest) = term.strip_prefix(">=") {
            (CompareOp::GreaterEq, rest)
        } else if let Some(rest) = term.strip_prefix("<=") {
            (CompareOp::LessEq, rest)
        } else if let Some(rest) = term.strip_prefix('>') {
            (CompareOp::Greater, rest)
        } else if let Some(rest) = term.strip_prefix('<') {
            (CompareOp::Less, rest)
        } else if term.starts_with("!=") {
            return Err(unsupported_requirement(raw)
                .because(
                    "`!=` is not supported, because exclusions make resolution order-dependent",
                )
                .hint("describe the versions you want with `>=` and `<` instead"));
        } else if term.starts_with('=') {
            return Err(unsupported_requirement(raw)
                .because("`=` is not supported inside a comparator range")
                .hint("write the version on its own to pin it exactly, e.g. `22.14.0`"));
        } else {
            return Err(unsupported_requirement(raw)
                .because(format!("`{term}` does not start with a comparator"))
                .hint("separate comparators with commas, e.g. `>=22, <23`"));
        };

        let version = PartialVersion::parse(rest.trim()).map_err(|e| {
            unsupported_requirement(raw).because(format!(
                "`{term}` is not a valid comparator: {}",
                e.reason().unwrap_or("the operand is not a version")
            ))
        })?;
        comparators.push(Comparator { op, version });
    }
    Ok(VersionReq::Range(comparators))
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionReq::Exact(version) => write!(f, "{version}"),
            VersionReq::Pinned(partial) => write!(f, "{partial}"),
            VersionReq::Caret(partial) => write!(f, "^{partial}"),
            VersionReq::Tilde(partial) => write!(f, "~{partial}"),
            VersionReq::Range(comparators) => {
                let terms: Vec<String> = comparators.iter().map(|c| c.to_string()).collect();
                f.write_str(&terms.join(", "))
            }
            VersionReq::Alias(alias) => write!(f, "{alias}"),
        }
    }
}

impl FromStr for VersionReq {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        VersionReq::parse(s)
    }
}

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

fn invalid_version(raw: &str, reason: impl Into<String>) -> Error {
    Error::config(format!("invalid version `{}`", raw.trim())).because(reason)
}

fn unsupported_requirement(raw: &str) -> Error {
    Error::config(format!("unsupported version requirement `{}`", raw.trim()))
        .expected(EXPECTED_REQUIREMENT_FORMS)
}

// ---------------------------------------------------------------------------
// serde
// ---------------------------------------------------------------------------
//
// Requirements deserialise through `FromStr`, so a bad value fails inside the
// TOML deserializer. That is deliberate: `toml` then hands us the byte span of
// the offending value for free, which is what lets Kiln draw a code frame
// pointing at the exact token.

use crate::error::to_serde_message;

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Version::parse(&raw).map_err(|e| de::Error::custom(to_serde_message(&e)))
    }
}

impl Serialize for VersionReq {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for VersionReq {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        VersionReq::parse(&raw).map_err(|e| de::Error::custom(to_serde_message(&e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("valid version")
    }

    fn req(text: &str) -> VersionReq {
        VersionReq::parse(text).unwrap_or_else(|e| panic!("`{text}` should parse: {e}"))
    }

    #[test]
    fn parses_complete_versions() {
        assert_eq!(v("22.14.0"), Version::new(22, 14, 0));
        assert_eq!(v("v22.14.0"), Version::new(22, 14, 0));
        assert_eq!(v("3.13.5").core(), (3, 13, 5));
    }

    #[test]
    fn rejects_incomplete_versions() {
        assert!(Version::parse("22").is_err());
        assert!(Version::parse("22.14").is_err());
        assert!(Version::parse("").is_err());
        assert!(Version::parse("banana").is_err());
    }

    #[test]
    fn build_metadata_is_ignored_when_comparing() {
        assert_eq!(v("1.0.0+a"), v("1.0.0+b"));
        assert_eq!(v("1.0.0+a").cmp(&v("1.0.0")), Ordering::Equal);
        assert_eq!(v("1.0.0+build").to_string(), "1.0.0+build");
    }

    #[test]
    fn equal_versions_hash_equally() {
        use std::collections::HashSet;

        // Equality ignores build metadata, so hashing must too, or a hash map
        // keyed by version silently stops finding its own entries.
        let mut seen = HashSet::new();
        seen.insert(v("1.0.0+a"));
        assert!(seen.contains(&v("1.0.0+b")));
        assert!(seen.contains(&v("1.0.0")));
        assert!(!seen.contains(&v("1.0.0-rc.1")));
    }

    #[test]
    fn prerelease_ordering_follows_semver() {
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"));
        assert!(v("1.0.0-alpha.beta") < v("1.0.0-beta"));
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0") < v("1.0.1"));
    }

    #[test]
    fn bare_complete_version_is_exact() {
        let r = req("22.14.0");
        assert!(matches!(r, VersionReq::Exact(_)));
        assert!(r.matches(&v("22.14.0")));
        assert!(!r.matches(&v("22.14.1")));
        assert!(!r.matches(&v("22.15.0")));
        assert!(!r.is_floating());
    }

    #[test]
    fn bare_major_pins_the_major() {
        let r = req("22");
        assert!(r.matches(&v("22.0.0")));
        assert!(r.matches(&v("22.14.9")));
        assert!(!r.matches(&v("23.0.0")));
        assert!(!r.matches(&v("21.9.9")));
        assert!(r.is_floating());
    }

    #[test]
    fn bare_minor_pins_major_and_minor() {
        let r = req("22.14");
        assert!(r.matches(&v("22.14.0")));
        assert!(r.matches(&v("22.14.99")));
        assert!(!r.matches(&v("22.15.0")));
    }

    #[test]
    fn wildcard_components_mean_omitted() {
        assert!(req("22.x").matches(&v("22.14.0")));
        assert!(!req("22.x").matches(&v("23.0.0")));
        assert!(req("22.14.*").matches(&v("22.14.3")));
        assert!(VersionReq::parse("22.x.3").is_err());
    }

    #[test]
    fn caret_ranges_follow_npm_and_cargo() {
        assert!(req("^22").matches(&v("22.99.0")));
        assert!(!req("^22").matches(&v("23.0.0")));
        assert!(req("^22.14").matches(&v("22.14.0")));
        assert!(!req("^22.14").matches(&v("22.13.9")));
        assert!(req("^0.2.3").matches(&v("0.2.9")));
        assert!(!req("^0.2.3").matches(&v("0.3.0")));
        assert!(req("^0.0.3").matches(&v("0.0.3")));
        assert!(!req("^0.0.3").matches(&v("0.0.4")));
        assert!(req("^0").matches(&v("0.9.0")));
        assert!(!req("^0").matches(&v("1.0.0")));
    }

    #[test]
    fn tilde_ranges_pin_the_minor() {
        assert!(req("~22.14").matches(&v("22.14.7")));
        assert!(!req("~22.14").matches(&v("22.15.0")));
        assert!(req("~22").matches(&v("22.15.0")));
        assert!(!req("~22").matches(&v("23.0.0")));
    }

    #[test]
    fn comparator_ranges_conjoin() {
        let r = req(">=22, <23");
        assert!(r.matches(&v("22.0.0")));
        assert!(r.matches(&v("22.14.0")));
        assert!(!r.matches(&v("23.0.0")));
        assert!(!r.matches(&v("21.0.0")));
        assert_eq!(r.to_string(), ">=22, <23");
    }

    #[test]
    fn comparator_ranges_tolerate_whitespace() {
        assert!(req("  >= 22 ,  < 23  ").matches(&v("22.4.1")));
    }

    #[test]
    fn prereleases_are_excluded_unless_named() {
        assert!(!req(">=22, <24").matches(&v("23.0.0-rc.1")));
        assert!(!req("^22").matches(&v("22.1.0-nightly")));
        assert!(req("22.1.0-nightly").matches(&v("22.1.0-nightly")));
        assert!(req(">=23.0.0-rc.1, <24").matches(&v("23.0.0-rc.1")));
        assert!(!req(">=23.0.0-rc.1, <24").matches(&v("23.1.0-rc.1")));
    }

    #[test]
    fn aliases_parse_but_do_not_match_locally() {
        let r = req("lts");
        assert_eq!(r.alias(), Some(VersionAlias::Lts));
        assert!(!r.matches(&v("22.14.0")));
        assert!(r.is_floating());
        assert_eq!(req("LATEST").alias(), Some(VersionAlias::Latest));
    }

    #[test]
    fn star_is_rejected_with_guidance() {
        let err = VersionReq::parse("*").unwrap_err();
        assert!(err.reason().unwrap().contains("drift"));
        assert!(err.hints().iter().any(|h| h.text().contains("latest")));
    }

    #[test]
    fn nonsense_requirements_explain_the_accepted_forms() {
        let err = VersionReq::parse("banana").unwrap_err();
        assert!(err.summary().contains("banana"));
        let expected = err.expectation().expect("expected forms");
        assert!(expected.contains("22.14.0"));
        assert!(expected.contains(">=22, <23"));
    }

    #[test]
    fn empty_requirement_is_rejected() {
        assert!(VersionReq::parse("").is_err());
        assert!(VersionReq::parse("   ").is_err());
    }

    #[test]
    fn exclusion_and_equality_operators_are_rejected_in_ranges() {
        assert!(VersionReq::parse(">=22, !=22.5.0").is_err());
        assert!(VersionReq::parse(">=22, =22.5.0").is_err());
        assert!(VersionReq::parse(">=22 <23").is_err());
    }

    #[test]
    fn requirements_round_trip_through_display() {
        for text in [
            "22.14.0",
            "22",
            "22.14",
            "^22.14",
            "~22.14",
            ">=22, <23",
            "lts",
        ] {
            assert_eq!(req(text).to_string(), text, "round-trip failed for {text}");
        }
    }

    #[test]
    fn open_ended_requirements_are_recognised() {
        assert!(req(">=22").is_open_ended());
        assert!(req(">3.11").is_open_ended());

        for bounded in ["22", "22.14.0", "^22", "~22.14", ">=22, <23", "<23", "lts"] {
            assert!(!req(bounded).is_open_ended(), "{bounded} is bounded");
        }
    }

    #[test]
    fn lower_bounds_are_the_tightest_one_stated() {
        assert_eq!(req("22.14.0").lower_bound().unwrap().to_string(), "22.14.0");
        assert_eq!(req("22").lower_bound().unwrap().to_string(), "22");
        assert_eq!(req("^22.14").lower_bound().unwrap().to_string(), "22.14");
        assert_eq!(req(">=22, <23").lower_bound().unwrap().to_string(), "22");
        // The strongest lower bound wins when several are stated.
        assert_eq!(
            req(">=20, >=22, <23").lower_bound().unwrap().to_string(),
            "22"
        );
        assert!(req("lts").lower_bound().is_none());
        assert!(req("<23").lower_bound().is_none());
    }

    #[test]
    fn equals_prefix_is_accepted_as_exact() {
        assert_eq!(req("=22.14.0"), VersionReq::Exact(Version::new(22, 14, 0)));
    }
}
