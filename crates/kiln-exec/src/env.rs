//! Composing the environment a project's commands run in.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use kiln_config::Manifest;
use kiln_core::error::{Error, Result};

/// Marks a shell or process as running inside a Kiln environment.
///
/// Set so that `kiln shell` can refuse to nest, and so a prompt or a status line
/// can show which project is active.
pub const KILN_PROJECT_ENV: &str = "KILN_PROJECT";

/// The environment a project's processes see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    path_entries: Vec<PathBuf>,
    variables: BTreeMap<String, String>,
}

impl Environment {
    /// An environment that changes nothing.
    pub fn new() -> Self {
        Environment::default()
    }

    /// Start from a manifest's `[environment]` table.
    ///
    /// Only the variables: the `PATH` entries come from resolved runtimes, which
    /// the manifest describes but does not contain.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut environment = Environment::new();
        for (name, value) in &manifest.environment {
            environment.set(name.as_str(), value);
        }
        environment.set(KILN_PROJECT_ENV, manifest.project.name.as_str());
        environment
    }

    /// Add a directory to the front of `PATH`.
    ///
    /// Order matters and is preserved: the first runtime added wins, which is
    /// how `[runtime]` ordering becomes a resolution rule for `node` versus a
    /// `node` shipped inside some other tool.
    pub fn prepend_path(&mut self, directory: impl Into<PathBuf>) -> &mut Self {
        self.path_entries.push(directory.into());
        self
    }

    /// Set a variable.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.variables.insert(name.into(), value.into());
        self
    }

    /// The directories this environment puts in front of `PATH`.
    pub fn path_entries(&self) -> &[PathBuf] {
        &self.path_entries
    }

    /// The variables this environment sets, in a deterministic order.
    pub fn variables(&self) -> impl Iterator<Item = (&str, &str)> {
        self.variables.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Build the `PATH` a child process should see.
    ///
    /// Kiln's directories go first, then whatever the user already had.
    /// Duplicates are dropped, keeping the earliest occurrence, so a directory
    /// that is already on the inherited `PATH` still ends up in front rather
    /// than appearing twice and resolving to the system copy.
    ///
    /// The inherited `PATH` is preserved rather than replaced: a developer's
    /// `git`, `ssh` and editor must keep working inside `kiln shell`. Kiln
    /// shadows what a project pins; it does not take the machine away.
    pub fn compose_path(&self, inherited: Option<&OsStr>) -> Result<OsString> {
        let mut ordered: Vec<PathBuf> = Vec::new();
        let mut seen: Vec<PathBuf> = Vec::new();

        let inherited_entries = inherited
            .map(|value| std::env::split_paths(value).collect::<Vec<_>>())
            .unwrap_or_default();

        for entry in self.path_entries.iter().cloned().chain(inherited_entries) {
            // An empty entry means "the current directory" to most shells, which
            // is a surprise nobody asked for; drop it.
            if entry.as_os_str().is_empty() || seen.contains(&entry) {
                continue;
            }
            seen.push(entry.clone());
            ordered.push(entry);
        }

        std::env::join_paths(&ordered).map_err(|e| {
            Error::internal("Could not build PATH for the project environment")
                .because("one of the directories contains the path separator")
                .with_source(e)
        })
    }

    /// Apply this environment to a command that is about to be spawned.
    ///
    /// The parent's environment is inherited rather than cleared. A cleared
    /// environment breaks `HOME`, `TERM`, `SSH_AUTH_SOCK` and every credential
    /// helper a developer relies on, in exchange for an isolation guarantee Kiln
    /// does not actually claim to make.
    pub fn apply(&self, command: &mut Command, inherited_path: Option<&OsStr>) -> Result<()> {
        command.env("PATH", self.compose_path(inherited_path)?);
        for (name, value) in &self.variables {
            command.env(name, value);
        }
        Ok(())
    }

    /// The `KEY=value` lines a shell would need to `export` to enter this
    /// environment. Used for diagnostics and for shell integration.
    pub fn describe(&self, inherited_path: Option<&OsStr>) -> Result<Vec<String>> {
        let mut lines = vec![format!(
            "PATH={}",
            self.compose_path(inherited_path)?.to_string_lossy()
        )];
        lines.extend(
            self.variables
                .iter()
                .map(|(name, value)| format!("{name}={value}")),
        );
        Ok(lines)
    }
}

/// Whether a process is already running inside a Kiln environment.
pub fn active_project() -> Option<String> {
    std::env::var(KILN_PROJECT_ENV)
        .ok()
        .filter(|v| !v.is_empty())
}

/// Join path entries for display, using the platform separator.
pub fn display_path(entries: &[PathBuf]) -> String {
    entries
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(if cfg!(windows) { ";" } else { ":" })
}

/// Whether `directory` is already on the given `PATH`.
pub fn path_contains(path: Option<&OsStr>, directory: &Path) -> bool {
    path.map(|value| std::env::split_paths(value).any(|entry| entry == directory))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn path_of(value: &OsStr) -> Vec<PathBuf> {
        std::env::split_paths(value).collect()
    }

    fn inherited(entries: &[&str]) -> OsString {
        std::env::join_paths(entries.iter().map(Path::new)).unwrap()
    }

    #[test]
    fn kiln_directories_come_first() {
        let mut environment = Environment::new();
        environment
            .prepend_path("/store/node/bin")
            .prepend_path("/store/python/bin");

        let system = inherited(&["/usr/local/bin", "/usr/bin", "/bin"]);
        let composed = environment.compose_path(Some(&system)).unwrap();

        assert_eq!(
            path_of(&composed),
            [
                Path::new("/store/node/bin"),
                Path::new("/store/python/bin"),
                Path::new("/usr/local/bin"),
                Path::new("/usr/bin"),
                Path::new("/bin"),
            ]
        );
    }

    #[test]
    fn insertion_order_is_preserved() {
        let mut environment = Environment::new();
        environment
            .prepend_path("/a")
            .prepend_path("/b")
            .prepend_path("/c");
        let composed = environment.compose_path(None).unwrap();
        assert_eq!(
            path_of(&composed),
            [Path::new("/a"), Path::new("/b"), Path::new("/c")]
        );
    }

    #[test]
    fn a_directory_already_on_path_is_promoted_not_duplicated() {
        let mut environment = Environment::new();
        environment.prepend_path("/usr/bin");

        let system = inherited(&["/usr/local/bin", "/usr/bin"]);
        let composed = environment.compose_path(Some(&system)).unwrap();

        assert_eq!(
            path_of(&composed),
            [Path::new("/usr/bin"), Path::new("/usr/local/bin")]
        );
    }

    #[test]
    fn duplicates_within_the_inherited_path_are_collapsed() {
        let system = inherited(&["/usr/bin", "/usr/local/bin", "/usr/bin"]);
        let composed = Environment::new().compose_path(Some(&system)).unwrap();
        assert_eq!(
            path_of(&composed),
            [Path::new("/usr/bin"), Path::new("/usr/local/bin")]
        );
    }

    #[test]
    fn the_system_path_survives() {
        // Kiln shadows what a project pins; it must not remove the user's tools.
        let system = inherited(&["/usr/local/bin", "/opt/homebrew/bin"]);
        let mut environment = Environment::new();
        environment.prepend_path("/store/node/bin");

        let composed = environment.compose_path(Some(&system)).unwrap();
        let entries = path_of(&composed);
        assert!(entries.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(entries.contains(&PathBuf::from("/usr/local/bin")));
    }

    #[test]
    fn an_absent_path_yields_only_kilns_entries() {
        let mut environment = Environment::new();
        environment.prepend_path("/store/node/bin");
        assert_eq!(
            path_of(&environment.compose_path(None).unwrap()),
            [Path::new("/store/node/bin")]
        );
    }

    #[test]
    fn empty_path_entries_are_dropped() {
        // A trailing `:` in PATH means "the current directory" to most shells.
        let system = OsString::from("/usr/bin::/bin:");
        let composed = Environment::new().compose_path(Some(&system)).unwrap();
        assert_eq!(
            path_of(&composed),
            [Path::new("/usr/bin"), Path::new("/bin")]
        );
    }

    #[test]
    fn an_empty_environment_changes_nothing() {
        let system = inherited(&["/usr/bin", "/bin"]);
        let composed = Environment::new().compose_path(Some(&system)).unwrap();
        assert_eq!(composed, system);
    }

    #[test]
    fn manifest_variables_are_carried_over() {
        let manifest = kiln_config::parse_str(
            r#"
[project]
name = "example-app"

[runtime]
node = "22"

[environment]
NODE_ENV = "development"
API_URL = "http://localhost:3000"
"#,
            Path::new("kiln.toml"),
        )
        .unwrap();

        let environment = Environment::from_manifest(&manifest);
        let variables: BTreeMap<&str, &str> = environment.variables().collect();

        assert_eq!(variables["NODE_ENV"], "development");
        assert_eq!(variables["API_URL"], "http://localhost:3000");
        assert_eq!(variables[KILN_PROJECT_ENV], "example-app");
    }

    #[test]
    fn applying_to_a_command_sets_path_and_variables() {
        let mut environment = Environment::new();
        environment
            .prepend_path("/store/node/bin")
            .set("NODE_ENV", "test");

        let mut command = Command::new("true");
        let system = inherited(&["/usr/bin"]);
        environment.apply(&mut command, Some(&system)).unwrap();

        let applied: BTreeMap<String, String> = command
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();

        assert_eq!(applied["NODE_ENV"], "test");
        assert_eq!(applied["PATH"], "/store/node/bin:/usr/bin");
    }

    #[test]
    fn describe_lists_path_first_then_variables() {
        let mut environment = Environment::new();
        environment
            .prepend_path("/store/node/bin")
            .set("NODE_ENV", "test");

        let lines = environment
            .describe(Some(&inherited(&["/usr/bin"])))
            .unwrap();
        assert_eq!(lines[0], "PATH=/store/node/bin:/usr/bin");
        assert_eq!(lines[1], "NODE_ENV=test");
    }

    #[test]
    fn path_membership_is_detectable() {
        let system = inherited(&["/usr/bin", "/bin"]);
        assert!(path_contains(Some(&system), Path::new("/usr/bin")));
        assert!(!path_contains(Some(&system), Path::new("/store/node/bin")));
        assert!(!path_contains(None, Path::new("/usr/bin")));
    }

    #[test]
    fn display_path_uses_the_platform_separator() {
        let entries = [PathBuf::from("/a"), PathBuf::from("/b")];
        let expected = if cfg!(windows) { "/a;/b" } else { "/a:/b" };
        assert_eq!(display_path(&entries), expected);
    }
}
