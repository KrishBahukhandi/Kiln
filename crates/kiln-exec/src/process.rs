//! Running a program inside the project environment.
//!
//! # Why Kiln resolves the program itself
//!
//! `Command::new("node").env("PATH", …)` does **not** reliably use that `PATH`
//! to find `node`. Rust's documentation is explicit that the interaction is
//! platform-specific, and on some targets the lookup happens against the
//! *parent's* `PATH` — which would mean `kiln run node` silently executing the
//! system Node.js while claiming to run the project's.
//!
//! That is the single guarantee this crate exists to make, so it is not left to
//! unspecified behaviour. Kiln searches the composed `PATH` itself, passes an
//! absolute path to the child, and gets a much better error message out of it as
//! a side effect: it can say *which* runtimes the project does have.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use kiln_core::error::{Error, ErrorKind, IoResultExt, Result};

use crate::env::Environment;

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// The exit status, if it exited normally.
    pub code: Option<i32>,
    /// The signal that killed it, if one did.
    pub signal: Option<i32>,
}

impl Exit {
    /// The code Kiln should exit with to mirror the child.
    ///
    /// A process killed by a signal conventionally reports `128 + signal`, which
    /// is what a shell would have reported had it run the program directly.
    /// `kiln run` has to be transparent to whatever is reading its exit code.
    pub fn process_code(&self) -> u8 {
        match (self.code, self.signal) {
            (Some(code), _) => u8::try_from(code & 0xff).unwrap_or(1),
            (None, Some(signal)) => u8::try_from((128 + signal) & 0xff).unwrap_or(1),
            (None, None) => 1,
        }
    }

    /// Whether the child succeeded.
    pub fn is_success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run `program` with `args` inside `environment`.
///
/// Standard input, output and error are inherited, so the child is
/// indistinguishable from one the user started themselves — pipes, pagers and
/// interactive prompts all behave normally.
pub fn run(
    program: &str,
    args: &[String],
    environment: &Environment,
    working_directory: &Path,
) -> Result<Exit> {
    let inherited = std::env::var_os("PATH");
    let path = environment.compose_path(inherited.as_deref())?;
    let resolved = resolve_program(program, &path, working_directory)
        .ok_or_else(|| not_found(program, environment, &path))?;

    let mut command = Command::new(&resolved);
    command.args(args).current_dir(working_directory);
    environment.apply(&mut command, inherited.as_deref())?;

    spawn_and_wait(command, &resolved)
}

/// Spawn a prepared command and wait for it, surviving Ctrl+C.
///
/// While the child runs, Kiln stops treating `SIGINT` as fatal. The terminal
/// sends it to the whole foreground process group, so the child receives its own
/// copy and decides what to do; if Kiln died on the same signal, the shell prompt
/// would come back while the child was still shutting down, and the child's exit
/// status would be lost.
pub fn spawn_and_wait(mut command: Command, program: &Path) -> Result<Exit> {
    let _guard = interrupt::Guard::install();

    let mut child = command.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => Error::new(
            ErrorKind::Io,
            format!("`{}` is not executable", program.display()),
        )
        .because(format!("{}: {e}", program.display())),
        _ => Error::io("Could not start the program", program, e),
    })?;

    let status = child
        .wait()
        .io_context("Could not wait for the program to finish", program)?;

    Ok(Exit {
        code: status.code(),
        signal: exit_signal(&status),
    })
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Find `program` on `path`, the way a shell would.
///
/// A name containing a separator is taken as a path relative to the working
/// directory and not searched for, matching every shell's behaviour: `./build`
/// means that file, not something on `PATH`.
pub fn resolve_program(program: &str, path: &OsStr, working_directory: &Path) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }

    if program.contains('/') || (cfg!(windows) && program.contains('\\')) {
        let candidate = if Path::new(program).is_absolute() {
            PathBuf::from(program)
        } else {
            working_directory.join(program)
        };
        return is_executable(&candidate).then_some(candidate);
    }

    std::env::split_paths(path)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
}

/// Whether a path names a file this user can execute.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    // `metadata` follows symlinks, which is what we want: `bin/python3` is one.
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Explain that a program is not in the project environment, and say what is.
fn not_found(program: &str, environment: &Environment, path: &OsStr) -> Error {
    let mut error = Error::not_found(format!("`{program}` is not available in this environment"))
        .because("Kiln searched the project's runtimes and then the rest of your PATH.");

    // Listing what the project *does* provide turns "not found" into a
    // diagnosis: usually the answer is that the manifest pins Python and the
    // command wanted Node, or that a package manager was never installed.
    let mut available: Vec<String> = Vec::new();
    for directory in environment.path_entries() {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                if is_executable(&entry.path())
                    && let Some(name) = entry.file_name().to_str()
                {
                    available.push(name.to_string());
                }
            }
        }
    }
    available.sort();
    available.dedup();

    if !available.is_empty() {
        let shown: Vec<String> = available.iter().take(12).cloned().collect();
        error = error.expected(format!(
            "this project provides:\n  {}{}",
            shown.join(", "),
            if available.len() > shown.len() {
                format!(", and {} more", available.len() - shown.len())
            } else {
                String::new()
            }
        ));
    }

    if std::env::split_paths(path).count() <= environment.path_entries().len() {
        error = error.hint("your PATH looks empty; is the environment set up?");
    }
    error.command("kiln list")
}

/// Keeping Ctrl+C from killing Kiln while a child is running.
mod interrupt {
    /// Restores the previous signal disposition when dropped.
    ///
    /// On platforms without signals this is inert, so callers need no `cfg`.
    pub struct Guard {
        #[cfg(unix)]
        registered: Option<signal_hook::SigId>,
    }

    impl Guard {
        #[cfg(unix)]
        pub fn install() -> Self {
            use std::sync::Arc;
            use std::sync::atomic::AtomicBool;

            // Registering *any* handler replaces the default "terminate"
            // disposition, which is the whole point — the flag itself is not
            // read. `signal_hook::flag` is used because it is the safe API;
            // installing a raw handler would need `unsafe`.
            let flag = Arc::new(AtomicBool::new(false));
            let registered = signal_hook::flag::register(signal_hook::consts::SIGINT, flag).ok();
            Guard { registered }
        }

        #[cfg(not(unix))]
        pub fn install() -> Self {
            Guard {}
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            #[cfg(unix)]
            if let Some(id) = self.registered.take() {
                signal_hook::low_level::unregister(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A directory holding a fake executable and a fake data file.
    struct Bin {
        directory: tempfile::TempDir,
    }

    impl Bin {
        fn new() -> Self {
            Bin {
                directory: tempfile::tempdir().unwrap(),
            }
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }

        fn executable(&self, name: &str, script: &str) -> PathBuf {
            let path = self.directory.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        }

        fn data_file(&self, name: &str) -> PathBuf {
            let path = self.directory.path().join(name);
            std::fs::write(&path, "not a program").unwrap();
            path
        }
    }

    fn path_of(directories: &[&Path]) -> OsString {
        std::env::join_paths(directories).unwrap()
    }

    #[test]
    fn resolves_a_program_from_the_path() {
        let bin = Bin::new();
        let node = bin.executable("node", "echo hi");

        let found = resolve_program("node", &path_of(&[bin.path()]), Path::new("/"));
        assert_eq!(found.as_deref(), Some(node.as_path()));
    }

    #[test]
    fn the_first_matching_directory_wins() {
        // This is the guarantee the whole crate exists for: a project's runtime
        // must shadow the system one.
        let store = Bin::new();
        let system = Bin::new();
        let pinned = store.executable("node", "echo project");
        system.executable("node", "echo system");

        let found = resolve_program(
            "node",
            &path_of(&[store.path(), system.path()]),
            Path::new("/"),
        );
        assert_eq!(found.as_deref(), Some(pinned.as_path()));
    }

    #[test]
    fn a_non_executable_file_is_skipped_not_selected() {
        let shadow = Bin::new();
        let real = Bin::new();
        shadow.data_file("node");
        let node = real.executable("node", "echo hi");

        let found = resolve_program(
            "node",
            &path_of(&[shadow.path(), real.path()]),
            Path::new("/"),
        );
        assert_eq!(found.as_deref(), Some(node.as_path()));
    }

    #[test]
    fn a_directory_named_like_the_program_is_not_a_program() {
        let bin = Bin::new();
        std::fs::create_dir(bin.path().join("node")).unwrap();
        assert!(resolve_program("node", &path_of(&[bin.path()]), Path::new("/")).is_none());
    }

    #[test]
    fn a_missing_program_resolves_to_nothing() {
        let bin = Bin::new();
        assert!(resolve_program("nope", &path_of(&[bin.path()]), Path::new("/")).is_none());
        assert!(resolve_program("", &path_of(&[bin.path()]), Path::new("/")).is_none());
    }

    #[test]
    fn a_path_bearing_name_is_taken_literally() {
        let bin = Bin::new();
        let script = bin.executable("build.sh", "echo built");

        // `./build.sh` means that file, not something on PATH.
        let found = resolve_program("./build.sh", &OsString::new(), bin.path());
        assert_eq!(
            found.as_deref(),
            Some(bin.path().join("build.sh").as_path())
        );

        let absolute = resolve_program(script.to_str().unwrap(), &OsString::new(), Path::new("/"));
        assert_eq!(absolute.as_deref(), Some(script.as_path()));
    }

    #[test]
    fn a_relative_path_that_does_not_exist_is_not_searched_for_on_path() {
        let bin = Bin::new();
        bin.executable("build.sh", "echo built");
        // `./build.sh` relative to somewhere else must not find it on PATH.
        assert!(
            resolve_program("./build.sh", &path_of(&[bin.path()]), Path::new("/tmp")).is_none()
        );
    }

    #[test]
    fn empty_path_entries_are_ignored() {
        let bin = Bin::new();
        bin.executable("node", "echo hi");
        // A trailing `:` means "the current directory" to a shell; Kiln drops it.
        let path = OsString::from(format!(":{}:", bin.path().display()));
        assert!(resolve_program("node", &path, Path::new("/")).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_program_runs_and_its_exit_code_is_reported() {
        let bin = Bin::new();
        bin.executable("succeed", "exit 0");
        bin.executable("fail", "exit 42");

        let mut environment = Environment::new();
        environment.prepend_path(bin.path());

        let ok = run("succeed", &[], &environment, bin.path()).unwrap();
        assert!(ok.is_success());
        assert_eq!(ok.process_code(), 0);

        let bad = run("fail", &[], &environment, bin.path()).unwrap();
        assert!(!bad.is_success());
        assert_eq!(bad.process_code(), 42);
    }

    #[cfg(unix)]
    #[test]
    fn the_project_environment_reaches_the_child() {
        let bin = Bin::new();
        let output = bin.directory.path().join("out");
        bin.executable(
            "show",
            &format!("printf '%s' \"$NODE_ENV\" > {}", output.display()),
        );

        let mut environment = Environment::new();
        environment.prepend_path(bin.path()).set("NODE_ENV", "test");

        assert!(
            run("show", &[], &environment, bin.path())
                .unwrap()
                .is_success()
        );
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "test");
    }

    #[cfg(unix)]
    #[test]
    fn arguments_are_passed_through_untouched() {
        let bin = Bin::new();
        let output = bin.directory.path().join("args");
        bin.executable(
            "echoargs",
            &format!("printf '%s|' \"$@\" > {}", output.display()),
        );

        let mut environment = Environment::new();
        environment.prepend_path(bin.path());

        let args: Vec<String> = ["--flag", "a b", "$HOME", "&&"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        run("echoargs", &args, &environment, bin.path()).unwrap();

        // Nothing was expanded, split or interpreted on the way through.
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "--flag|a b|$HOME|&&|"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_signalled_child_reports_the_conventional_code() {
        let bin = Bin::new();
        bin.executable("suicide", "kill -TERM $$");

        let mut environment = Environment::new();
        environment.prepend_path(bin.path());

        let exit = run("suicide", &[], &environment, bin.path()).unwrap();
        assert_eq!(exit.code, None);
        assert_eq!(exit.signal, Some(15));
        assert_eq!(exit.process_code(), 143, "128 + SIGTERM");
    }

    #[test]
    fn exit_codes_map_the_way_a_shell_reports_them() {
        assert_eq!(
            Exit {
                code: Some(0),
                signal: None
            }
            .process_code(),
            0
        );
        assert_eq!(
            Exit {
                code: Some(1),
                signal: None
            }
            .process_code(),
            1
        );
        assert_eq!(
            Exit {
                code: Some(42),
                signal: None
            }
            .process_code(),
            42
        );
        assert_eq!(
            Exit {
                code: None,
                signal: Some(2)
            }
            .process_code(),
            130
        );
        assert_eq!(
            Exit {
                code: None,
                signal: Some(9)
            }
            .process_code(),
            137
        );
        assert_eq!(
            Exit {
                code: None,
                signal: None
            }
            .process_code(),
            1
        );
    }

    #[test]
    fn a_missing_program_says_what_the_project_does_provide() {
        let bin = Bin::new();
        bin.executable("node", "true");
        bin.executable("npm", "true");

        let mut environment = Environment::new();
        environment.prepend_path(bin.path());

        // The name has to be one no machine can have. `prepend_path` prepends
        // to the *inherited* `PATH` — that is the whole design — so a real
        // program name only proves absence on a host that happens not to have
        // it. This test previously asked for `python`, which macOS has not
        // shipped for years and every CI runner provides, so it passed locally
        // and failed on Linux by finding the system one and succeeding.
        const ABSENT: &str = "kiln-test-program-that-does-not-exist";

        let error = run(ABSENT, &[], &environment, bin.path()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(
            error
                .summary()
                .contains(&format!("`{ABSENT}` is not available"))
        );

        let expected = error.expectation().expect("a list of what is available");
        assert!(expected.contains("node"), "{expected}");
        assert!(expected.contains("npm"), "{expected}");
    }

    #[cfg(unix)]
    #[test]
    fn the_interrupt_guard_restores_what_it_replaced() {
        // Installing and dropping repeatedly must not leak registrations.
        for _ in 0..3 {
            let guard = interrupt::Guard::install();
            drop(guard);
        }
    }
}
