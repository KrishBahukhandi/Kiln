//! The `kiln.toml` schema.
//!
//! ```toml
//! [project]
//! name = "example-app"
//! version = "0.1.0"
//!
//! [runtime]
//! node = "22.14.0"
//! python = "3.13.5"
//!
//! [tools]
//! pnpm = "10.12.1"
//!
//! [environment]
//! NODE_ENV = "development"
//!
//! [commands]
//! dev = "npm run dev"
//!
//! [services]         # parsed and reported; not managed by this release
//! postgres = "17"
//! ```
//!
//! Every map is a [`BTreeMap`], so iteration order is the manifest's meaning
//! rather than its layout. Two manifests that differ only in the order their
//! keys were typed must produce byte-identical resolution.
//!
//! The schema knows nothing about *which* runtimes exist. `node` and `nodejs`
//! are equally well-formed here; deciding that only one of them names a real
//! provider is the resolver's job. Keeping that boundary means the config crate
//! never has to be recompiled to add a runtime.

use std::collections::BTreeMap;

use kiln_core::VersionReq;
use serde::{Deserialize, Serialize};

use crate::command::CommandSpec;
use crate::names::{CommandName, EnvVarName, ProjectName, RuntimeName, ServiceName};

/// The file name Kiln looks for.
pub const MANIFEST_FILE: &str = "kiln.toml";

/// The lockfile name. Reserved here so both names live in one place.
pub const LOCKFILE_FILE: &str = "kiln.lock";

/// A parsed and validated `kiln.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Project identity.
    pub project: Project,

    /// Language runtimes the project needs, e.g. `node`, `python`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime: BTreeMap<RuntimeName, VersionReq>,

    /// Tools that ride on those runtimes, e.g. `pnpm`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<RuntimeName, VersionReq>,

    /// Environment variables exported into the project environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<EnvVarName, String>,

    /// Named commands, runnable with `kiln run <name>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commands: BTreeMap<CommandName, CommandSpec>,

    /// Backing services the project expects.
    ///
    /// Accepted and reported by `kiln doctor`, but this release does not start,
    /// stop or provision anything. See the roadmap.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<ServiceName, VersionReq>,
}

/// The `[project]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// Project name. Appears in Kiln's output.
    pub name: ProjectName,
    /// Optional project version. Kiln does not interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Optional one-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Manifest {
    /// Checks that cannot be expressed as a single field's validity.
    ///
    /// Anything that *can* be checked while deserialising one value is checked
    /// there instead, so it inherits a precise source span.
    pub fn validate(&self) -> kiln_core::Result<()> {
        for name in self.runtime.keys() {
            if self.tools.contains_key(name) {
                return Err(kiln_core::Error::config(format!(
                    "`{name}` is declared in both [runtime] and [tools]"
                ))
                .because("Kiln would not know which version to install")
                .hint(format!("remove one of the two `{name}` entries")));
            }
        }

        if self.runtime.is_empty() && self.tools.is_empty() {
            return Err(kiln_core::Error::config(
                "this manifest does not pin any runtimes or tools",
            )
            .because("there would be nothing for `kiln install` to do")
            .hint("add a runtime, for example `node = \"22\"` under [runtime]")
            .command("kiln init"));
        }

        Ok(())
    }

    /// Every runtime and tool the project pins, runtimes first.
    ///
    /// Most callers want this rather than the two maps separately: the
    /// distinction is documentation for humans, not a resolution rule.
    pub fn requirements(&self) -> impl Iterator<Item = (&RuntimeName, &VersionReq)> {
        self.runtime.iter().chain(self.tools.iter())
    }

    /// How many runtimes and tools are pinned.
    pub fn requirement_count(&self) -> usize {
        self.runtime.len() + self.tools.len()
    }

    /// Whether any requirement needs a provider's release list to resolve.
    ///
    /// Floating requirements are legal, but they are the reason `kiln.lock`
    /// exists: without it, two clones can resolve differently.
    pub fn has_floating_requirements(&self) -> bool {
        self.requirements().any(|(_, req)| req.is_floating())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_str;
    use std::path::Path;

    fn manifest(text: &str) -> Manifest {
        parse_str(text, Path::new("kiln.toml")).expect("manifest should parse")
    }

    const MINIMAL: &str = r#"
[project]
name = "app"

[runtime]
node = "22.14.0"
"#;

    #[test]
    fn requirements_iterate_runtimes_then_tools() {
        let manifest = manifest(
            r#"
[project]
name = "app"

[runtime]
python = "3.13"
node = "22"

[tools]
pnpm = "10"
"#,
        );
        let names: Vec<&str> = manifest
            .requirements()
            .map(|(name, _)| name.as_str())
            .collect();
        // Runtimes first, each group sorted: order is the manifest's meaning.
        assert_eq!(names, ["node", "python", "pnpm"]);
        assert_eq!(manifest.requirement_count(), 3);
    }

    #[test]
    fn key_order_in_the_file_does_not_change_the_model() {
        let a = manifest(
            r#"
[project]
name = "app"
[runtime]
node = "22"
python = "3.13"
"#,
        );
        let b = manifest(
            r#"
[project]
name = "app"
[runtime]
python = "3.13"
node = "22"
"#,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn exact_pins_are_not_floating() {
        assert!(!manifest(MINIMAL).has_floating_requirements());
        assert!(
            manifest(
                r#"
[project]
name = "app"
[runtime]
node = "22"
"#
            )
            .has_floating_requirements()
        );
    }

    #[test]
    fn a_runtime_cannot_also_be_a_tool() {
        let text = r#"
[project]
name = "app"

[runtime]
node = "22"

[tools]
node = "20"
"#;
        let err = parse_str(text, Path::new("kiln.toml")).unwrap_err();
        assert!(err.summary().contains("both [runtime] and [tools]"));
    }

    #[test]
    fn a_manifest_must_pin_something() {
        let err = parse_str("[project]\nname = \"app\"\n", Path::new("kiln.toml")).unwrap_err();
        assert!(err.summary().contains("does not pin any runtimes"));
    }

    #[test]
    fn optional_project_fields_are_optional() {
        let m = manifest(MINIMAL);
        assert_eq!(m.project.name.as_str(), "app");
        assert!(m.project.version.is_none());
        assert!(m.project.description.is_none());
    }

    #[test]
    fn services_are_parsed_even_though_they_are_not_managed() {
        let m = manifest(
            r#"
[project]
name = "app"
[runtime]
node = "22"
[services]
postgres = "17"
redis = "7"
"#,
        );
        assert_eq!(m.services.len(), 2);
        assert_eq!(
            m.services[&ServiceName::parse("postgres").unwrap()].to_string(),
            "17"
        );
    }
}
