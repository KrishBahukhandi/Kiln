//! Turning `kiln.toml` text into a [`Manifest`], and failures into diagnostics.
//!
//! The whole point of this module is the second half. A configuration error is
//! the first thing many people will ever see Kiln do, and "Error: invalid input"
//! is not an acceptable first impression.

use std::path::Path;

use kiln_core::error::{Error, IoResultExt, Result, decode_serde_message};
use kiln_core::source::SourceLocation;

use crate::manifest::{MANIFEST_FILE, Manifest};

/// Encode a Kiln error for `serde::de::Error::custom`.
///
/// Re-exported for the `Deserialize` impls in this crate so they all agree on
/// the format [`from_toml_error`] decodes.
pub(crate) fn serde_message(error: &Error) -> String {
    kiln_core::error::to_serde_message(error)
}

/// Parse manifest text. `path` is used only for diagnostics.
pub fn parse_str(text: &str, path: &Path) -> Result<Manifest> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| from_toml_error(&e, text, path))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Read and parse a manifest from disk.
pub fn parse_file(path: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            Error::not_found(format!("No {MANIFEST_FILE} at {}", path.display()))
                .command("kiln init")
        }
        _ => Error::io(format!("Could not read {MANIFEST_FILE}"), path, e),
    })?;
    parse_str(&text, path)
}

/// Write a manifest to disk atomically.
///
/// The manifest is written to a sibling temporary file and renamed into place,
/// so an interrupted write cannot leave a project holding half a manifest.
pub fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = directory.join(format!(".{MANIFEST_FILE}.{}.tmp", std::process::id()));

    std::fs::write(&temporary, contents).io_context("Could not write the manifest", &temporary)?;

    if let Err(e) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(Error::io("Could not write the manifest", path, e));
    }
    Ok(())
}

/// Convert a TOML deserialization failure into a Kiln diagnostic.
fn from_toml_error(error: &toml::de::Error, text: &str, path: &Path) -> Error {
    let decoded = decode_serde_message(error.message());
    let (label, expected) = refine(&decoded.label, decoded.expected);

    let mut diagnostic = Error::config(format!("Invalid {MANIFEST_FILE}"));

    match error.span() {
        Some(span) => {
            diagnostic = diagnostic.at(SourceLocation::from_span(
                path,
                text,
                span,
                Some(label.clone()),
            ));
        }
        None => diagnostic = diagnostic.because(label.clone()),
    }

    if let Some(expected) = expected {
        diagnostic = diagnostic.expected(expected);
    }
    for hint in decoded.hints {
        diagnostic = diagnostic.hint(hint);
    }
    for hint in hints_for(&label) {
        diagnostic = diagnostic.hint(hint);
    }
    diagnostic
}

/// Reshape `serde`'s own messages so they read like the rest of Kiln's output.
///
/// `serde` reports unknown fields as one long line ending in a comma-separated
/// list of alternatives. Splitting the list into the "Expected" block turns an
/// unreadable sentence into a menu.
fn refine(label: &str, expected: Option<String>) -> (String, Option<String>) {
    if expected.is_some() {
        return (label.to_string(), expected);
    }
    if let Some((head, tail)) = label.split_once(", expected one of ") {
        let alternatives: Vec<&str> = tail.split(", ").map(str::trim).collect();
        return (head.trim().to_string(), Some(alternatives.join("\n")));
    }
    (label.to_string(), None)
}

/// Suggestions for the mistakes people actually make.
///
/// Every entry here is a name that a reasonable person would guess, which is
/// exactly why guessing it deserves an answer rather than a rejection.
fn hints_for(label: &str) -> Vec<String> {
    const NEAR_MISSES: &[(&str, &str)] = &[
        ("package_manager", "package managers go under [tools]"),
        ("packages", "package managers go under [tools]"),
        (
            "runtimes",
            "the section is called [runtime], in the singular",
        ),
        ("tool", "the section is called [tools], in the plural"),
        ("env", "environment variables go under [environment]"),
        ("scripts", "named commands go under [commands]"),
        (
            "dependencies",
            "runtimes go under [runtime], tools under [tools]",
        ),
    ];

    let mut hints = Vec::new();
    if label.starts_with("unknown field") {
        for (name, advice) in NEAR_MISSES {
            if label.contains(&format!("`{name}`")) {
                hints.push((*advice).to_string());
            }
        }
    }
    if label.contains("missing field `project`") {
        hints.push("every kiln.toml starts with a [project] table".to_string());
    }
    if label.contains("missing field `name`") {
        hints.push("add `name = \"my-app\"` under [project]".to_string());
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::ErrorKind;

    fn error_for(text: &str) -> Error {
        parse_str(text, Path::new("/app/kiln.toml")).expect_err("should not parse")
    }

    const VALID: &str = r#"
[project]
name = "example-app"
version = "0.1.0"

[runtime]
node = "22.14.0"
python = "3.13.5"

[tools]
pnpm = "10.12.1"

[environment]
NODE_ENV = "development"

[commands]
dev = "npm run dev"
test = "npm test"
"#;

    #[test]
    fn parses_a_complete_manifest() {
        let manifest = parse_str(VALID, Path::new("kiln.toml")).expect("valid manifest");
        assert_eq!(manifest.project.name.as_str(), "example-app");
        assert_eq!(manifest.project.version.as_deref(), Some("0.1.0"));
        assert_eq!(manifest.runtime.len(), 2);
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.environment.len(), 1);
        assert_eq!(manifest.commands.len(), 2);
        assert!(!manifest.has_floating_requirements());
    }

    #[test]
    fn bad_version_requirements_point_at_the_value() {
        let error = error_for(
            r#"
[project]
name = "app"

[runtime]
node = "banana"
"#,
        );
        assert_eq!(error.kind(), ErrorKind::Config);
        assert_eq!(error.summary(), "Invalid kiln.toml");

        let location = error.location().expect("a code frame");
        assert_eq!(location.line, 6);
        assert_eq!(location.line_text, r#"node = "banana""#);
        assert_eq!(location.column, 8);
        assert_eq!(location.highlight_len, 8);
        assert!(
            location.label.as_deref().unwrap().contains("banana"),
            "label: {:?}",
            location.label
        );

        // The accepted spellings survive the trip through serde.
        let expected = error.expectation().expect("expected forms");
        assert!(expected.contains("22.14.0"));
        assert!(expected.contains(">=22, <23"));
    }

    #[test]
    fn hints_survive_the_trip_through_serde() {
        let error = error_for(
            r#"
[project]
name = "app"
[runtime]
node = "*"
"#,
        );
        assert!(
            error.hints().iter().any(|h| h.text().contains("latest")),
            "hints: {:?}",
            error.hints()
        );
    }

    #[test]
    fn unknown_sections_become_a_menu() {
        let error = error_for(
            r#"
[project]
name = "app"
[runtimes]
node = "22"
"#,
        );
        let location = error.location().expect("a code frame");
        assert!(
            location
                .label
                .as_deref()
                .unwrap()
                .starts_with("unknown field")
        );

        let expected = error.expectation().expect("alternatives");
        assert!(expected.contains("`runtime`"), "expected: {expected}");
        assert!(expected.lines().count() > 3, "one alternative per line");

        assert!(
            error.hints().iter().any(|h| h.text().contains("singular")),
            "near-miss hint missing"
        );
    }

    #[test]
    fn package_manager_section_is_redirected_to_tools() {
        let error = error_for(
            r#"
[project]
name = "app"
[runtime]
node = "22"
[package_manager]
pnpm = "10"
"#,
        );
        assert!(
            error.hints().iter().any(|h| h.text().contains("[tools]")),
            "hints: {:?}",
            error.hints()
        );
    }

    #[test]
    fn a_missing_project_table_is_explained() {
        let error = error_for("[runtime]\nnode = \"22\"\n");
        assert!(
            error.hints().iter().any(|h| h.text().contains("[project]")),
            "hints: {:?}",
            error.hints()
        );
    }

    #[test]
    fn reserved_environment_variables_point_at_the_key() {
        let error = error_for(
            r#"
[project]
name = "app"
[runtime]
node = "22"
[environment]
PATH = "/usr/bin"
"#,
        );
        let label = error
            .location()
            .and_then(|l| l.label.clone())
            .or_else(|| error.reason().map(str::to_string))
            .expect("some explanation");
        assert!(label.contains("PATH"), "label: {label}");
    }

    #[test]
    fn shell_operators_in_commands_are_rejected_with_a_frame() {
        let error = error_for(
            r#"
[project]
name = "app"
[runtime]
node = "22"
[commands]
dev = "npm run build && npm run dev"
"#,
        );
        let location = error.location().expect("a code frame");
        assert_eq!(location.line, 7);
        assert!(location.label.as_deref().unwrap().contains('&'));
        assert!(
            error
                .hints()
                .iter()
                .any(|h| h.text().contains("separate commands"))
        );
    }

    #[test]
    fn malformed_toml_is_reported_as_a_config_error() {
        let error = error_for("[project\nname = \"app\"\n");
        assert_eq!(error.kind(), ErrorKind::Config);
        assert!(error.location().is_some() || error.reason().is_some());
    }

    #[test]
    fn a_missing_file_suggests_init() {
        let error = parse_file(Path::new("/nonexistent/kiln.toml")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(error.hints().iter().any(|h| h.text() == "kiln init"));
    }

    #[test]
    fn atomic_writes_leave_no_temporary_behind() {
        let directory = std::env::temp_dir().join(format!("kiln-write-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(MANIFEST_FILE);

        write_atomically(&path, VALID).expect("write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), VALID);

        let leftovers: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files remained");

        std::fs::remove_dir_all(&directory).ok();
    }
}
