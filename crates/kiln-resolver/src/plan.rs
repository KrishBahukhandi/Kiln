//! Checking a manifest against reality before touching the network.
//!
//! Everything here is offline. A misspelled runtime, or a machine no provider
//! publishes for, should be reported in milliseconds — not after a download has
//! already started.

use kiln_config::Manifest;
use kiln_core::error::{Error, Result};
use kiln_core::{Platform, VersionReq};
use kiln_runtime::{Registry, RuntimeKind};

/// One requirement, matched to the provider that will satisfy it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRuntime {
    /// The identifier from `kiln.toml`, e.g. `node`.
    pub id: String,
    /// The provider's human-facing name, e.g. `Node.js`.
    pub display_name: String,
    /// Whether this is a language runtime or a tool.
    pub kind: RuntimeKind,
    /// The requirement as written by the user.
    pub requirement: VersionReq,
}

/// Match every requirement in the manifest to a provider that can satisfy it.
///
/// Fails on the first requirement Kiln cannot serve, naming what went wrong and
/// what the alternatives are.
pub fn plan(
    manifest: &Manifest,
    registry: &Registry,
    platform: &Platform,
) -> Result<Vec<PlannedRuntime>> {
    if !platform.is_supported() {
        return Err(
            Error::unsupported(format!("Kiln cannot manage runtimes on {platform}"))
                .because("this release installs runtimes for macOS and Linux only")
                .hint("follow platform support in the project roadmap"),
        );
    }

    let mut planned = Vec::with_capacity(manifest.requirement_count());
    for (name, requirement) in manifest.requirements() {
        let Some(provider) = registry.get(name.as_str()) else {
            return Err(registry.unknown(name.as_str()));
        };

        if !provider.supports(platform) {
            return Err(Error::unsupported(format!(
                "{} is not available for {platform}",
                provider.display_name()
            ))
            .because(format!(
                "no prebuilt {} artifacts are published for this platform",
                provider.display_name()
            ))
            .hint(format!("see {}", provider.homepage()))
            .command("kiln doctor"));
        }

        planned.push(PlannedRuntime {
            id: provider.id().to_string(),
            display_name: provider.display_name().to_string(),
            kind: provider.kind(),
            requirement: requirement.clone(),
        });
    }
    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::{Arch, Os};
    use std::path::Path;

    fn manifest(text: &str) -> Manifest {
        kiln_config::parse_str(text, Path::new("kiln.toml")).expect("valid manifest")
    }

    fn host() -> Platform {
        Platform::detect().expect("host platform")
    }

    #[test]
    fn plans_every_requirement_in_order() {
        let manifest = manifest(
            r#"
[project]
name = "app"

[runtime]
node = "22.14.0"
python = "3.13.5"
"#,
        );

        let planned = plan(&manifest, &Registry::builtin(), &host()).expect("plan");
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].id, "node");
        assert_eq!(planned[0].display_name, "Node.js");
        assert_eq!(planned[0].requirement.to_string(), "22.14.0");
        assert_eq!(planned[1].id, "python");
        assert_eq!(planned[1].kind, RuntimeKind::Language);
    }

    #[test]
    fn an_unknown_runtime_lists_what_kiln_does_know() {
        let manifest = manifest(
            r#"
[project]
name = "app"
[runtime]
frobnicator = "1"
"#,
        );

        let error = plan(&manifest, &Registry::builtin(), &host()).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::NotFound);
        assert!(error.summary().contains("frobnicator"));
        assert!(error.expectation().unwrap().contains("node"));
        assert!(error.expectation().unwrap().contains("python"));
    }

    #[test]
    fn a_near_miss_gets_a_correction() {
        let manifest = manifest(
            r#"
[project]
name = "app"
[runtime]
nodejs = "22"
"#,
        );

        let error = plan(&manifest, &Registry::builtin(), &host()).unwrap_err();
        assert!(
            error
                .hints()
                .iter()
                .any(|h| h.text().contains("did you mean `node`")),
            "hints: {:?}",
            error.hints()
        );
    }

    #[test]
    fn an_unrelated_name_gets_no_misleading_correction() {
        let manifest = manifest(
            r#"
[project]
name = "app"
[runtime]
postgres = "17"
"#,
        );

        let error = plan(&manifest, &Registry::builtin(), &host()).unwrap_err();
        assert!(
            !error
                .hints()
                .iter()
                .any(|h| h.text().contains("did you mean")),
            "should not guess: {:?}",
            error.hints()
        );
    }

    #[test]
    fn unsupported_platforms_are_refused_before_anything_else() {
        let manifest = manifest(
            r#"
[project]
name = "app"
[runtime]
frobnicator = "1"
"#,
        );

        // The platform check runs first, so this reports the platform rather
        // than the unknown runtime.
        let windows = Platform::new(Os::Windows, Arch::X86_64, None);
        let error = plan(&manifest, &Registry::builtin(), &windows).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::Unsupported);
        assert!(error.summary().contains("Windows"));
    }

    #[test]
    fn tools_are_planned_alongside_runtimes() {
        let manifest = manifest(
            r#"
[project]
name = "app"
[runtime]
node = "22"
[tools]
python = "3.13"
"#,
        );

        let planned = plan(&manifest, &Registry::builtin(), &host()).expect("plan");
        let ids: Vec<&str> = planned.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["node", "python"]);
    }
}
