//! The command-line surface.
//!
//! Commands scheduled for a later phase are listed in `--help` and marked, then
//! fail with an explanation naming the phase. Hiding them would make the tool
//! look smaller than its design; pretending they work would be worse.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use kiln_core::ColorChoice;

/// Define your development environment in one file.
#[derive(Debug, Parser)]
#[command(
    name = "kiln",
    version,
    about = "Define your development environment in one file.",
    long_about = "Kiln pins the runtimes and tools a project needs in kiln.toml, so that\n\
                  cloning a repository is enough to reproduce its environment.",
    disable_help_subcommand = true,
    propagate_version = true
)]
pub struct Cli {
    /// Run as if Kiln was started in this directory.
    #[arg(short = 'C', long = "directory", global = true, value_name = "PATH")]
    pub directory: Option<PathBuf>,

    /// Print more detail. Repeat for more still.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Print only errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// When to colour output.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    pub color: ColorArg,

    /// Never touch the network. Fails if something is not already installed.
    #[arg(long, global = true)]
    pub offline: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// `--color` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ColorArg {
    /// Colour when writing to a terminal.
    #[default]
    Auto,
    /// Always colour.
    Always,
    /// Never colour.
    Never,
}

impl From<ColorArg> for ColorChoice {
    fn from(value: ColorArg) -> Self {
        match value {
            ColorArg::Auto => ColorChoice::Auto,
            ColorArg::Always => ColorChoice::Always,
            ColorArg::Never => ColorChoice::Never,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a kiln.toml for this project.
    Init(InitArgs),

    /// Install the environment described by kiln.toml.
    #[command(long_about = "Install the environment described by kiln.toml.\n\n\
                            Resolves every requirement to an exact version, downloads and \
                            verifies the artifacts, and records the result in kiln.lock. \
                            Runtimes already in the store are reused, so a second project \
                            needing the same version downloads nothing.")]
    Install(InstallArgs),

    /// Resolve versions and write kiln.lock, without installing.
    #[command(
        long_about = "Resolve versions and write kiln.lock, without installing.\n\n\
                            With --all-platforms, resolves for every platform Kiln supports, \
                            so one developer can commit a lockfile that everyone else — \
                            including a CI runner on another operating system — installs \
                            from without re-resolving."
    )]
    Lock(LockArgs),

    /// Run a command inside the project environment.
    #[command(trailing_var_arg = true)]
    Run(RunArgs),

    /// Start a shell with the project environment activated.
    Shell(ShellArgs),

    /// Show the runtimes this project uses.
    List(ListArgs),

    /// Diagnose the project and this machine.
    Doctor(DoctorArgs),

    /// Inspect Kiln's artifact store.
    Cache(CacheArgs),

    /// Remove Kiln's scratch space and cached release indexes.
    Clean(CleanArgs),

    /// Show version and environment information.
    Version(VersionArgs),
}

#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Project name. Defaults to the directory name.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Pin a runtime, e.g. `--runtime node=22`. May be repeated.
    #[arg(short = 'r', long = "runtime", value_name = "NAME=REQUIREMENT")]
    pub runtimes: Vec<String>,

    /// Do not inspect the project for existing version files.
    #[arg(long)]
    pub no_detect: bool,

    /// Accept the proposed manifest without asking.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Overwrite an existing kiln.toml.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, clap::Args)]
pub struct InstallArgs {
    /// Fail rather than change kiln.lock. Use this in CI.
    #[arg(long)]
    pub locked: bool,
}

#[derive(Debug, clap::Args)]
pub struct LockArgs {
    /// Lock for this platform, e.g. `linux-x86_64-gnu`. May be repeated.
    #[arg(long = "platform", value_name = "KEY")]
    pub platforms: Vec<String>,

    /// Lock for every platform Kiln supports.
    #[arg(long, conflicts_with = "platforms")]
    pub all_platforms: bool,

    /// Report whether kiln.lock is up to date without writing it.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// The command to run, or the name of a command from `[commands]`.
    #[arg(required = true, allow_hyphen_values = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub struct ShellArgs {
    /// Shell to start. Defaults to `$SHELL`.
    #[arg(long, value_name = "PATH")]
    pub shell: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Args)]
pub struct DoctorArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// List stored artifacts.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Re-hash stored artifacts and report any that no longer match. [Phase 3]
    Verify,
    /// Remove runtimes nothing has used recently.
    #[command(long_about = "Remove runtimes nothing has used recently.\n\n\
                            Kiln keeps no registry of projects, so it cannot know which \
                            entries another project still needs. It records when each \
                            entry was last used instead, and evicts by age. Anything \
                            removed by mistake is reinstalled by `kiln install`.")]
    Clean {
        /// Remove entries unused for this many days.
        #[arg(long, value_name = "DAYS", default_value_t = 30)]
        older_than: u64,

        /// Remove every entry, however recently used.
        #[arg(long, conflicts_with = "older_than")]
        all: bool,

        /// Actually delete. Without it, Kiln only reports what it would remove.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, clap::Args)]
pub struct CleanArgs {
    /// Actually delete. Without it, Kiln only reports what it would remove.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, clap::Args)]
pub struct VersionArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_passes_flags_through_to_the_child_command() {
        let cli = Cli::try_parse_from(["kiln", "run", "node", "--version"]).unwrap();
        match cli.command {
            Command::Run(args) => assert_eq!(args.command, ["node", "--version"]),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn run_requires_something_to_run() {
        assert!(Cli::try_parse_from(["kiln", "run"]).is_err());
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = Cli::try_parse_from(["kiln", "init", "--verbose", "-C", "/tmp"]).unwrap();
        assert_eq!(cli.verbose, 1);
        assert_eq!(cli.directory.as_deref(), Some(std::path::Path::new("/tmp")));
    }

    #[test]
    fn verbosity_accumulates() {
        assert_eq!(
            Cli::try_parse_from(["kiln", "-vv", "doctor"])
                .unwrap()
                .verbose,
            2
        );
    }

    #[test]
    fn quiet_and_verbose_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["kiln", "-q", "-v", "doctor"]).is_err());
    }

    #[test]
    fn runtime_pins_accumulate() {
        let cli =
            Cli::try_parse_from(["kiln", "init", "-r", "node=22", "-r", "python=3.13"]).unwrap();
        match cli.command {
            Command::Init(args) => assert_eq!(args.runtimes, ["node=22", "python=3.13"]),
            other => panic!("expected init, got {other:?}"),
        }
    }

    #[test]
    fn locking_a_platform_accumulates() {
        let cli = Cli::try_parse_from([
            "kiln",
            "lock",
            "--platform",
            "linux-x86_64-gnu",
            "--platform",
            "macos-aarch64",
        ])
        .unwrap();
        match cli.command {
            Command::Lock(args) => {
                assert_eq!(args.platforms, ["linux-x86_64-gnu", "macos-aarch64"]);
                assert!(!args.all_platforms);
            }
            other => panic!("expected lock, got {other:?}"),
        }
    }

    #[test]
    fn explicit_platforms_and_all_platforms_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from([
                "kiln",
                "lock",
                "--all-platforms",
                "--platform",
                "macos-aarch64"
            ])
            .is_err()
        );
    }

    #[test]
    fn install_accepts_the_ci_flag() {
        let cli = Cli::try_parse_from(["kiln", "install", "--locked"]).unwrap();
        match cli.command {
            Command::Install(args) => assert!(args.locked),
            other => panic!("expected install, got {other:?}"),
        }
    }

    #[test]
    fn colour_choice_is_validated() {
        assert!(Cli::try_parse_from(["kiln", "--color", "never", "doctor"]).is_ok());
        assert!(Cli::try_parse_from(["kiln", "--color", "maybe", "doctor"]).is_err());
    }
}
