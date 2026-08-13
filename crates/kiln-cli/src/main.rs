//! The `kiln` binary.
//!
//! Everything here is glue: argument parsing, choosing a working directory,
//! dispatching to a command, and turning a [`kiln_core::Error`] into a rendered
//! report and an exit code. The behaviour lives in the library crates.

#![forbid(unsafe_code)]

mod cli;
mod commands;
mod logging;
mod prompt;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use kiln_core::Ui;
use kiln_core::error::{Error, IoResultExt, Result};

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ui = Ui::new(cli.color.into(), cli.quiet, cli.verbose);
    logging::init(cli.verbose);

    match dispatch(&cli, &ui) {
        Ok(code) => code,
        Err(error) => {
            ui.report(&error);
            ExitCode::from(error.exit_code())
        }
    }
}

/// Commands return an [`ExitCode`] rather than `()` so that `kiln run` can
/// hand back whatever the program it ran exited with. A wrapper that collapses
/// every success to `0` is useless in a Makefile or a CI step.
fn dispatch(cli: &Cli, ui: &Ui) -> Result<ExitCode> {
    let directory = base_directory(cli.directory.as_deref())?;

    match &cli.command {
        Command::Init(args) => {
            commands::init::run(args, &directory, ui).map(|()| ExitCode::SUCCESS)
        }
        Command::Install(args) => {
            commands::install::run(args, &directory, cli.offline, ui).map(|()| ExitCode::SUCCESS)
        }
        Command::Lock(args) => {
            commands::lock::run(args, &directory, cli.offline, ui).map(|()| ExitCode::SUCCESS)
        }
        Command::Doctor(args) => {
            commands::doctor::run(args, &directory, ui).map(|()| ExitCode::SUCCESS)
        }
        Command::Cache(args) => commands::cache::run(args, ui).map(|()| ExitCode::SUCCESS),
        Command::Version(args) => commands::version::run(args, ui).map(|()| ExitCode::SUCCESS),
        Command::List(args) => {
            commands::list::run(args, &directory, ui).map(|()| ExitCode::SUCCESS)
        }
        Command::Run(args) => commands::run::run(args, &directory, ui),
        Command::Shell(args) => commands::shell::run(args, &directory, ui),
        Command::Clean(_) => commands::pending::clean(&directory).map(|()| ExitCode::SUCCESS),
    }
}

/// The directory Kiln should behave as if it were started in.
///
/// `-C` is resolved without changing the process's working directory: mutating
/// global state to pass one argument makes every later path resolution depend on
/// when it happened rather than on what was asked for.
fn base_directory(requested: Option<&Path>) -> Result<PathBuf> {
    let Some(requested) = requested else {
        return std::env::current_dir()
            .io_context("Could not determine the current directory", Path::new("."));
    };

    if !requested.exists() {
        return Err(
            Error::not_found(format!("No such directory: {}", requested.display()))
                .because("`--directory` must point at a directory that exists"),
        );
    }
    if !requested.is_dir() {
        return Err(
            Error::config(format!("{} is not a directory", requested.display()))
                .because("`--directory` takes a directory, not a file"),
        );
    }

    if requested.is_absolute() {
        Ok(requested.to_path_buf())
    } else {
        let cwd = std::env::current_dir()
            .io_context("Could not determine the current directory", requested)?;
        Ok(cwd.join(requested))
    }
}
