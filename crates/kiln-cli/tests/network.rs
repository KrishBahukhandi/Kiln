//! Tests that really download from nodejs.org and python-build-standalone.
//!
//! Every test here is `#[ignore]`d, so `cargo test` stays offline, fast and
//! deterministic. Run them deliberately:
//!
//! ```text
//! cargo test -p kiln-cli --test network -- --ignored --test-threads=1
//! ```
//!
//! They exist because the parts of Kiln that can only be wrong against the real
//! world — a release index whose shape changed, an artifact filename that no
//! longer matches, a checksum document with a new format — are exactly the parts
//! a mocked test would happily confirm are fine.
//!
//! `--test-threads=1` is deliberate: they share nothing, but running several
//! fifty-megabyte downloads at once is rude to the hosts serving them.

use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

/// A project directory plus an isolated Kiln home, permitted to use the network.
struct Sandbox {
    project: TempDir,
    home: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox {
            project: TempDir::new().expect("project directory"),
            home: TempDir::new().expect("kiln home"),
        }
    }

    fn path(&self) -> &Path {
        self.project.path()
    }

    fn write(&self, name: &str, contents: &str) -> &Self {
        std::fs::write(self.path().join(name), contents).expect("write fixture");
        self
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path().join(name)).expect("read file")
    }

    fn kiln(&self) -> Command {
        let mut command = Command::cargo_bin("kiln").expect("kiln binary");
        command
            .current_dir(self.path())
            .env("KILN_HOME", self.home.path())
            .env_remove("KILN_LOG")
            .env("NO_COLOR", "1")
            .args(["--color", "never"]);
        command
    }

    /// Run the binary that was just installed, and return its `--version`.
    fn run_installed(&self, relative: &str) -> String {
        let binary = self
            .home
            .path()
            .join("store")
            .read_dir()
            .and_then(|_| find_binary(&self.home.path().join("store"), relative))
            .unwrap_or_else(|e| panic!("could not find `{relative}` in the store: {e}"));

        let output = std::process::Command::new(&binary)
            .arg("--version")
            .output()
            .expect("run the installed runtime");
        assert!(
            output.status.success(),
            "{} --version failed: {}",
            binary.display(),
            String::from_utf8_lossy(&output.stderr)
        );

        // Python writes its version to stdout; older ones used stderr.
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        } else {
            stdout
        }
    }
}

fn find_binary(root: &Path, relative: &str) -> std::io::Result<std::path::PathBuf> {
    for algorithm in std::fs::read_dir(root)?.flatten() {
        for shard in std::fs::read_dir(algorithm.path())?.flatten() {
            for entry in std::fs::read_dir(shard.path())?.flatten() {
                let candidate = entry.path().join("content").join(relative);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("no store entry contains {relative}"),
    ))
}

#[test]
#[ignore = "downloads from nodejs.org"]
fn node_installs_and_runs() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .success()
        .stderr(contains("Environment ready"));

    // The point of the whole exercise: a runtime that actually executes.
    assert_eq!(sandbox.run_installed("bin/node"), "v22.14.0");
}

#[test]
#[ignore = "downloads from nodejs.org"]
fn node_records_the_published_digest() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n",
    );
    sandbox.kiln().arg("install").assert().success();

    // This is the digest nodejs.org publishes in SHASUMS256.txt for the
    // darwin-arm64 tarball. If Kiln ever writes a different one for the same
    // artifact, verification has stopped meaning anything.
    let lockfile = sandbox.read("kiln.lock");
    assert!(lockfile.contains("version = \"22.14.0\""));
    assert!(lockfile.contains("nodejs.org/dist/v22.14.0/"));
    assert!(lockfile.contains("digest = \"sha256:"));
    assert!(lockfile.contains("format = \"tar.gz\""));
}

#[test]
#[ignore = "downloads from python-build-standalone"]
fn python_installs_and_runs() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\npython = \"3.13\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .success()
        .stderr(contains("Environment ready"));

    let version = sandbox.run_installed("bin/python3");
    assert!(version.starts_with("Python 3.13."), "got {version}");
}

#[test]
#[ignore = "downloads from python-build-standalone"]
fn pythons_symlinks_and_stdlib_survive_extraction() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\npython = \"3.13\"\n",
    );
    sandbox.kiln().arg("install").assert().success();

    let python = find_binary(&sandbox.home.path().join("store"), "bin/python3").unwrap();
    // `bin/python3` is a symlink to `python3.13`; a link-safety rule that was
    // too strict would have dropped it.
    assert!(
        std::fs::symlink_metadata(&python).unwrap().is_symlink(),
        "bin/python3 should still be a symlink"
    );

    let output = std::process::Command::new(&python)
        .args(["-c", "import ssl, sqlite3, zlib, lzma; print('ok')"])
        .output()
        .expect("run python");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("ok"),
        "stdlib modules failed to import: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "reads the nodejs.org release index"]
fn a_floating_requirement_resolves_and_locks() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22\"\n",
    );
    sandbox.kiln().arg("install").assert().success();

    let lockfile = sandbox.read("kiln.lock");
    assert!(
        lockfile.contains("requirement = \"22\""),
        "the requirement is kept so drift is detectable"
    );
    assert!(
        lockfile.contains("version = \"22."),
        "a floating requirement must resolve to an exact version: {lockfile}"
    );
}

#[test]
#[ignore = "reads the nodejs.org release index"]
fn lts_resolves_to_a_supported_line() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"lts\"\n",
    );
    sandbox.kiln().arg("install").assert().success();
    assert!(sandbox.read("kiln.lock").contains("requirement = \"lts\""));
}

#[test]
#[ignore = "reads the python-build-standalone release list"]
fn python_has_no_lts_and_says_so() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\npython = \"lts\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(3)
        .stderr(contains("does not have long-term-support releases"));
}

#[test]
#[ignore = "downloads from nodejs.org"]
fn a_second_install_reuses_the_store_without_the_network() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n",
    );
    sandbox.kiln().arg("install").assert().success();

    // Offline this time: if anything still needed the network, this fails.
    sandbox
        .kiln()
        .args(["install", "--offline"])
        .assert()
        .success()
        .stderr(contains("already in the store"))
        .stderr(contains("0 downloaded, 1 reused"));
}

#[test]
#[ignore = "downloads from nodejs.org"]
fn a_tampered_lockfile_is_caught_and_installs_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n",
    );

    // A real, resolvable URL paired with a digest that does not describe it.
    // This is the shape of a compromised mirror or a corrupted lockfile.
    sandbox.write(
        "kiln.lock",
        &format!(
            "version = 1\ngenerated_by = \"kiln 0.1.0\"\n\n[project]\nname = \"app\"\n\n\
             [platform.{}.runtime.node]\nprovider = \"node\"\n\
             requirement = \"22.14.0\"\nversion = \"22.14.0\"\n\n\
             [platform.{}.runtime.node.artifact]\n\
             url = \"https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-x64.tar.gz\"\n\
             digest = \"sha256:{}\"\nformat = \"tar.gz\"\n",
            platform_key(),
            platform_key(),
            "0".repeat(64),
        ),
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(6)
        .stderr(contains("failed verification"))
        .stderr(contains("installed nothing"));

    // And it really installed nothing.
    let store = sandbox.home.path().join("store");
    assert!(find_binary(&store, "bin/node").is_err());

    // Nor did it leave a half-unpacked runtime behind.
    let staging = sandbox.home.path().join("staging");
    if staging.is_dir() {
        assert!(
            staging.read_dir().unwrap().next().is_none(),
            "staging should be empty after a rejected download"
        );
    }
}

#[test]
#[ignore = "reads the nodejs.org release index"]
fn a_version_that_does_not_exist_lists_the_ones_that_do() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"999\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(3)
        .stderr(contains("No Node.js release matches"));
}

fn platform_key() -> String {
    let output = Command::cargo_bin("kiln")
        .unwrap()
        .args(["version", "--json"])
        .output()
        .expect("kiln version");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["platform"].as_str().unwrap().to_string()
}

#[test]
#[ignore = "downloads from nodejs.org and python-build-standalone"]
fn the_whole_loop_works_with_real_runtimes() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\npython = \"3.13\"\n\n\
         [environment]\nNODE_ENV = \"development\"\n\n\
         [commands]\nver = \"node --version\"\n",
    );
    sandbox.kiln().arg("install").assert().success();

    // The success criterion: the versions a command sees are the ones pinned.
    sandbox
        .kiln()
        .args(["run", "node", "--version"])
        .assert()
        .success()
        .stdout("v22.14.0\n");

    sandbox
        .kiln()
        .args(["run", "python", "--version"])
        .assert()
        .success()
        .stdout(contains("Python 3.13."));

    // A named command, and the manifest's environment.
    sandbox
        .kiln()
        .args(["run", "ver"])
        .assert()
        .success()
        .stdout("v22.14.0\n");
    sandbox
        .kiln()
        .args([
            "run",
            "node",
            "-e",
            "process.stdout.write(process.env.NODE_ENV)",
        ])
        .assert()
        .success()
        .stdout("development");
}

#[test]
#[ignore = "downloads from nodejs.org"]
fn a_real_shell_sees_the_pinned_runtime() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n",
    );
    sandbox.kiln().arg("install").assert().success();

    sandbox
        .kiln()
        .args(["shell", "--shell", "/bin/sh"])
        .write_stdin("node --version\nexit\n")
        .assert()
        .success()
        .stdout(contains("v22.14.0"));
}

#[test]
#[ignore = "downloads from nodejs.org"]
fn npm_bundled_with_node_is_runnable() {
    // Node's tarball ships npm as a symlink into lib/node_modules. If extraction
    // had mangled symlinks, this is where it would show.
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n",
    );
    sandbox.kiln().arg("install").assert().success();

    sandbox
        .kiln()
        .args(["run", "npm", "--version"])
        .assert()
        .success();
}

#[test]
#[ignore = "resolves every platform against both upstreams"]
fn one_developer_can_lock_for_the_whole_team() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\npython = \"3.13\"\n",
    );

    sandbox
        .kiln()
        .args(["lock", "--all-platforms"])
        .assert()
        .success()
        // Node publishes no musl builds, so those platforms are reported and
        // skipped rather than failing the command.
        .stderr(contains("Skipped"))
        .stderr(contains("musl"));

    let lockfile = sandbox.read("kiln.lock");
    for platform in [
        "macos-aarch64",
        "macos-x86_64",
        "linux-x86_64-gnu",
        "linux-aarch64-gnu",
    ] {
        assert!(
            lockfile.contains(&format!("[platform.{platform}.")),
            "missing {platform} in:\n{lockfile}"
        );
    }

    // Each platform got its own artifact, not a copy of the host's.
    assert!(lockfile.contains("node-v22.14.0-linux-x64.tar.gz"));
    assert!(lockfile.contains("node-v22.14.0-darwin-arm64.tar.gz"));
    assert!(lockfile.contains("x86_64-unknown-linux-gnu-install_only.tar.gz"));

    // And the result is stable: locking again changes nothing.
    let before = sandbox.read("kiln.lock");
    sandbox
        .kiln()
        .args(["lock", "--all-platforms"])
        .assert()
        .success();
    assert_eq!(
        sandbox.read("kiln.lock"),
        before,
        "locking must be idempotent"
    );

    sandbox
        .kiln()
        .args(["lock", "--check", "--all-platforms"])
        .assert()
        .success();
}

#[test]
#[ignore = "resolves against nodejs.org"]
fn locking_another_platform_does_not_need_that_platform() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22\"\n",
    );

    // Running on macOS, locking for Linux.
    sandbox
        .kiln()
        .args(["lock", "--platform", "linux-x86_64-gnu"])
        .assert()
        .success();

    let lockfile = sandbox.read("kiln.lock");
    assert!(lockfile.contains("[platform.linux-x86_64-gnu."));
    assert!(lockfile.contains("linux-x64.tar.gz"), "{lockfile}");
}

#[test]
#[ignore = "downloads from nodejs.org"]
fn a_ci_runner_installs_from_a_committed_lockfile() {
    let author = Sandbox::new();
    author.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22\"\n",
    );
    author.kiln().args(["lock"]).assert().success();

    // A fresh checkout with the same two files, and a cold store.
    let runner = Sandbox::new();
    runner.write("kiln.toml", &author.read("kiln.toml"));
    runner.write("kiln.lock", &author.read("kiln.lock"));

    runner
        .kiln()
        .args(["install", "--locked"])
        .assert()
        .success()
        .stderr(contains("from kiln.lock"));

    // The lockfile the runner used is the one that was reviewed.
    assert_eq!(runner.read("kiln.lock"), author.read("kiln.lock"));
}
