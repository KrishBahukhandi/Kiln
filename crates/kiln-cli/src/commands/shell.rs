//! `kiln shell` — a shell with the project's runtimes in front.
//!
//! Kiln starts a child shell with a modified environment and waits for it. It
//! writes nothing to a shell profile, installs no hook, and leaves no state
//! behind: when the shell exits, the environment it changed exits with it. That
//! is the entire mechanism, and it is why "does Kiln mess with my shell?" has a
//! one-word answer.
//!
//! The consequence, stated plainly in the banner, is that this is a `PATH`
//! change and not a sandbox. Everything else on the machine still works, which
//! is the point — `git`, `ssh` and your editor have to keep functioning inside
//! it.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use kiln_core::error::{Error, Result};
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Platform, Ui};
use kiln_exec::Environment;
use kiln_exec::env::{KILN_PROJECT_ENV, active_project};
use kiln_resolver::Lockfile;
use kiln_runtime::Registry;

use crate::cli::ShellArgs;
use crate::commands::require_project;

/// The shell to fall back to when `$SHELL` says nothing.
const FALLBACK_SHELL: &str = "/bin/sh";

pub fn run(args: &ShellArgs, directory: &Path, ui: &Ui) -> Result<ExitCode> {
    let project = require_project(directory)?;
    let manifest = project.manifest();
    let name = manifest.project.name.as_str();
    let platform = Platform::detect()?;
    let paths = KilnPaths::discover()?;
    let store = kiln_cache::ContentStore::new(paths.store());

    if let Some(active) = active_project()
        && active == name
    {
        return Err(
            Error::conflict(format!("Already inside the `{active}` environment"))
                .because("Starting another shell for the same project would only nest.")
                .hint("type `exit` to leave the current one"),
        );
    }

    let state = kiln_resolver::activate(
        manifest,
        &Registry::builtin(),
        &platform,
        &store,
        Lockfile::read(&project.lockfile_path())?.as_ref(),
    );
    state.require_ready()?;

    let shell = choose_shell(args.shell.as_deref())?;

    let mut environment = Environment::from_manifest(manifest);
    for entry in state.path_entries() {
        environment.prepend_path(entry);
    }

    announce(ui, name, &shell, &state);

    let mut command = Command::new(&shell);
    command.current_dir(directory);
    environment.apply(&mut command, std::env::var_os("PATH").as_deref())?;

    let exit = kiln_exec::spawn_and_wait(command, &shell)?;

    ui.status(format!(
        "{} left the {name} environment",
        paint("[kiln]", Style::Cyan, ui.color())
    ));

    Ok(ExitCode::from(exit.process_code()))
}

/// Pick the shell to start: what was asked for, then `$SHELL`, then `/bin/sh`.
fn choose_shell(requested: Option<&Path>) -> Result<PathBuf> {
    if let Some(requested) = requested {
        if !requested.exists() {
            return Err(
                Error::not_found(format!("No such shell: {}", requested.display()))
                    .because("`--shell` must point at a program that exists"),
            );
        }
        return Ok(requested.to_path_buf());
    }

    match std::env::var_os("SHELL") {
        Some(shell) if !shell.is_empty() => Ok(PathBuf::from(shell)),
        // A missing $SHELL is normal in a container or a cron job.
        _ => Ok(PathBuf::from(FALLBACK_SHELL)),
    }
}

fn announce(ui: &Ui, project: &str, shell: &Path, state: &kiln_resolver::ProjectEnvironment) {
    let tag = paint("[kiln]", Style::Cyan, ui.color());
    ui.status(format!("{tag} {project} environment activated"));

    let width = state
        .runtimes
        .iter()
        .map(|runtime| runtime.display_name.len())
        .max()
        .unwrap_or(0);
    for runtime in &state.runtimes {
        if let Some(version) = &runtime.version {
            ui.status(format!(
                "  {:<width$}  {}",
                runtime.display_name,
                paint(&version.to_string(), Style::Dim, ui.color())
            ));
        }
    }

    ui.status(format!(
        "  {}",
        paint(
            &format!(
                "{} · type `exit` to leave · {KILN_PROJECT_ENV}={project}",
                shell.display()
            ),
            Style::Dim,
            ui.color()
        )
    ));
    ui.blank();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_shell_must_exist() {
        assert_eq!(
            choose_shell(Some(Path::new("/bin/sh"))).unwrap(),
            PathBuf::from("/bin/sh")
        );

        let error = choose_shell(Some(Path::new("/nonexistent/fish"))).unwrap_err();
        assert!(error.summary().contains("No such shell"));
    }

    #[test]
    fn the_fallback_is_a_shell_that_always_exists() {
        // `$SHELL` is unset in containers and cron jobs often enough that
        // failing there would be a real problem.
        assert!(Path::new(FALLBACK_SHELL).exists());
    }
}
