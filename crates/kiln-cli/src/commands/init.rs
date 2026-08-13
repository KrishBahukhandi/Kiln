//! `kiln init` — propose a manifest for the project already in this directory.
//!
//! Detection proposes; the user decides. Kiln never writes a manifest without
//! showing what is in it first, because a wrong `kiln.toml` is worse than no
//! `kiln.toml`: it looks authoritative and it gets committed.

use std::collections::BTreeMap;
use std::path::Path;

use kiln_config::{MANIFEST_FILE, ManifestDraft, ProjectName, RuntimeName, write_atomically};
use kiln_core::error::{Error, Result};
use kiln_core::ui::{Style, paint};
use kiln_core::{Ui, VersionReq};
use kiln_runtime::{Evidence, Registry, RuntimeKind};

use crate::cli::InitArgs;
use crate::prompt;

/// A line in the proposal.
struct Row {
    name: String,
    requirement: String,
    source: String,
}

pub fn run(args: &InitArgs, directory: &Path, ui: &Ui) -> Result<()> {
    let manifest_path = directory.join(MANIFEST_FILE);
    if manifest_path.exists() && !args.force {
        return Err(Error::conflict(format!("{MANIFEST_FILE} already exists"))
            .because(format!(
                "{} is already a Kiln project.",
                manifest_path.display()
            ))
            .hint("edit it directly to change the environment")
            .command("kiln init --force"));
    }

    let registry = Registry::builtin();
    let overrides = parse_overrides(&args.runtimes, &registry)?;

    let mut draft = ManifestDraft {
        name: Some(project_name(args.name.as_deref(), directory)?),
        ..Default::default()
    };
    let mut sources: BTreeMap<String, String> = BTreeMap::new();

    if !args.no_detect {
        for provider in registry.providers() {
            // An explicit `--runtime` pin is an answer, not a hint; do not
            // spend time sniffing for something the user already told us.
            if overrides.contains_key(provider.id()) {
                continue;
            }
            let Some(evidence) = provider.detect(directory) else {
                continue;
            };

            let (requirement, source) = match evidence {
                Evidence::Pinned {
                    requirement,
                    source,
                } => narrow(requirement, source),
                Evidence::Present { source } => (
                    provider.default_requirement(),
                    format!("{source}, no version pinned"),
                ),
            };

            insert(&mut draft, provider.kind(), provider.id(), requirement);
            sources.insert(provider.id().to_string(), source);
        }

        collect_tools(&mut draft, &mut sources, &registry, directory);
    }

    for (id, requirement) in overrides {
        let provider = registry.get(&id).ok_or_else(|| registry.unknown(&id))?;
        insert(&mut draft, provider.kind(), provider.id(), requirement);
        sources.insert(id, "--runtime".to_string());
    }

    if draft.runtimes.is_empty() && draft.tools.is_empty() {
        return Err(nothing_detected(directory, &registry));
    }

    let rows = describe(&draft, &sources, &registry);
    let deferred = describe_deferred(&draft, &sources, &registry);
    report(ui, &draft, directory, &rows, &deferred);

    if !args.yes && !prompt::confirm(ui, "\nCreate kiln.toml?", true)? {
        ui.blank();
        ui.note("Nothing was written.");
        return Ok(());
    }

    write_atomically(&manifest_path, &draft.render())?;

    ui.blank();
    ui.ok(format!("Wrote {}", manifest_path.display()));
    ui.blank();
    ui.note("  Commit this file so every clone reproduces the same environment.");
    ui.blank();
    ui.section("Next");
    ui.status(format!(
        "  {}   install the runtimes and write kiln.lock",
        paint("kiln install", Style::Cyan, ui.color())
    ));
    ui.status(format!(
        "  {}     a shell with them in front",
        paint("kiln shell", Style::Cyan, ui.color())
    ));

    Ok(())
}

/// Turn a compatibility statement into a pin.
///
/// `engines.node = ">=22"` and `requires-python = ">=3.11"` say what a project
/// *tolerates*, not what it should be developed against. Copying them into
/// `kiln.toml` verbatim would produce a manifest that resolves to a different
/// major release every year — the drift Kiln exists to remove.
///
/// So an open-ended requirement becomes a pin on its own lower bound, which
/// always satisfies the original, and the change is stated in the proposal that
/// the user is about to approve.
fn narrow(requirement: VersionReq, source: String) -> (VersionReq, String) {
    if !requirement.is_open_ended() {
        return (requirement, source);
    }
    match requirement.lower_bound() {
        Some(bound) => {
            let pinned = VersionReq::Pinned(bound);
            let note = format!("{source}, pinned from {requirement}");
            (pinned, note)
        }
        None => (requirement, source),
    }
}

/// Put a requirement in the right section for its provider's role.
fn insert(draft: &mut ManifestDraft, kind: RuntimeKind, id: &str, requirement: VersionReq) {
    let Ok(name) = RuntimeName::parse(id) else {
        // Provider identifiers are compile-time constants that a unit test in
        // `kiln-runtime` already checks; nothing can reach this at runtime.
        return;
    };
    match kind {
        RuntimeKind::Language => draft.runtimes.insert(name, requirement),
        RuntimeKind::PackageManager => draft.tools.insert(name, requirement),
    };
}

/// Record tools the project's files mention.
///
/// A tool Kiln has a provider for becomes a real entry. Everything else is
/// written commented out with the reason, so the manifest never claims Kiln will
/// manage something it cannot.
fn collect_tools(
    draft: &mut ManifestDraft,
    sources: &mut BTreeMap<String, String>,
    registry: &Registry,
    directory: &Path,
) {
    for provider in registry.providers() {
        for tool in provider.detect_tools(directory) {
            let Ok(name) = RuntimeName::parse(&tool.name) else {
                continue;
            };
            if draft.runtimes.contains_key(&name) || draft.tools.contains_key(&name) {
                continue;
            }

            match registry.get(&tool.name) {
                Some(tool_provider) => {
                    let requirement = tool
                        .requirement
                        .unwrap_or_else(|| tool_provider.default_requirement());
                    draft.tools.insert(name, requirement);
                    sources.insert(tool.name, tool.source);
                }
                None => {
                    let Some(requirement) = tool.requirement else {
                        // Without a version there is nothing to propose, and
                        // guessing one for a tool Kiln cannot install helps
                        // nobody.
                        continue;
                    };
                    draft.deferred_tools.insert(
                        name,
                        (
                            requirement,
                            format!(
                                "{} is not managed by Kiln yet (found in {})",
                                tool.name, tool.source
                            ),
                        ),
                    );
                    sources.insert(tool.name, tool.source);
                }
            }
        }
    }
}

fn describe(
    draft: &ManifestDraft,
    sources: &BTreeMap<String, String>,
    registry: &Registry,
) -> Vec<Row> {
    draft
        .runtimes
        .iter()
        .chain(draft.tools.iter())
        .map(|(name, requirement)| Row {
            name: registry
                .get(name.as_str())
                .map(|p| p.display_name().to_string())
                .unwrap_or_else(|| name.to_string()),
            requirement: requirement.to_string(),
            source: sources
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| "proposed".to_string()),
        })
        .collect()
}

fn describe_deferred(
    draft: &ManifestDraft,
    sources: &BTreeMap<String, String>,
    registry: &Registry,
) -> Vec<Row> {
    draft
        .deferred_tools
        .iter()
        .map(|(name, (requirement, _))| Row {
            name: registry
                .get(name.as_str())
                .map(|p| p.display_name().to_string())
                .unwrap_or_else(|| name.to_string()),
            requirement: requirement.to_string(),
            source: sources
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| "detected".to_string()),
        })
        .collect()
}

fn report(ui: &Ui, draft: &ManifestDraft, directory: &Path, rows: &[Row], deferred: &[Row]) {
    let name_width = rows
        .iter()
        .chain(deferred)
        .map(|r| r.name.len())
        .max()
        .unwrap_or(0)
        .max(9);
    let requirement_width = rows
        .iter()
        .chain(deferred)
        .map(|r| r.requirement.len())
        .max()
        .unwrap_or(0)
        .max(7);

    ui.banner("init");
    ui.blank();
    ui.section("Project");
    ui.field(
        "name",
        draft.name.as_ref().map(ProjectName::as_str).unwrap_or("-"),
        name_width,
    );
    ui.field("directory", &directory.display().to_string(), name_width);

    ui.blank();
    ui.section("Environment");
    for row in rows {
        ui.status(format!(
            "  {:<name_width$}  {:<requirement_width$}  {}",
            row.name,
            row.requirement,
            kiln_core::ui::paint(&row.source, kiln_core::ui::Style::Dim, ui.color()),
        ));
    }

    if !deferred.is_empty() {
        ui.blank();
        ui.section("Detected, but not managed by this release");
        for row in deferred {
            ui.status(format!(
                "  {:<name_width$}  {:<requirement_width$}  {}",
                row.name,
                row.requirement,
                kiln_core::ui::paint(&row.source, kiln_core::ui::Style::Dim, ui.color()),
            ));
        }
    }
}

/// Turn `--runtime name=requirement` arguments into pins.
fn parse_overrides(specs: &[String], registry: &Registry) -> Result<BTreeMap<String, VersionReq>> {
    let mut overrides = BTreeMap::new();

    for spec in specs {
        let Some((name, requirement)) = spec.split_once('=') else {
            return Err(Error::config(format!("`{spec}` is not a runtime pin"))
                .because("a pin is a runtime name, `=`, and a version requirement")
                .expected("NAME=REQUIREMENT")
                .command("kiln init --runtime node=22"));
        };

        let name = RuntimeName::parse(name.trim())?;
        if !registry.contains(name.as_str()) {
            return Err(registry.unknown(name.as_str()));
        }
        let requirement = VersionReq::parse(requirement.trim())?;

        if overrides.insert(name.to_string(), requirement).is_some() {
            return Err(Error::config(format!("`{name}` was pinned more than once"))
                .because("Kiln would not know which of the two requirements you meant"));
        }
    }
    Ok(overrides)
}

/// The project name to propose: the one given, or the directory's own name.
fn project_name(requested: Option<&str>, directory: &Path) -> Result<ProjectName> {
    if let Some(requested) = requested {
        return ProjectName::parse(requested);
    }

    let derived = directory
        .file_name()
        .map(|name| sanitize(&name.to_string_lossy()))
        .and_then(|candidate| ProjectName::parse(&candidate).ok());

    // A directory named `.` or `/` tells us nothing; a placeholder the user can
    // edit beats refusing to run.
    derived.map_or_else(|| ProjectName::parse("my-app"), Ok)
}

/// Reduce arbitrary directory names to something a project name allows.
fn sanitize(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();

    let trimmed = mapped.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let mut cleaned = trimmed.replace("..", "-");
    cleaned.truncate(64);
    cleaned
}

fn nothing_detected(directory: &Path, registry: &Registry) -> Error {
    let mut error = Error::not_found("Kiln could not tell what this project needs")
        .because(format!(
            "No project files were found in {}.",
            directory.display()
        ))
        .expected(
            "a package.json, pyproject.toml, .nvmrc, .python-version or similar\n\
             marker that says which runtime the project uses",
        );

    for provider in registry.providers() {
        error = error.command(format!(
            "kiln init --runtime {}={}",
            provider.id(),
            provider.default_requirement()
        ));
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_name_is_used_verbatim() {
        let name = project_name(Some("my-app"), Path::new("/tmp/whatever")).unwrap();
        assert_eq!(name.as_str(), "my-app");
    }

    #[test]
    fn an_invalid_explicit_name_is_reported_rather_than_repaired() {
        // Silently rewriting what the user typed would be worse than saying no.
        assert!(project_name(Some("../escape"), Path::new("/tmp")).is_err());
        assert!(project_name(Some(""), Path::new("/tmp")).is_err());
    }

    #[test]
    fn the_directory_name_becomes_the_project_name() {
        let name = project_name(None, Path::new("/Users/dev/my-app")).unwrap();
        assert_eq!(name.as_str(), "my-app");
    }

    #[test]
    fn awkward_directory_names_are_reduced_to_something_valid() {
        for (directory, expected) in [
            ("/Users/dev/My Project", "My-Project"),
            ("/Users/dev/@scope", "scope"),
            ("/Users/dev/.hidden", "hidden"),
            ("/Users/dev/a..b", "a-b"),
            ("/Users/dev/app+v2", "app-v2"),
        ] {
            let name = project_name(None, Path::new(directory)).unwrap();
            assert_eq!(name.as_str(), expected, "for {directory}");
        }
    }

    #[test]
    fn an_unusable_directory_name_falls_back_to_a_placeholder() {
        assert_eq!(
            project_name(None, Path::new("/")).unwrap().as_str(),
            "my-app"
        );
        assert_eq!(
            project_name(None, Path::new("/Users/dev/---"))
                .unwrap()
                .as_str(),
            "my-app"
        );
    }

    #[test]
    fn overlong_directory_names_are_truncated_to_a_valid_length() {
        let long = format!("/tmp/{}", "a".repeat(200));
        let name = project_name(None, Path::new(&long)).unwrap();
        assert_eq!(name.as_str().len(), 64);
    }

    #[test]
    fn runtime_pins_parse() {
        let registry = Registry::builtin();
        let overrides =
            parse_overrides(&["node=22".into(), "python=3.13.5".into()], &registry).unwrap();

        assert_eq!(overrides["node"].to_string(), "22");
        assert_eq!(overrides["python"].to_string(), "3.13.5");
    }

    #[test]
    fn runtime_pins_tolerate_spacing() {
        let registry = Registry::builtin();
        let overrides = parse_overrides(&[" node = 22 ".into()], &registry).unwrap();
        assert_eq!(overrides["node"].to_string(), "22");
    }

    #[test]
    fn malformed_runtime_pins_explain_the_shape() {
        let registry = Registry::builtin();
        let error = parse_overrides(&["node".into()], &registry).unwrap_err();
        assert_eq!(error.expectation(), Some("NAME=REQUIREMENT"));
    }

    #[test]
    fn unknown_runtime_pins_are_rejected_with_a_correction() {
        let registry = Registry::builtin();
        let error = parse_overrides(&["nodejs=22".into()], &registry).unwrap_err();
        assert!(
            error
                .hints()
                .iter()
                .any(|h| h.text().contains("did you mean `node`"))
        );
    }

    #[test]
    fn bad_requirements_in_a_pin_are_rejected() {
        let registry = Registry::builtin();
        let error = parse_overrides(&["node=banana".into()], &registry).unwrap_err();
        assert!(error.expectation().unwrap().contains("22.14.0"));
    }

    #[test]
    fn pinning_the_same_runtime_twice_is_an_error() {
        let registry = Registry::builtin();
        assert!(parse_overrides(&["node=22".into(), "node=20".into()], &registry).is_err());
    }

    #[test]
    fn open_ended_requirements_become_pins() {
        let (requirement, source) = narrow(
            VersionReq::parse(">=22").unwrap(),
            "package.json (engines.node)".into(),
        );
        assert_eq!(requirement.to_string(), "22");
        assert_eq!(source, "package.json (engines.node), pinned from >=22");

        // The pin still satisfies what the project declared.
        assert!(
            VersionReq::parse(">=22")
                .unwrap()
                .matches(&kiln_core::Version::new(22, 14, 0))
        );
        assert!(requirement.matches(&kiln_core::Version::new(22, 14, 0)));
    }

    #[test]
    fn already_bounded_requirements_are_left_alone() {
        for text in ["22", "22.14.0", "^22", ">=22, <23", "lts"] {
            let (requirement, source) = narrow(VersionReq::parse(text).unwrap(), ".nvmrc".into());
            assert_eq!(requirement.to_string(), text);
            assert_eq!(source, ".nvmrc");
        }
    }

    #[test]
    fn the_nothing_detected_error_offers_a_way_forward() {
        let error = nothing_detected(Path::new("/tmp/empty"), &Registry::builtin());
        let commands: Vec<&str> = error.hints().iter().map(|h| h.text()).collect();
        assert!(
            commands
                .iter()
                .any(|c| c.starts_with("kiln init --runtime node="))
        );
        assert!(
            commands
                .iter()
                .any(|c| c.starts_with("kiln init --runtime python="))
        );
    }
}
