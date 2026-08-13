//! Generating a `kiln.toml` that a human would have written.
//!
//! `kiln init` does not serialise the model with `toml::to_string`. A generated
//! manifest is a file someone will read, edit and review in a pull request, so
//! it keeps its section order, its comments, and the commented-out suggestions
//! that explain what Kiln noticed but could not act on.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use kiln_core::VersionReq;

use crate::command::CommandSpec;
use crate::names::{CommandName, EnvVarName, ProjectName, RuntimeName};

/// The manifest `kiln init` is about to write.
#[derive(Debug, Clone, Default)]
pub struct ManifestDraft {
    /// Project name.
    pub name: Option<ProjectName>,
    /// Runtimes to pin under `[runtime]`.
    pub runtimes: BTreeMap<RuntimeName, VersionReq>,
    /// Tools to pin under `[tools]`.
    pub tools: BTreeMap<RuntimeName, VersionReq>,
    /// Tools Kiln detected but has no provider for yet. Written commented out,
    /// each with the reason it is not active, so the manifest never promises an
    /// environment Kiln cannot deliver.
    pub deferred_tools: BTreeMap<RuntimeName, (VersionReq, String)>,
    /// Environment variables.
    pub environment: BTreeMap<EnvVarName, String>,
    /// Named commands.
    pub commands: BTreeMap<CommandName, CommandSpec>,
}

impl ManifestDraft {
    /// Render the manifest text.
    pub fn render(&self) -> String {
        let mut out = String::new();

        out.push_str(
            "# kiln.toml — the development environment for this repository.\n\
             #\n\
             # Commit this file. Anyone who clones the repository can then run:\n\
             #\n\
             #     kiln install\n\
             #     kiln shell\n\
             #\n\
             # and get exactly the runtimes pinned below.\n\n",
        );

        let name = self
            .name
            .as_ref()
            .map(ProjectName::as_str)
            .unwrap_or("my-app");
        out.push_str("[project]\n");
        let _ = writeln!(out, "name = {}", toml_string(name));
        out.push('\n');

        out.push_str("[runtime]\n");
        if self.runtimes.is_empty() {
            out.push_str("# node = \"22\"\n# python = \"3.13\"\n");
        } else {
            for (runtime, requirement) in &self.runtimes {
                let _ = writeln!(out, "{runtime} = {}", toml_string(&requirement.to_string()));
            }
        }

        if !self.tools.is_empty() || !self.deferred_tools.is_empty() {
            out.push_str("\n[tools]\n");
            for (tool, requirement) in &self.tools {
                let _ = writeln!(out, "{tool} = {}", toml_string(&requirement.to_string()));
            }
            for (tool, (requirement, reason)) in &self.deferred_tools {
                let _ = writeln!(out, "# {reason}");
                let _ = writeln!(out, "# {tool} = {}", toml_string(&requirement.to_string()));
            }
        }

        if !self.environment.is_empty() {
            out.push_str("\n[environment]\n");
            for (variable, value) in &self.environment {
                let _ = writeln!(out, "{variable} = {}", toml_string(value));
            }
        }

        if !self.commands.is_empty() {
            out.push_str("\n[commands]\n");
            out.push_str("# Run these with `kiln run <name>`. Kiln executes them directly,\n");
            out.push_str("# without a shell, so `&&`, `|` and `$` are not available here.\n");
            for (command, spec) in &self.commands {
                let _ = writeln!(out, "{command} = {}", toml_string(spec.raw()));
            }
        }

        out
    }
}

/// Quote a value as a TOML basic string.
///
/// Written by hand because generated files must be safe even when the input is
/// not: a project name lifted from a `package.json` is untrusted text, and it
/// must not be able to close the quote and add a key of its own.
pub fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Remaining control characters have no literal form in TOML.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_str;
    use std::path::Path;

    fn draft() -> ManifestDraft {
        let mut draft = ManifestDraft {
            name: Some(ProjectName::parse("example-app").unwrap()),
            ..Default::default()
        };
        draft.runtimes.insert(
            RuntimeName::parse("node").unwrap(),
            VersionReq::parse("22").unwrap(),
        );
        draft.runtimes.insert(
            RuntimeName::parse("python").unwrap(),
            VersionReq::parse("3.13").unwrap(),
        );
        draft
    }

    #[test]
    fn rendered_manifests_parse_back() {
        let text = draft().render();
        let manifest =
            parse_str(&text, Path::new("kiln.toml")).expect("generated manifest is valid");
        assert_eq!(manifest.project.name.as_str(), "example-app");
        assert_eq!(manifest.runtime.len(), 2);
    }

    #[test]
    fn round_trip_preserves_every_requirement() {
        let mut draft = draft();
        draft.tools.insert(
            RuntimeName::parse("pnpm").unwrap(),
            VersionReq::parse("10.12.1").unwrap(),
        );
        draft
            .environment
            .insert(EnvVarName::parse("NODE_ENV").unwrap(), "development".into());
        draft.commands.insert(
            CommandName::parse("dev").unwrap(),
            CommandSpec::parse("npm run dev").unwrap(),
        );

        let manifest = parse_str(&draft.render(), Path::new("kiln.toml")).expect("valid");
        assert_eq!(manifest.runtime.len(), 2);
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.environment.len(), 1);
        assert_eq!(manifest.commands.len(), 1);
        assert_eq!(
            manifest.commands[&CommandName::parse("dev").unwrap()].raw(),
            "npm run dev"
        );
    }

    #[test]
    fn deferred_tools_are_commented_out_with_their_reason() {
        let mut draft = draft();
        draft.deferred_tools.insert(
            RuntimeName::parse("pnpm").unwrap(),
            (
                VersionReq::parse("10.12.1").unwrap(),
                "pnpm has no provider yet; uncomment once one lands".into(),
            ),
        );

        let text = draft.render();
        assert!(text.contains("# pnpm has no provider yet"));
        assert!(text.contains("# pnpm = \"10.12.1\""));

        // Commented suggestions must not become real requirements.
        let manifest = parse_str(&text, Path::new("kiln.toml")).expect("valid");
        assert!(manifest.tools.is_empty());
    }

    #[test]
    fn an_empty_draft_still_renders_a_readable_starting_point() {
        let text = ManifestDraft::default().render();
        assert!(text.contains("[project]"));
        assert!(text.contains("[runtime]"));
        assert!(text.contains("# node = \"22\""));
        // It has no active requirement, so it is intentionally not yet valid.
        assert!(parse_str(&text, Path::new("kiln.toml")).is_err());
    }

    #[test]
    fn untrusted_names_cannot_inject_toml() {
        let hostile = "app\"\n[environment]\nPATH = \"/evil";
        let quoted = toml_string(hostile);
        assert!(!quoted[1..quoted.len() - 1].contains('\n'));
        assert_eq!(quoted, r#""app\"\n[environment]\nPATH = \"/evil""#);
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(toml_string("a\u{7}b"), r#""a\u0007b""#);
        assert_eq!(toml_string("tab\there"), r#""tab\there""#);
        assert_eq!(toml_string(r"back\slash"), r#""back\\slash""#);
    }

    #[test]
    fn the_header_tells_the_reader_what_to_do_with_the_file() {
        let text = draft().render();
        assert!(text.starts_with("# kiln.toml"));
        assert!(text.contains("kiln install"));
        assert!(text.contains("Commit this file"));
    }
}
