//! `kiln install` — make this machine match the manifest.

use std::path::Path;

use kiln_config::Project;
use kiln_core::error::Result;
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Platform, Ui};
use kiln_net::{Http, MetadataCache};
use kiln_resolver::{Lockfile, Resolution};
use kiln_runtime::Registry;

use crate::cli::InstallArgs;
use crate::commands::progress::CliObserver;
use crate::commands::{humanize_bytes, require_project};

pub fn run(args: &InstallArgs, directory: &Path, offline: bool, ui: &Ui) -> Result<()> {
    let project = require_project(directory)?;
    let manifest = project.manifest();
    let platform = Platform::detect()?;
    let registry = Registry::builtin();
    let paths = KilnPaths::discover()?;

    ui.banner("install");
    ui.blank();
    ui.section("Project");
    ui.field("name", manifest.project.name.as_str(), 10);
    ui.field("directory", &project.root().display().to_string(), 10);
    ui.field("platform", &platform.to_string(), 10);
    ui.blank();

    let http = Http::new(offline).with_cache(MetadataCache::new(paths.state().join("http")));
    let existing = Lockfile::read(&project.lockfile_path())?;

    // Checked before resolving, and offline: `--locked` exists to stop CI from
    // silently installing something other than what was reviewed, so it has to
    // fail before any of that can happen.
    if args.locked {
        let drifts = kiln_resolver::drift(manifest, existing.as_ref(), &platform);
        if !drifts.is_empty() {
            return Err(kiln_resolver::locked_error(&drifts, &platform));
        }
    }

    ui.section("Resolving");
    let resolution =
        kiln_resolver::resolve(manifest, &registry, &platform, &http, existing.as_ref())?;
    report_resolution(ui, &resolution);

    ui.blank();
    ui.section("Installing");
    let mut observer = CliObserver::new(ui);
    let outcome = kiln_resolver::install(&resolution, &registry, &paths, &http, &mut observer)?;

    // Installing counts as using: a runtime reinstalled today should not be
    // evicted tomorrow for having been downloaded a year ago.
    kiln_cache::ContentStore::new(paths.store())
        .touch_all(outcome.runtimes.iter().map(|r| &r.entry.digest));

    write_lockfile(&project, &resolution, existing, ui)?;
    summarise(ui, &outcome, manifest);

    Ok(())
}

fn report_resolution(ui: &Ui, resolution: &Resolution) {
    let width = resolution
        .runtimes
        .iter()
        .map(|r| r.display_name.len())
        .max()
        .unwrap_or(0)
        .max(10);

    for runtime in &resolution.runtimes {
        let origin = if runtime.from_lockfile {
            "from kiln.lock"
        } else {
            "resolved"
        };
        ui.status(format!(
            "  {:<width$}  {:<12} {}",
            runtime.display_name,
            runtime.version.to_string(),
            paint(origin, Style::Dim, ui.color()),
        ));
    }
}

fn write_lockfile(
    project: &Project,
    resolution: &Resolution,
    existing: Option<Lockfile>,
    ui: &Ui,
) -> Result<()> {
    let updated = kiln_resolver::record(resolution, project.manifest(), existing.clone());

    // Only write when something actually changed, so `kiln install` on an
    // unchanged project leaves the working tree clean and does not show up in
    // `git status`.
    if existing.as_ref() == Some(&updated) {
        return Ok(());
    }

    updated.write(&project.lockfile_path())?;
    ui.blank();
    ui.note(format!(
        "  {} {}",
        if existing.is_none() {
            "created"
        } else {
            "updated"
        },
        kiln_config::LOCKFILE_FILE
    ));
    Ok(())
}

fn summarise(ui: &Ui, outcome: &kiln_resolver::InstallOutcome, manifest: &kiln_config::Manifest) {
    ui.blank();
    ui.section("Environment");

    let width = outcome
        .runtimes
        .iter()
        .map(|r| r.display_name.len())
        .max()
        .unwrap_or(0)
        .max(10);
    for runtime in &outcome.runtimes {
        ui.field(&runtime.display_name, &runtime.version.to_string(), width);
    }

    ui.blank();
    let downloaded = outcome.downloaded();
    let reused = outcome.reused();
    ui.note(format!(
        "  {} runtime(s): {downloaded} downloaded, {reused} reused from the store",
        outcome.runtimes.len()
    ));

    if !manifest.services.is_empty() {
        let names: Vec<&str> = manifest.services.keys().map(|s| s.as_str()).collect();
        ui.blank();
        ui.warn(format!(
            "{} declared under [services]; Kiln does not manage services yet",
            names.join(", ")
        ));
    }

    ui.blank();
    ui.ok("Environment ready");
    ui.blank();
    ui.note(format!("  Installed into {}", humanize_store(outcome)));
    ui.note("  `kiln shell` to enter it, or `kiln run <command>` for one command.");
}

fn humanize_store(outcome: &kiln_resolver::InstallOutcome) -> String {
    let bytes: u64 = outcome
        .runtimes
        .iter()
        .filter_map(|r| r.entry.meta.as_ref().map(|m| m.artifact_bytes))
        .sum();
    if bytes == 0 {
        "the Kiln store".to_string()
    } else {
        format!("the Kiln store ({} of artifacts)", humanize_bytes(bytes))
    }
}
