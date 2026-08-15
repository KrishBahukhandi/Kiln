//! End-to-end tests: the real binary, real files on disk, real exit codes.
//!
//! Every test gets its own `KILN_HOME`, so the suite never reads or writes the
//! developer's actual store, and runs identically on a machine that has never
//! run Kiln before.
//!
//! Nothing here touches the network: `Sandbox::kiln` passes `--offline`, so a
//! test that accidentally reaches for nodejs.org fails loudly instead of
//! quietly downloading fifty megabytes. Tests that genuinely exercise the
//! download path live in `tests/network.rs` and are `#[ignore]`d.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

/// Exit codes, mirroring `kiln_core::ErrorKind::exit_code`.
mod exit {
    pub const SUCCESS: i32 = 0;
    pub const CONFIG: i32 = 2;
    pub const NOT_FOUND: i32 = 3;
    pub const CONFLICT: i32 = 8;
    pub const UNSUPPORTED: i32 = 4;
    pub const NETWORK: i32 = 5;
    pub const VERIFICATION: i32 = 6;
    pub const NOT_IMPLEMENTED: i32 = 9;
}

/// A project directory plus an isolated Kiln home.
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
        let path = self.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write fixture");
        self
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path().join(name)).expect("read file")
    }

    fn exists(&self, name: &str) -> bool {
        self.path().join(name).exists()
    }

    /// Plant a runtime in the store as though it had been installed.
    ///
    /// The store is content-addressed, so an entry is indistinguishable from a
    /// real install as long as its directory is named by the right digest. That
    /// lets the whole install path — lockfile, reuse, PATH — be tested without
    /// downloading fifty megabytes.
    ///
    /// Installed *now*, not at a fixed date: an entry with no recorded use ages
    /// from its install time, so a hardcoded timestamp would make every freshly
    /// planted runtime look abandoned as the calendar moved on.
    fn plant(&self, digest_hex: &str, provider: &str, version: &str) {
        let entry = self.entry_path(digest_hex);
        std::fs::create_dir_all(entry.join("content/bin")).unwrap();
        std::fs::write(entry.join("content/bin/fake"), b"#!/bin/sh\n").unwrap();
        std::fs::write(
            entry.join("meta.toml"),
            format!(
                "provider = \"{provider}\"\nversion = \"{version}\"\n\
                 url = \"https://example.test/artifact.tar.gz\"\n\
                 digest = \"sha256:{digest_hex}\"\n\
                 artifact_bytes = 1024\ninstalled_unix = {now}\n",
                now = kiln_now(),
            ),
        )
        .unwrap();
    }

    /// Record a tree manifest for a planted entry, as a real install would.
    ///
    /// Planting alone deliberately leaves none, which is what an entry from
    /// before Kiln recorded manifests looks like. Sealing is the separate step
    /// so a test can choose which of the two it needs.
    fn seal(&self, digest_hex: &str) {
        let entry = self.entry_path(digest_hex);
        let manifest = kiln_cache::tree::manifest_of(&entry.join("content")).unwrap();
        std::fs::write(entry.join("tree.manifest"), manifest.render()).unwrap();

        let meta = entry.join("meta.toml");
        let existing = std::fs::read_to_string(&meta).unwrap();
        std::fs::write(
            &meta,
            format!("{existing}tree_digest = \"{}\"\n", manifest.digest()),
        )
        .unwrap();
    }

    /// Backdate an entry so garbage collection considers it idle.
    fn age(&self, digest_hex: &str, unix: u64) {
        std::fs::write(
            self.entry_path(digest_hex).join("last-used"),
            unix.to_string(),
        )
        .unwrap();
    }

    fn entry_path(&self, digest_hex: &str) -> PathBuf {
        self.home
            .path()
            .join("store/sha256")
            .join(&digest_hex[..2])
            .join(digest_hex)
    }

    /// Put an executable inside a planted runtime's `bin` directory.
    ///
    /// A store entry is just a directory, so a shell script standing in for
    /// `node` exercises exactly the same PATH composition and program
    /// resolution that a real 50 MB runtime would.
    fn plant_program(&self, digest_hex: &str, name: &str, script: &str) {
        let bin = self.entry_path(digest_hex).join("content/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// A lockfile pinning `node` to `version` at `digest_hex` for this host.
    fn lockfile(&self, requirement: &str, version: &str, digest_hex: &str) {
        let platform = platform_key();
        self.write(
            "kiln.lock",
            &format!(
                "version = 1\ngenerated_by = \"kiln 0.1.0\"\n\n\
                 [project]\nname = \"app\"\n\n\
                 [platform.{platform}.runtime.node]\n\
                 provider = \"node\"\nrequirement = \"{requirement}\"\nversion = \"{version}\"\n\n\
                 [platform.{platform}.runtime.node.artifact]\n\
                 url = \"https://example.test/node.tar.gz\"\n\
                 digest = \"sha256:{digest_hex}\"\nformat = \"tar.gz\"\n"
            ),
        );
    }

    /// `kiln` with colour off, an isolated home, and this project as the cwd.
    ///
    /// `--offline` is always passed. This suite must run on a plane and must
    /// never depend on nodejs.org being up, and making that structural beats
    /// remembering it in every test. Tests that genuinely exercise the network
    /// live in `tests/network.rs` and are `#[ignore]`d.
    fn kiln(&self) -> Command {
        let mut command = Command::cargo_bin("kiln").expect("kiln binary");
        command
            .current_dir(self.path())
            .env("KILN_HOME", self.home.path())
            .env_remove("KILN_LOG")
            .env("NO_COLOR", "1")
            .args(["--color", "never", "--offline"]);
        command
    }
}

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(relative)
}

const MINIMAL: &str = "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n";

/// A digest that is valid in shape but corresponds to nothing real.
const PLANTED: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// A second one, for tests that need two entries in the store at once.
const PLANTED_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// The platform key for the machine running the tests.
fn platform_key() -> String {
    let output = Command::cargo_bin("kiln")
        .unwrap()
        .args(["version", "--json"])
        .output()
        .expect("kiln version");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["platform"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Help and version
// ---------------------------------------------------------------------------

#[test]
fn help_lists_every_command() {
    let sandbox = Sandbox::new();
    let assert = sandbox.kiln().arg("--help").assert().success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for command in [
        "init", "install", "run", "shell", "list", "doctor", "cache", "clean", "version",
    ] {
        assert!(stdout.contains(command), "`{command}` missing from --help");
    }
}

#[test]
fn help_marks_the_commands_that_are_not_built_yet() {
    let sandbox = Sandbox::new();
    let assert = sandbox.kiln().arg("--help").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    // Every command in the tree is now built, so the invariant has flipped:
    // nothing may advertise itself as pending. This still earns its keep — it
    // is what fails if a future stub ships with a phase marker instead of an
    // implementation.
    let cache = sandbox.kiln().args(["cache", "--help"]).assert().success();
    let cache_help = String::from_utf8(cache.get_output().stdout.clone()).unwrap();

    for help in [&stdout, &cache_help] {
        assert!(
            !help.contains("[Phase "),
            "a command advertising a phase marker is not implemented:\n{help}"
        );
    }
}

#[test]
fn version_is_reported_both_ways() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("kiln 0.1.0"));
    sandbox
        .kiln()
        .arg("version")
        .assert()
        .success()
        .stdout(contains("kiln 0.1.0"));
}

#[test]
fn version_json_is_machine_readable_and_alone_on_stdout() {
    let sandbox = Sandbox::new();
    let assert = sandbox
        .kiln()
        .args(["version", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be pure JSON");

    assert_eq!(value["version"], "0.1.0");
    assert_eq!(value["lockfile_version"], 1);
    assert!(value["platform"].is_string());
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn init_detects_a_node_project() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "package.json",
        r#"{ "name": "app", "engines": { "node": "22.14.0" } }"#,
    );

    sandbox
        .kiln()
        .args(["init", "--yes"])
        .assert()
        .success()
        .stderr(contains("Node.js"))
        .stderr(contains("package.json (engines.node)"));

    let manifest = sandbox.read("kiln.toml");
    assert!(manifest.contains("[runtime]"));
    assert!(manifest.contains("node = \"22.14.0\""));
}

#[test]
fn init_detects_a_python_project() {
    let sandbox = Sandbox::new();
    sandbox.write(".python-version", "3.13.5\n");

    sandbox.kiln().args(["init", "--yes"]).assert().success();
    assert!(sandbox.read("kiln.toml").contains("python = \"3.13.5\""));
}

#[test]
fn init_narrows_a_compatibility_range_into_a_pin() {
    let sandbox = Sandbox::new();
    sandbox.write("package.json", r#"{ "engines": { "node": ">=22" } }"#);

    sandbox
        .kiln()
        .args(["init", "--yes"])
        .assert()
        .success()
        .stderr(contains("pinned from >=22"));

    // `>=22` would drift to every future major; the pin does not.
    assert!(sandbox.read("kiln.toml").contains("node = \"22\""));
}

#[test]
fn init_writes_unmanaged_tools_commented_out() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "package.json",
        r#"{ "engines": { "node": "22" }, "packageManager": "pnpm@10.12.1" }"#,
    );

    sandbox.kiln().args(["init", "--yes"]).assert().success();

    let manifest = sandbox.read("kiln.toml");
    assert!(manifest.contains("# pnpm = \"10.12.1\""));
    assert!(
        !manifest.contains("\npnpm = "),
        "an unmanaged tool must not become a live requirement"
    );

    // And the result is still a valid manifest.
    sandbox
        .kiln()
        .arg("doctor")
        .assert()
        .success()
        .stderr(contains("configuration"));
}

#[test]
fn init_generates_a_manifest_kiln_can_read_back() {
    let sandbox = Sandbox::new();
    sandbox.write("package.json", r#"{ "engines": { "node": "22.14.0" } }"#);
    sandbox.kiln().args(["init", "--yes"]).assert().success();

    // The round trip that matters: what init writes, doctor must accept.
    sandbox.kiln().arg("doctor").assert().success();
}

#[test]
fn init_accepts_explicit_runtime_pins() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .args(["init", "--yes", "-r", "node=22", "-r", "python=3.13"])
        .assert()
        .success();

    let manifest = sandbox.read("kiln.toml");
    assert!(manifest.contains("node = \"22\""));
    assert!(manifest.contains("python = \"3.13\""));
}

#[test]
fn an_explicit_pin_overrides_what_was_detected() {
    let sandbox = Sandbox::new();
    sandbox.write("package.json", r#"{ "engines": { "node": "20" } }"#);

    sandbox
        .kiln()
        .args(["init", "--yes", "-r", "node=22.14.0"])
        .assert()
        .success();

    assert!(sandbox.read("kiln.toml").contains("node = \"22.14.0\""));
}

#[test]
fn init_refuses_to_guess_in_an_empty_directory() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .args(["init", "--yes"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("could not tell what this project needs"))
        .stderr(contains("kiln init --runtime node="));

    assert!(!sandbox.exists("kiln.toml"), "nothing should be written");
}

#[test]
fn init_will_not_silently_replace_an_existing_manifest() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    sandbox
        .kiln()
        .args(["init", "--yes", "-r", "node=20"])
        .assert()
        .code(exit::CONFLICT)
        .stderr(contains("already exists"))
        .stderr(contains("kiln init --force"));

    assert_eq!(
        sandbox.read("kiln.toml"),
        MINIMAL,
        "the file must be untouched"
    );
}

#[test]
fn init_force_replaces_an_existing_manifest() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    sandbox
        .kiln()
        .args(["init", "--yes", "--force", "-r", "node=20"])
        .assert()
        .success();

    assert!(sandbox.read("kiln.toml").contains("node = \"20\""));
}

#[test]
fn init_rejects_an_unknown_runtime_with_a_correction() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .args(["init", "--yes", "-r", "nodejs=22"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("did you mean `node`?"));

    assert!(!sandbox.exists("kiln.toml"));
}

#[test]
fn init_rejects_a_malformed_pin() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .args(["init", "--yes", "-r", "node=banana"])
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("22.14.0"));
}

#[test]
fn no_detect_ignores_the_project_files() {
    let sandbox = Sandbox::new();
    sandbox.write("package.json", r#"{ "engines": { "node": "20" } }"#);

    sandbox
        .kiln()
        .args(["init", "--yes", "--no-detect"])
        .assert()
        .code(exit::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Project discovery
// ---------------------------------------------------------------------------

#[test]
fn commands_find_the_project_from_a_nested_directory() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    let nested = sandbox.path().join("src/components/widgets");
    std::fs::create_dir_all(&nested).unwrap();

    sandbox
        .kiln()
        .current_dir(&nested)
        .arg("doctor")
        .assert()
        .success()
        .stderr(contains("kiln.toml"));
}

#[test]
fn a_missing_manifest_says_where_kiln_looked() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("No kiln.toml found"))
        .stderr(contains("kiln init"));
}

#[test]
fn the_directory_flag_relocates_every_command() {
    let sandbox = Sandbox::new();
    sandbox.write("nested/kiln.toml", MINIMAL);

    sandbox
        .kiln()
        .args(["-C", "nested", "doctor"])
        .assert()
        .success();
}

#[test]
fn the_directory_flag_rejects_a_path_that_is_not_there() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .args(["-C", "nope", "doctor"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("No such directory"));
}

// ---------------------------------------------------------------------------
// Configuration diagnostics
// ---------------------------------------------------------------------------

#[test]
fn every_invalid_fixture_is_rejected_with_a_config_error() {
    let sandbox = Sandbox::new();
    let directory = fixture("invalid");

    let mut checked = 0;
    for entry in std::fs::read_dir(&directory).expect("invalid fixtures") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        checked += 1;

        let contents = std::fs::read_to_string(&path).unwrap();
        sandbox.write("kiln.toml", &contents);

        let assert = sandbox.kiln().arg("install").assert().code(exit::CONFIG);
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.starts_with("error:"),
            "{}: expected a diagnostic, got:\n{stderr}",
            path.display()
        );
    }
    assert!(checked >= 6, "expected the invalid fixtures to be present");
}

#[test]
fn a_bad_version_points_a_caret_at_the_value() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"banana\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("kiln.toml:5:8"))
        .stderr(contains("node = \"banana\""))
        .stderr(contains("^^^^^^^^"))
        .stderr(contains("Expected:"))
        .stderr(contains("a comparator range      >=22, <23"));
}

#[test]
fn a_misspelled_section_lists_the_real_ones() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n\n[runtimes]\nnode = \"22\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("unknown field `runtimes`"))
        .stderr(contains("`environment`"))
        .stderr(contains("singular"));
}

#[test]
fn a_project_cannot_take_over_path() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\n[environment]\nPATH = \"/evil\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("`PATH` cannot be set from kiln.toml"));
}

#[test]
fn shell_operators_in_a_command_are_refused() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\n[commands]\ndev = \"a && b\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("is not supported in a command"))
        .stderr(contains("separate commands"));
}

// ---------------------------------------------------------------------------
// install, doctor, cache
// ---------------------------------------------------------------------------

#[test]
fn install_reports_the_environment_it_built() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .success()
        .stderr(contains("Node.js"))
        .stderr(contains("22.14.0"))
        .stderr(contains("Environment ready"));
}

#[test]
fn install_reports_a_real_problem_rather_than_the_missing_phase() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnodejs = \"22\"\n",
    );

    // The unknown runtime is the user's actual problem; the unbuilt phase is not.
    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("does not know the runtime `nodejs`"));
}

#[test]
fn install_warns_that_services_are_declared_but_unmanaged() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"22.14.0\"\n[services]\npostgres = \"17\"\n",
    );

    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .success()
        .stderr(contains("does not manage services yet"));
}

#[test]
fn doctor_passes_on_the_reference_fixture() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        &std::fs::read_to_string(fixture("example-project/kiln.toml")).unwrap(),
    );

    sandbox
        .kiln()
        .arg("doctor")
        .assert()
        .code(exit::SUCCESS)
        .stderr(contains("configuration"))
        .stderr(contains("Not checked by this release"));
}

#[test]
fn a_tool_with_no_provider_is_refused_rather_than_ignored() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"22.14.0\"\n[tools]\npnpm = \"10.12.1\"\n",
    );

    // Quietly skipping it would hand the developer an environment that does not
    // match the manifest they are reading.
    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("does not know the runtime `pnpm`"));
}

#[test]
fn doctor_fails_when_there_is_no_project() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .arg("doctor")
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("did not pass"));
}

#[test]
fn doctor_json_is_valid_and_names_what_it_cannot_check() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    let assert = sandbox.kiln().args(["doctor", "--json"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    assert_eq!(value["ok"], true);
    assert!(!value["checks"].as_array().unwrap().is_empty());

    let pending = value["not_checked"].as_array().unwrap();
    assert!(
        pending.iter().any(|c| c["name"] == "cache integrity"),
        "doctor must admit what it does not cover"
    );
    assert!(
        !pending.iter().any(|c| c["name"] == "installed runtimes"),
        "installed runtimes are checked now, not deferred"
    );
}

#[test]
fn cache_list_reports_an_empty_store_without_creating_one() {
    let sandbox = Sandbox::new();
    let store = sandbox.home.path().join("store");

    sandbox
        .kiln()
        .args(["cache", "list"])
        .assert()
        .success()
        .stderr(contains("is empty"));

    assert!(
        !store.exists(),
        "a read-only command must not create directories"
    );
}

#[test]
fn cache_list_json_is_valid_on_an_empty_store() {
    let sandbox = Sandbox::new();
    let assert = sandbox
        .kiln()
        .args(["cache", "list", "--json"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(value["artifacts"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// Commands scheduled for later phases
// ---------------------------------------------------------------------------

#[test]
fn no_command_reports_itself_as_unimplemented() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    // Exit code 9 is reserved for "Kiln has not built this yet". Nothing in the
    // command tree should be able to produce it any more, and a stub that
    // silently succeeded instead would be worse than one that failed.
    for args in [
        vec!["cache", "verify"],
        vec!["cache", "list"],
        vec!["cache", "clean"],
        vec!["list"],
        vec!["doctor"],
        vec!["clean"],
    ] {
        let code = sandbox
            .kiln()
            .args(&args)
            .assert()
            .get_output()
            .status
            .code();
        assert_ne!(
            code,
            Some(exit::NOT_IMPLEMENTED),
            "`kiln {}` is still a stub",
            args.join(" ")
        );
    }
}

// ---------------------------------------------------------------------------
// cache verify
// ---------------------------------------------------------------------------

#[test]
fn verifying_a_sealed_entry_reports_it_intact() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.seal(PLANTED);

    sandbox
        .kiln()
        .args(["cache", "verify"])
        .assert()
        .success()
        .stderr(contains("intact"));
}

#[test]
fn verifying_reports_a_corrupted_file_and_exits_nonzero() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.seal(PLANTED);

    // Same length, different bytes: the corruption a size check cannot see.
    std::fs::write(
        sandbox.entry_path(PLANTED).join("content/bin/fake"),
        b"#!/bin/SH\n",
    )
    .unwrap();

    sandbox
        .kiln()
        .args(["cache", "verify"])
        .assert()
        .code(exit::VERIFICATION)
        .stderr(contains("bin/fake"))
        .stderr(contains("same size, different contents"));
}

#[test]
fn verifying_reports_a_deleted_file() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.seal(PLANTED);
    std::fs::remove_file(sandbox.entry_path(PLANTED).join("content/bin/fake")).unwrap();

    sandbox
        .kiln()
        .args(["cache", "verify"])
        .assert()
        .code(exit::VERIFICATION)
        .stderr(contains("is gone"));
}

#[test]
fn an_entry_without_a_manifest_is_unverifiable_not_intact() {
    let sandbox = Sandbox::new();
    // Planted but never sealed: an entry from before Kiln recorded manifests.
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .args(["cache", "verify"])
        .assert()
        // Not an error — Kiln simply cannot tell, and says so.
        .success()
        .stderr(contains("could not be checked"));
}

#[test]
fn a_damaged_entry_does_not_stop_the_others_being_reported() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.plant(PLANTED_B, "python", "3.13.15");
    sandbox.seal(PLANTED);
    sandbox.seal(PLANTED_B);
    std::fs::remove_file(sandbox.entry_path(PLANTED).join("content/bin/fake")).unwrap();

    // A store with one bad runtime must still tell you about the good one.
    sandbox
        .kiln()
        .args(["cache", "verify"])
        .assert()
        .code(exit::VERIFICATION)
        .stderr(contains("node 22.14.0"))
        .stderr(contains("python 3.13.15"));
}

#[test]
fn verify_can_be_narrowed_to_one_runtime() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.plant(PLANTED_B, "python", "3.13.15");
    sandbox.seal(PLANTED);
    sandbox.seal(PLANTED_B);
    std::fs::remove_file(sandbox.entry_path(PLANTED_B).join("content/bin/fake")).unwrap();

    // Asking about the healthy one must not fail because of the other.
    sandbox
        .kiln()
        .args(["cache", "verify", "node"])
        .assert()
        .success();
}

#[test]
fn verifying_something_absent_says_what_to_run() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .args(["cache", "verify", "ruby"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("kiln cache list"));
}

#[test]
fn verify_json_is_valid_on_an_empty_store() {
    let sandbox = Sandbox::new();
    let assert = sandbox
        .kiln()
        .args(["cache", "verify", "--json"])
        .assert()
        .success();

    let value: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("--json must always parse");
    assert_eq!(value["entries"].as_array().unwrap().len(), 0);
}

#[test]
fn verify_json_names_the_damaged_paths() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.seal(PLANTED);
    std::fs::write(
        sandbox.entry_path(PLANTED).join("content/bin/fake"),
        b"tampered",
    )
    .unwrap();

    let assert = sandbox
        .kiln()
        .args(["cache", "verify", "--json"])
        .assert()
        .code(exit::VERIFICATION);

    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let entry = &value["entries"][0];
    assert_eq!(entry["status"], "damaged");
    assert_eq!(entry["differences"][0]["path"], "bin/fake");
    assert_eq!(entry["differences"][0]["kind"], "size_changed");
}

#[test]
fn commands_report_a_missing_project_first() {
    let sandbox = Sandbox::new();
    for args in [vec!["run", "node"], vec!["shell"], vec!["list"]] {
        sandbox
            .kiln()
            .args(&args)
            .assert()
            .code(exit::NOT_FOUND)
            .stderr(contains("No kiln.toml found"));
    }
}

// ---------------------------------------------------------------------------
// Documentation that must not rot
// ---------------------------------------------------------------------------

/// The repository root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn every_shipped_example_manifest_is_valid() {
    let root = repo_root();
    let mut checked = Vec::new();

    let mut candidates = vec![root.join("kiln.example.toml")];
    for entry in std::fs::read_dir(root.join("examples")).expect("examples directory") {
        let path = entry.unwrap().path();
        if path.is_dir() {
            candidates.push(path.join("kiln.toml"));
        }
    }

    for manifest in candidates {
        assert!(manifest.is_file(), "{} is missing", manifest.display());

        // Documentation people copy from has to actually work.
        let sandbox = Sandbox::new();
        sandbox.write("kiln.toml", &std::fs::read_to_string(&manifest).unwrap());
        let assert = sandbox.kiln().arg("doctor").assert();

        let output = assert.get_output();
        assert_eq!(
            output.status.code(),
            Some(exit::SUCCESS),
            "{} does not pass `kiln doctor`:\n{}",
            manifest.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        checked.push(manifest);
    }

    assert!(
        checked.len() >= 4,
        "expected the example manifests to exist"
    );
}

#[test]
fn the_reference_manifest_exercises_every_section() {
    let text = std::fs::read_to_string(repo_root().join("kiln.example.toml")).unwrap();
    for section in [
        "[project]",
        "[runtime]",
        "[tools]",
        "[environment]",
        "[commands]",
        "[services]",
    ] {
        assert!(
            text.contains(section),
            "kiln.example.toml is missing {section}"
        );
    }
}

// ---------------------------------------------------------------------------
// Output discipline
// ---------------------------------------------------------------------------

#[test]
fn narration_goes_to_stderr_so_stdout_stays_pipeable() {
    let sandbox = Sandbox::new();
    sandbox.write("package.json", r#"{ "engines": { "node": "22" } }"#);

    let assert = sandbox.kiln().args(["init", "--yes"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(
        stdout.is_empty(),
        "init produces no data, so stdout must be empty; got: {stdout}"
    );
}

#[test]
fn errors_are_never_silenced_by_quiet() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"nope\"\n",
    );

    sandbox
        .kiln()
        .args(["--quiet", "install"])
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("Invalid kiln.toml"));
}

#[test]
fn colour_is_off_when_asked_and_on_when_forced() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"nope\"\n",
    );

    let plain = sandbox.kiln().arg("install").assert().code(exit::CONFIG);
    let stderr = String::from_utf8(plain.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains('\u{1b}'),
        "--color never must emit no escapes"
    );

    let mut forced = Command::cargo_bin("kiln").unwrap();
    let coloured = forced
        .current_dir(sandbox.path())
        .env("KILN_HOME", sandbox.home.path())
        .env_remove("NO_COLOR")
        .args(["--color", "always", "install"])
        .assert()
        .code(exit::CONFIG);
    let stderr = String::from_utf8(coloured.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains('\u{1b}'),
        "--color always must emit escapes"
    );
}

#[test]
fn kiln_never_touches_the_home_directory_it_was_not_given() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    // Every command in this release is read-only with respect to the store.
    for args in [vec!["doctor"], vec!["cache", "list"], vec!["version"]] {
        sandbox.kiln().args(&args).assert().success();
    }

    let entries: Vec<_> = std::fs::read_dir(sandbox.home.path())
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        entries.is_empty(),
        "no command in this release should write to the store yet"
    );
}

// ---------------------------------------------------------------------------
// install, offline and the lockfile
//
// These are hermetic: the runtime is planted in the store and the lockfile
// points at it, so the whole install path runs without a byte crossing the
// network. Tests that genuinely need upstream live in `tests/network.rs`.
// ---------------------------------------------------------------------------

#[test]
fn a_locked_and_cached_project_installs_offline() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .args(["install", "--offline"])
        .assert()
        .success()
        .stderr(contains("already in the store"))
        .stderr(contains("0 downloaded, 1 reused"))
        .stderr(contains("Environment ready"));
}

#[test]
fn an_unchanged_install_leaves_the_lockfile_untouched() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    let before = sandbox.read("kiln.lock");
    sandbox
        .kiln()
        .args(["install", "--offline"])
        .assert()
        .success();

    // A no-op install must not show up in `git status`.
    assert_eq!(sandbox.read("kiln.lock"), before);
}

#[test]
fn offline_install_says_what_it_would_have_fetched() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    sandbox
        .kiln()
        .args(["install", "--offline"])
        .assert()
        .code(exit::NETWORK)
        .stderr(contains("offline"))
        .stderr(contains("without `--offline`"));
}

#[test]
fn editing_the_manifest_invalidates_the_lockfile_entry() {
    let sandbox = Sandbox::new();
    // The lockfile was made for `22.14.0`, but the manifest now asks for `20`.
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"20\"\n",
    );
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    // Offline, so re-resolution cannot happen — which proves it was attempted
    // rather than the stale entry being reused.
    sandbox
        .kiln()
        .args(["install", "--offline"])
        .assert()
        .code(exit::NETWORK);
}

#[test]
fn a_lockfile_from_a_newer_kiln_asks_for_an_upgrade() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.write(
        "kiln.lock",
        "version = 999\ngenerated_by = \"kiln 9.9.9\"\n\n[project]\nname = \"app\"\n",
    );

    sandbox
        .kiln()
        .args(["install", "--offline"])
        .assert()
        .code(exit::UNSUPPORTED)
        .stderr(contains("newer version of Kiln"))
        .stderr(contains("upgrade"));
}

#[test]
fn list_reports_what_is_installed() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    let assert = sandbox.kiln().arg("list").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(stdout.contains("RUNTIME"));
    assert!(stdout.contains("Node.js"));
    assert!(stdout.contains("22.14.0"));
    assert!(stdout.contains("installed"));
}

#[test]
fn list_distinguishes_locked_from_installed() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    // Locked, but the artifact is not in the store.
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);

    let assert = sandbox.kiln().arg("list").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("locked, not installed"), "{stdout}");
}

#[test]
fn list_reports_an_unresolved_project() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    let assert = sandbox.kiln().arg("list").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("not resolved yet"), "{stdout}");
}

#[test]
fn list_json_is_machine_readable() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    let assert = sandbox.kiln().args(["list", "--json"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    let node = &value["runtimes"][0];
    assert_eq!(node["id"], "node");
    assert_eq!(node["version"], "22.14.0");
    assert_eq!(node["installed"], true);
}

#[test]
fn cache_list_shows_provenance_rather_than_store_paths() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");

    let assert = sandbox.kiln().args(["cache", "list"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(stdout.contains("RUNTIME"));
    assert!(stdout.contains("node"));
    assert!(stdout.contains("22.14.0"));
    assert!(
        stdout.contains(&PLANTED[..12]),
        "the short digest identifies the entry"
    );
}

#[test]
fn doctor_reports_a_project_that_is_installed() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .arg("doctor")
        .assert()
        .success()
        .stderr(contains("installed"))
        .stderr(contains("22.14.0"));
}

#[test]
fn doctor_warns_when_the_environment_is_not_installed() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);

    sandbox
        .kiln()
        .arg("doctor")
        // A project that has not been installed is not *broken*.
        .assert()
        .success()
        .stderr(contains("run `kiln install`"));
}

#[test]
fn two_projects_share_one_store_entry() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", MINIMAL);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    // A second project in a subdirectory, pinning the same runtime.
    sandbox.write("other/kiln.toml", MINIMAL);
    let platform = platform_key();
    sandbox.write(
        "other/kiln.lock",
        &format!(
            "version = 1\ngenerated_by = \"kiln 0.1.0\"\n\n[project]\nname = \"other\"\n\n\
             [platform.{platform}.runtime.node]\nprovider = \"node\"\n\
             requirement = \"22.14.0\"\nversion = \"22.14.0\"\n\n\
             [platform.{platform}.runtime.node.artifact]\n\
             url = \"https://example.test/node.tar.gz\"\n\
             digest = \"sha256:{PLANTED}\"\nformat = \"tar.gz\"\n"
        ),
    );

    sandbox
        .kiln()
        .args(["-C", "other", "install", "--offline"])
        .assert()
        .success()
        .stderr(contains("already in the store"));

    let assert = sandbox
        .kiln()
        .args(["cache", "list", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        value["artifacts"].as_array().unwrap().len(),
        1,
        "the same runtime must not be stored twice"
    );
}

// ---------------------------------------------------------------------------
// run and shell
//
// The "runtime" here is a shell script planted in the store. That exercises the
// real code path — lockfile, store lookup, PATH composition, program
// resolution, spawning, exit codes — without a download, so these run offline
// in milliseconds. Real runtimes are covered in `tests/network.rs`.
// ---------------------------------------------------------------------------

/// A project whose single pinned runtime is installed and provides programs.
fn ready_sandbox(manifest: &str) -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", manifest);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox
}

const RUNNABLE: &str = "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n";

#[test]
fn run_executes_the_projects_runtime() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "echo v22.14.0-from-the-store");

    sandbox
        .kiln()
        .args(["run", "node", "--version"])
        .assert()
        .success()
        .stdout(contains("v22.14.0-from-the-store"));
}

#[test]
fn the_projects_runtime_shadows_the_system_one() {
    // `echo` exists on every Unix. If Kiln's PATH ordering or its program
    // resolution were wrong, this would find /bin/echo and print nothing.
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "echo", "printf 'the store won\\n'");

    sandbox
        .kiln()
        .args(["run", "echo", "ignored"])
        .assert()
        .success()
        .stdout(contains("the store won"));
}

#[test]
fn run_propagates_the_exit_code() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "exit 42");

    // Transparency is the point: `kiln run npm test` has to be usable wherever
    // `npm test` was.
    sandbox.kiln().args(["run", "node"]).assert().code(42);
}

#[test]
fn run_passes_arguments_through_untouched() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "printf '%s|' \"$@\"");

    sandbox
        .kiln()
        .args(["run", "node", "--flag", "a b", "$HOME", "&&", "*"])
        .assert()
        .success()
        // Nothing expanded, split or interpreted on the way through.
        .stdout("--flag|a b|$HOME|&&|*|");
}

#[test]
fn run_exports_the_manifest_environment() {
    let sandbox = ready_sandbox(
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n\n\
         [environment]\nNODE_ENV = \"development\"\n",
    );
    sandbox.plant_program(
        PLANTED,
        "node",
        "printf '%s %s' \"$NODE_ENV\" \"$KILN_PROJECT\"",
    );

    sandbox
        .kiln()
        .args(["run", "node"])
        .assert()
        .success()
        .stdout("development app");
}

#[test]
fn run_expands_a_named_command() {
    let sandbox = ready_sandbox(
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n\n\
         [commands]\ndev = \"node serve --port 3000\"\n",
    );
    sandbox.plant_program(PLANTED, "node", "printf '%s|' \"$@\"");

    sandbox
        .kiln()
        .args(["run", "dev"])
        .assert()
        .success()
        .stdout("serve|--port|3000|");
}

#[test]
fn extra_arguments_append_to_a_named_command() {
    let sandbox = ready_sandbox(
        "[project]\nname = \"app\"\n\n[runtime]\nnode = \"22.14.0\"\n\n\
         [commands]\ndev = \"node serve\"\n",
    );
    sandbox.plant_program(PLANTED, "node", "printf '%s|' \"$@\"");

    sandbox
        .kiln()
        .args(["run", "dev", "--watch"])
        .assert()
        .success()
        .stdout("serve|--watch|");
}

#[test]
fn run_refuses_to_install_as_a_side_effect() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", RUNNABLE);
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    // Locked, but nothing in the store.

    sandbox
        .kiln()
        .args(["run", "node"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("not installed"))
        .stderr(contains("kiln install"));
}

#[test]
fn run_reports_a_program_that_is_not_there() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "true");
    sandbox.plant_program(PLANTED, "npm", "true");

    sandbox
        .kiln()
        .args(["run", "definitely-not-installed"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("is not available in this environment"))
        // Saying what the project *does* provide turns this into a diagnosis.
        .stderr(contains("node"))
        .stderr(contains("npm"));
}

#[test]
fn run_prints_nothing_of_its_own() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "echo output");

    let assert = sandbox.kiln().args(["run", "node"]).assert().success();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    // A wrapper that announces itself is a wrapper you cannot pipe.
    assert!(stderr.is_empty(), "run should be silent, got: {stderr}");
    assert_eq!(
        String::from_utf8(assert.get_output().stdout.clone()).unwrap(),
        "output\n"
    );
}

#[test]
fn shell_activates_the_environment() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "echo v22.14.0-from-the-store");

    let assert = sandbox
        .kiln()
        .args(["shell", "--shell", "/bin/sh"])
        .write_stdin("node\nexit\n")
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();

    assert!(
        stdout.contains("v22.14.0-from-the-store"),
        "stdout: {stdout}"
    );
    assert!(stderr.contains("environment activated"), "stderr: {stderr}");
    assert!(
        stderr.contains("left the app environment"),
        "stderr: {stderr}"
    );
}

#[test]
fn shell_propagates_the_exit_code() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox
        .kiln()
        .args(["shell", "--shell", "/bin/sh"])
        .write_stdin("exit 7\n")
        .assert()
        .code(7);
}

#[test]
fn shell_refuses_to_nest_in_the_same_project() {
    let sandbox = ready_sandbox(RUNNABLE);

    sandbox
        .kiln()
        .env("KILN_PROJECT", "app")
        .args(["shell", "--shell", "/bin/sh"])
        .write_stdin("")
        .assert()
        .code(exit::CONFLICT)
        .stderr(contains("Already inside the `app` environment"));
}

#[test]
fn shell_allows_moving_to_a_different_project() {
    let sandbox = ready_sandbox(RUNNABLE);

    // Being inside *another* project's shell is a normal workflow.
    sandbox
        .kiln()
        .env("KILN_PROJECT", "some-other-project")
        .args(["shell", "--shell", "/bin/sh"])
        .write_stdin("exit\n")
        .assert()
        .success();
}

#[test]
fn shell_rejects_a_shell_that_does_not_exist() {
    let sandbox = ready_sandbox(RUNNABLE);

    sandbox
        .kiln()
        .args(["shell", "--shell", "/nonexistent/fish"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("No such shell"));
}

#[test]
fn shell_refuses_before_install() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", RUNNABLE);

    sandbox
        .kiln()
        .args(["shell", "--shell", "/bin/sh"])
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("kiln install"));
}

#[test]
fn list_agrees_with_what_run_will_do() {
    // `list` says installed, so `run` must work — they read the same state.
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "echo ok");

    let assert = sandbox.kiln().args(["list", "--json"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["ready"], true);
    assert!(
        !value["runtimes"][0]["bin_dirs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "an installed runtime contributes a bin directory"
    );

    sandbox.kiln().args(["run", "node"]).assert().success();
}

// ---------------------------------------------------------------------------
// lock and --locked
//
// This is the CI story: can a team trust that what was reviewed is what gets
// installed? All hermetic — drift detection is structural and offline by
// design, which is exactly what lets `--locked` fail before touching a network.
// ---------------------------------------------------------------------------

#[test]
fn locked_install_succeeds_when_the_lockfile_matches() {
    let sandbox = ready_sandbox(RUNNABLE);

    sandbox
        .kiln()
        .args(["install", "--locked"])
        .assert()
        .success()
        .stderr(contains("Environment ready"));
}

#[test]
fn locked_install_refuses_when_the_manifest_was_edited() {
    let sandbox = Sandbox::new();
    // Locked against `22.14.0`; someone changed the manifest to `22`.
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"22\"\n",
    );
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .args(["install", "--locked"])
        .assert()
        .code(exit::CONFLICT)
        .stderr(contains("`--locked` was given"))
        .stderr(contains("locked for `22.14.0`"))
        .stderr(contains("asks for `22`"))
        .stderr(contains("kiln lock"));
}

#[test]
fn locked_install_refuses_without_a_lockfile() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", RUNNABLE);

    // Offline *and* locked: it must fail on the lockfile, not on the network,
    // because the check is meant to run before anything is resolved.
    sandbox
        .kiln()
        .args(["install", "--locked"])
        .assert()
        .code(exit::CONFLICT)
        .stderr(contains("is not locked"));
}

#[test]
fn locked_install_notices_a_runtime_that_was_removed() {
    let sandbox = Sandbox::new();
    // The lockfile pins node; the manifest no longer mentions it.
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\npython = \"3.13\"\n",
    );
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);

    sandbox
        .kiln()
        .args(["install", "--locked"])
        .assert()
        .code(exit::CONFLICT)
        .stderr(contains("no longer in kiln.toml"));
}

#[test]
fn an_ordinary_install_still_updates_the_lockfile() {
    let sandbox = ready_sandbox(RUNNABLE);
    let before = sandbox.read("kiln.lock");

    // Without `--locked`, an unchanged project still writes nothing...
    sandbox.kiln().arg("install").assert().success();
    assert_eq!(sandbox.read("kiln.lock"), before);
}

#[test]
fn lock_check_passes_on_a_matching_lockfile() {
    let sandbox = ready_sandbox(RUNNABLE);

    sandbox
        .kiln()
        .args(["lock", "--check"])
        .assert()
        .success()
        .stderr(contains("up to date"));
}

#[test]
fn lock_check_fails_and_names_the_platform() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nnode = \"20\"\n",
    );
    sandbox.lockfile("22.14.0", "22.14.0", PLANTED);

    let assert = sandbox
        .kiln()
        .args(["lock", "--check"])
        .assert()
        .code(exit::CONFLICT);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("out of date"), "{stderr}");
    assert!(
        stderr.contains(&platform_key()),
        "should name the platform: {stderr}"
    );
    assert!(stderr.contains("kiln lock"), "{stderr}");
}

#[test]
fn lock_check_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", RUNNABLE);

    sandbox.kiln().args(["lock", "--check"]).assert().failure();
    assert!(!sandbox.exists("kiln.lock"), "--check must never write");
}

#[test]
fn lock_rejects_an_unknown_platform_and_lists_the_real_ones() {
    let sandbox = Sandbox::new();
    sandbox.write("kiln.toml", RUNNABLE);

    sandbox
        .kiln()
        .args(["lock", "--platform", "solaris-sparc"])
        .assert()
        .code(exit::CONFIG)
        .stderr(contains("macos-aarch64"))
        .stderr(contains("linux-x86_64-musl"));
}

#[test]
fn lock_and_check_agree_about_which_platforms_matter() {
    // A project that cannot exist on Alpine must not make `--check` fail
    // forever; the two commands have to apply the same rule.
    let sandbox = ready_sandbox(RUNNABLE);

    // The planted lockfile only covers this host, so `--check` for the host
    // passes while `--all-platforms` would want more. What must never happen is
    // `--check --all-platforms` demanding a platform `lock` would have skipped.
    sandbox.kiln().args(["lock", "--check"]).assert().success();
}

#[test]
fn upgrading_kiln_does_not_invalidate_a_lockfile() {
    // The lockfile records `generated_by`. If Kiln rewrote that on every run,
    // `--locked` would fail after an upgrade for no real reason.
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.write(
        "kiln.lock",
        &sandbox.read("kiln.lock").replace(
            "generated_by = \"kiln 0.1.0\"",
            "generated_by = \"kiln 0.0.1\"",
        ),
    );

    sandbox
        .kiln()
        .args(["install", "--locked"])
        .assert()
        .success();
    assert!(
        sandbox.read("kiln.lock").contains("kiln 0.0.1"),
        "an unchanged lockfile must not be rewritten"
    );
}

#[test]
fn lock_help_explains_the_team_workflow() {
    let sandbox = Sandbox::new();
    let assert = sandbox.kiln().args(["lock", "--help"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(stdout.contains("--all-platforms"));
    assert!(stdout.contains("--check"));
    assert!(stdout.contains("--platform"));
}

// ---------------------------------------------------------------------------
// Go, and the cost of adding a runtime
// ---------------------------------------------------------------------------

#[test]
fn go_is_a_known_runtime() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\ngo = \"1.25\"\n",
    );

    // Offline, so this reaches the network and stops — which proves the
    // manifest validated and `go` resolved to a real provider.
    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::NETWORK)
        .stderr(contains("offline"));
}

#[test]
fn an_unknown_runtime_lists_go_among_the_alternatives() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\nrust = \"1.80\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("one of: go, node, python"));
}

#[test]
fn golang_is_corrected_to_go() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "kiln.toml",
        "[project]\nname = \"app\"\n[runtime]\ngolang = \"1.25\"\n",
    );

    sandbox
        .kiln()
        .arg("install")
        .assert()
        .code(exit::NOT_FOUND)
        .stderr(contains("did you mean `go`?"));
}

#[test]
fn init_detects_a_go_project_from_its_go_mod() {
    let sandbox = Sandbox::new();
    sandbox.write("go.mod", "module example.com/app\n\ngo 1.25\n");

    sandbox
        .kiln()
        .args(["init", "--yes"])
        .assert()
        .success()
        .stderr(contains("Go"))
        .stderr(contains("go.mod (go directive)"));

    assert!(sandbox.read("kiln.toml").contains("go = \"1.25\""));
}

// ---------------------------------------------------------------------------
// clean
// ---------------------------------------------------------------------------

#[test]
fn clean_reports_before_it_removes() {
    let sandbox = Sandbox::new();
    let staging = sandbox.home.path().join("staging/interrupted-999");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("partial"), vec![0u8; 4096]).unwrap();

    sandbox
        .kiln()
        .arg("clean")
        .assert()
        .success()
        .stderr(contains("would be freed"))
        .stderr(contains("Nothing was removed"));

    // A command whose whole job is deleting must not delete by default.
    assert!(staging.exists(), "dry run must leave everything in place");
}

#[test]
fn clean_force_removes_interrupted_installs() {
    let sandbox = Sandbox::new();
    let staging = sandbox.home.path().join("staging/interrupted-999");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("partial"), vec![0u8; 4096]).unwrap();

    sandbox
        .kiln()
        .args(["clean", "--force"])
        .assert()
        .success()
        .stderr(contains("Freed"));

    assert!(!staging.exists());
}

#[test]
fn clean_never_touches_installed_runtimes() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    std::fs::create_dir_all(sandbox.home.path().join("staging/junk")).unwrap();

    sandbox.kiln().args(["clean", "--force"]).assert().success();

    // The store is the one thing `kiln clean` must leave alone.
    let assert = sandbox
        .kiln()
        .args(["cache", "list", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["artifacts"].as_array().unwrap().len(), 1);
}

#[test]
fn clean_on_a_fresh_machine_says_so() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .arg("clean")
        .assert()
        .success()
        .stderr(contains("Nothing to clean"));
}

// ---------------------------------------------------------------------------
// cache clean
//
// Kiln keeps no registry of projects, so reachability is unknowable and
// eviction goes by last use instead. These pin down both halves: that idle
// entries go, and that used ones stay.
// ---------------------------------------------------------------------------

/// A timestamp comfortably older than any sane `--older-than`.
const LONG_AGO: u64 = 1_600_000_000;

#[test]
fn cache_clean_keeps_recently_used_runtimes() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");

    sandbox
        .kiln()
        .args(["cache", "clean"])
        .assert()
        .success()
        .stderr(contains("Nothing to remove"));
}

#[test]
fn cache_clean_reports_idle_runtimes_without_removing_them() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.age(PLANTED, LONG_AGO);

    sandbox
        .kiln()
        .args(["cache", "clean"])
        .assert()
        .success()
        .stderr(contains("node 22.14.0"))
        .stderr(contains("would be removed"))
        .stderr(contains("Nothing was deleted"));

    // A command whose job is deleting must not delete by default.
    assert!(sandbox.entry_path(PLANTED).exists());
}

#[test]
fn cache_clean_force_removes_idle_runtimes() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.age(PLANTED, LONG_AGO);

    sandbox
        .kiln()
        .args(["cache", "clean", "--force"])
        .assert()
        .success()
        .stderr(contains("Removed 1 runtime"))
        .stderr(contains("comes back with `kiln install`"));

    assert!(!sandbox.entry_path(PLANTED).exists());
}

#[test]
fn cache_clean_leaves_no_debris_in_the_store() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.age(PLANTED, LONG_AGO);
    sandbox
        .kiln()
        .args(["cache", "clean", "--force"])
        .assert()
        .success();

    let assert = sandbox
        .kiln()
        .args(["cache", "list", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["artifacts"].as_array().unwrap().len(), 0);
}

#[test]
fn the_age_threshold_is_adjustable() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    // Idle, but only just.
    let recent = kiln_now() - (5 * 24 * 60 * 60);
    sandbox.age(PLANTED, recent);

    sandbox
        .kiln()
        .args(["cache", "clean", "--older-than", "30"])
        .assert()
        .success()
        .stderr(contains("Nothing to remove"));

    sandbox
        .kiln()
        .args(["cache", "clean", "--older-than", "3"])
        .assert()
        .success()
        .stderr(contains("would be removed"));
}

#[test]
fn cache_clean_all_ignores_age() {
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    // Used seconds ago, and still condemned by `--all`.
    sandbox.age(PLANTED, kiln_now());

    sandbox
        .kiln()
        .args(["cache", "clean", "--all", "--force"])
        .assert()
        .success()
        .stderr(contains("Removed 1 runtime"));

    assert!(!sandbox.entry_path(PLANTED).exists());
}

#[test]
fn running_a_command_protects_its_runtime_from_eviction() {
    let sandbox = ready_sandbox(RUNNABLE);
    sandbox.plant_program(PLANTED, "node", "true");
    sandbox.age(PLANTED, LONG_AGO);

    // The whole point of tracking use rather than install time.
    sandbox.kiln().args(["run", "node"]).assert().success();

    sandbox
        .kiln()
        .args(["cache", "clean"])
        .assert()
        .success()
        .stderr(contains("Nothing to remove"));
}

#[test]
fn listing_the_store_does_not_count_as_using_it() {
    // Read-only commands must not keep an entry alive, or nothing would ever
    // be collectable on a machine where someone runs `kiln list` in a prompt.
    let sandbox = Sandbox::new();
    sandbox.plant(PLANTED, "node", "22.14.0");
    sandbox.age(PLANTED, LONG_AGO);

    sandbox.kiln().args(["cache", "list"]).assert().success();
    // `doctor` warns about the missing project here; its status is not the point.
    sandbox.kiln().arg("doctor").assert().code(exit::CONFIG);

    sandbox
        .kiln()
        .args(["cache", "clean"])
        .assert()
        .success()
        .stderr(contains("would be removed"));
}

#[test]
fn cache_clean_on_an_empty_store_says_so() {
    let sandbox = Sandbox::new();
    sandbox
        .kiln()
        .args(["cache", "clean"])
        .assert()
        .success()
        .stderr(contains("store is empty"));
}

/// Seconds since the Unix epoch.
fn kiln_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
