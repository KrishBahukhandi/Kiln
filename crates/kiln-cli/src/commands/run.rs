//! `kiln run` — execute a command inside the project environment.
//!
//! Transparent by design. Standard streams are inherited, arguments are passed
//! through untouched, and the child's exit code becomes Kiln's, so
//! `kiln run npm test` can stand in for `npm test` anywhere — a Makefile, a CI
//! step, a shell pipeline — without anything downstream noticing.
//!
//! Kiln prints nothing on the happy path. A wrapper that announces itself is a
//! wrapper you cannot pipe.

use std::path::Path;
use std::process::ExitCode;

use kiln_core::error::{Error, Result};
use kiln_core::{KilnPaths, Platform, Ui};
use kiln_exec::Environment;
use kiln_resolver::Lockfile;
use kiln_runtime::Registry;

use crate::cli::RunArgs;
use crate::commands::require_project;

pub fn run(args: &RunArgs, directory: &Path, ui: &Ui) -> Result<ExitCode> {
    let project = require_project(directory)?;
    let manifest = project.manifest();
    let platform = Platform::detect()?;
    let paths = KilnPaths::discover()?;
    let store = kiln_cache::ContentStore::new(paths.store());

    let environment_state = kiln_resolver::activate(
        manifest,
        &Registry::builtin(),
        &platform,
        &store,
        Lockfile::read(&project.lockfile_path())?.as_ref(),
    );
    environment_state.require_ready()?;

    // Recorded here rather than inside `activate`, so that read-only commands
    // like `kiln list` do not make an entry look used just by describing it.
    store.touch_all(
        environment_state
            .runtimes
            .iter()
            .filter_map(|r| r.entry.as_ref().map(|entry| &entry.digest)),
    );

    let (program, arguments) = resolve_command(args, manifest)?;

    let mut environment = Environment::from_manifest(manifest);
    for entry in environment_state.path_entries() {
        environment.prepend_path(entry);
    }

    if ui.verbosity() > 0 {
        ui.note(format!("  running: {program} {}", arguments.join(" ")));
    }

    // The command runs in the user's directory, not the project root: `kiln run
    // npm test` from a subdirectory must behave like `npm test` did there.
    let exit = kiln_exec::run(&program, &arguments, &environment, directory)?;
    Ok(ExitCode::from(exit.process_code()))
}

/// Work out what to actually run.
///
/// A first argument that names a `[commands]` entry expands to that command, and
/// any further arguments are appended — `kiln run test --watch` behaves the way
/// `npm run test -- --watch` does, without the `--`.
///
/// Named commands win over programs of the same name. `kiln run test` running
/// the project's test command rather than `/usr/bin/test` is what almost
/// everyone means; anyone who wants the binary can say `./test` or name its
/// path.
fn resolve_command(
    args: &RunArgs,
    manifest: &kiln_config::Manifest,
) -> Result<(String, Vec<String>)> {
    let (first, rest) = args
        .command
        .split_first()
        .ok_or_else(|| Error::config("No command was given").command("kiln run node --version"))?;

    let named = kiln_config::CommandName::parse(first)
        .ok()
        .and_then(|name| manifest.commands.get(&name));

    match named {
        Some(spec) => {
            let mut arguments: Vec<String> = spec.args().to_vec();
            arguments.extend_from_slice(rest);
            Ok((spec.program().to_string(), arguments))
        }
        None => Ok((first.clone(), rest.to_vec())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> kiln_config::Manifest {
        kiln_config::parse_str(text, Path::new("kiln.toml")).expect("valid manifest")
    }

    fn args(items: &[&str]) -> RunArgs {
        RunArgs {
            command: items.iter().map(|s| s.to_string()).collect(),
        }
    }

    const WITH_COMMANDS: &str = r#"
[project]
name = "app"
[runtime]
node = "22"
[commands]
dev = "npm run dev"
test = "vitest run"
"#;

    #[test]
    fn a_literal_command_is_passed_through() {
        let (program, arguments) =
            resolve_command(&args(&["node", "--version"]), &manifest(WITH_COMMANDS)).unwrap();
        assert_eq!(program, "node");
        assert_eq!(arguments, ["--version"]);
    }

    #[test]
    fn a_named_command_expands_to_its_argv() {
        let (program, arguments) =
            resolve_command(&args(&["dev"]), &manifest(WITH_COMMANDS)).unwrap();
        assert_eq!(program, "npm");
        assert_eq!(arguments, ["run", "dev"]);
    }

    #[test]
    fn extra_arguments_append_to_a_named_command() {
        let (program, arguments) =
            resolve_command(&args(&["test", "--watch"]), &manifest(WITH_COMMANDS)).unwrap();
        assert_eq!(program, "vitest");
        assert_eq!(arguments, ["run", "--watch"]);
    }

    #[test]
    fn a_named_command_wins_over_a_program_of_the_same_name() {
        // `/usr/bin/test` exists on every Unix; the project's `test` is what
        // someone typing `kiln run test` means.
        let (program, _) = resolve_command(&args(&["test"]), &manifest(WITH_COMMANDS)).unwrap();
        assert_eq!(program, "vitest");
    }

    #[test]
    fn an_unknown_name_is_treated_as_a_program() {
        let (program, arguments) =
            resolve_command(&args(&["python", "script.py"]), &manifest(WITH_COMMANDS)).unwrap();
        assert_eq!(program, "python");
        assert_eq!(arguments, ["script.py"]);
    }

    #[test]
    fn a_path_is_never_read_as_a_command_name() {
        // `./dev` must not expand to the `dev` command.
        let (program, _) = resolve_command(&args(&["./dev"]), &manifest(WITH_COMMANDS)).unwrap();
        assert_eq!(program, "./dev");
    }

    #[test]
    fn a_project_with_no_commands_runs_programs_normally() {
        let bare = manifest("[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\n");
        let (program, arguments) = resolve_command(&args(&["node", "-e", "1"]), &bare).unwrap();
        assert_eq!(program, "node");
        assert_eq!(arguments, ["-e", "1"]);
    }

    #[test]
    fn an_empty_invocation_is_rejected() {
        assert!(resolve_command(&args(&[]), &manifest(WITH_COMMANDS)).is_err());
    }
}
