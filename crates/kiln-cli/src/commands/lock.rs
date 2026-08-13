//! `kiln lock` — resolve versions and write `kiln.lock`, without installing.
//!
//! The command that makes a lockfile useful to a team rather than to one laptop.
//! `--all-platforms` resolves for every platform Kiln supports, not just the one
//! you happen to be sitting at, so a macOS developer can commit a lockfile that
//! the Linux CI runner installs from without re-resolving anything.
//!
//! That works because a provider is handed the platform it is resolving *for*,
//! never the host it is running on — the artifact URL for `linux-x86_64-gnu` is
//! as computable from a Mac as it is from Linux.
//!
//! Not every runtime exists everywhere. Node.js publishes no musl builds, so a
//! project pinning Node cannot be locked for Alpine. Kiln says so and carries on
//! with the platforms it can serve, because "your project does not run on
//! Alpine" is information, not a failure of the lock command.

use std::path::Path;

use kiln_core::error::{Error, Result};
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Platform, Ui};
use kiln_net::{Http, MetadataCache};
use kiln_resolver::{Lockfile, Resolution};
use kiln_runtime::Registry;

use crate::cli::LockArgs;
use crate::commands::require_project;

pub fn run(args: &LockArgs, directory: &Path, offline: bool, ui: &Ui) -> Result<()> {
    let project = require_project(directory)?;
    let manifest = project.manifest();
    let registry = Registry::builtin();
    let paths = KilnPaths::discover()?;

    let targets = targets(args)?;
    let existing = Lockfile::read(&project.lockfile_path())?;

    // `--check` is the CI form: report, never write, and let the exit code
    // carry the answer.
    if args.check {
        return check(manifest, existing.as_ref(), &targets, &registry, args, ui);
    }

    ui.banner("lock");
    ui.blank();
    ui.section("Resolving");

    let http = Http::new(offline).with_cache(MetadataCache::new(paths.state().join("http")));
    let mut resolutions: Vec<Resolution> = Vec::new();
    let mut skipped: Vec<(Platform, String)> = Vec::new();

    for platform in &targets {
        match kiln_resolver::resolve(manifest, &registry, platform, &http, existing.as_ref()) {
            Ok(resolution) => {
                report(ui, platform, &resolution);
                resolutions.push(resolution);
            }
            // A platform no provider serves is a fact about the project, not a
            // reason to abandon the platforms that do work — but only when the
            // user asked for the whole set. An explicit `--platform` that
            // cannot be served is a failed request.
            Err(error)
                if args.all_platforms && error.kind() == kiln_core::ErrorKind::Unsupported =>
            {
                skipped.push((*platform, error.summary().to_string()));
            }
            Err(error) => return Err(error),
        }
    }

    if !skipped.is_empty() {
        ui.blank();
        ui.section("Skipped");
        for (platform, reason) in &skipped {
            ui.status(format!(
                "  {:<20} {}",
                platform.key(),
                paint(reason, Style::Dim, ui.color())
            ));
        }
    }

    if resolutions.is_empty() {
        return Err(Error::not_found("No platform could be locked")
            .because("Every platform asked for is missing a runtime this project pins."));
    }

    let updated = kiln_resolver::record_all(&resolutions, manifest, existing.clone());
    ui.blank();

    if existing.as_ref() == Some(&updated) {
        ui.ok(format!(
            "{} is already up to date",
            kiln_config::LOCKFILE_FILE
        ));
        return Ok(());
    }

    updated.write(&project.lockfile_path())?;
    ui.ok(format!(
        "{} {} for {}",
        if existing.is_none() {
            "Wrote"
        } else {
            "Updated"
        },
        kiln_config::LOCKFILE_FILE,
        pluralize(updated.platform.len(), "platform")
    ));
    ui.blank();
    ui.note("  Commit it. Everyone who clones the repository installs from it.");
    Ok(())
}

/// Which platforms to lock for.
fn targets(args: &LockArgs) -> Result<Vec<Platform>> {
    if args.all_platforms {
        return Ok(Platform::all_supported());
    }
    if args.platforms.is_empty() {
        return Ok(vec![Platform::detect()?]);
    }

    let mut targets = Vec::new();
    for key in &args.platforms {
        let platform = Platform::from_key(key).map_err(|error| {
            error.expected(format!(
                "one of:\n{}",
                Platform::all_supported()
                    .iter()
                    .map(|p| format!("  {}", p.key()))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        })?;
        if !targets.contains(&platform) {
            targets.push(platform);
        }
    }
    Ok(targets)
}

/// `--check`: is the lockfile already correct for every requested platform?
///
/// Skips the same platforms a real lock would skip. `kiln lock --all-platforms`
/// followed by `kiln lock --check --all-platforms` has to pass, or the CI gate
/// is unusable — and a project pinning Node.js can never be locked for Alpine.
fn check(
    manifest: &kiln_config::Manifest,
    existing: Option<&Lockfile>,
    targets: &[Platform],
    registry: &Registry,
    args: &LockArgs,
    ui: &Ui,
) -> Result<()> {
    let mut stale: Vec<(Platform, Vec<kiln_resolver::Drift>)> = Vec::new();
    let mut checked = 0;

    for platform in targets {
        // `plan` is offline and answers exactly the question a lock would ask
        // first: can this project exist on this platform at all?
        if args.all_platforms && kiln_resolver::plan(manifest, registry, platform).is_err() {
            continue;
        }
        checked += 1;

        let drifts = kiln_resolver::drift(manifest, existing, platform);
        if !drifts.is_empty() {
            stale.push((*platform, drifts));
        }
    }

    if stale.is_empty() {
        ui.status(format!(
            "{} {} is up to date for {}",
            paint("✓", Style::Green, ui.color()),
            kiln_config::LOCKFILE_FILE,
            pluralize(checked, "platform")
        ));
        return Ok(());
    }

    let detail = stale
        .iter()
        .map(|(platform, drifts)| {
            let lines = drifts
                .iter()
                .map(|drift| format!("    {drift}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("  {}\n{lines}", platform.key())
        })
        .collect::<Vec<_>>()
        .join("\n");

    Err(
        Error::conflict(format!("{} is out of date", kiln_config::LOCKFILE_FILE))
            .because(format!("These platforms would change:\n{detail}"))
            .command("kiln lock"),
    )
}

fn report(ui: &Ui, platform: &Platform, resolution: &Resolution) {
    ui.status(format!(
        "  {}",
        paint(&platform.key(), Style::Bold, ui.color())
    ));

    let width = resolution
        .runtimes
        .iter()
        .map(|runtime| runtime.display_name.len())
        .max()
        .unwrap_or(0);
    for runtime in &resolution.runtimes {
        ui.status(format!(
            "    {:<width$}  {}  {}",
            runtime.display_name,
            runtime.version,
            paint(
                if runtime.from_lockfile {
                    "unchanged"
                } else {
                    "resolved"
                },
                Style::Dim,
                ui.color()
            )
        ));
    }
}

fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(platforms: &[&str], all: bool, check: bool) -> LockArgs {
        LockArgs {
            platforms: platforms.iter().map(|s| s.to_string()).collect(),
            all_platforms: all,
            check,
        }
    }

    #[test]
    fn no_arguments_locks_the_host() {
        let targets = targets(&args(&[], false, false)).unwrap();
        assert_eq!(targets, vec![Platform::detect().unwrap()]);
    }

    #[test]
    fn all_platforms_covers_every_supported_one() {
        let targets = targets(&args(&[], true, false)).unwrap();
        assert_eq!(targets, Platform::all_supported());
        assert!(targets.len() >= 6);
    }

    #[test]
    fn explicit_platforms_are_taken_in_order() {
        let targets = targets(&args(&["linux-x86_64-gnu", "macos-aarch64"], false, false)).unwrap();
        let keys: Vec<String> = targets.iter().map(Platform::key).collect();
        assert_eq!(keys, ["linux-x86_64-gnu", "macos-aarch64"]);
    }

    #[test]
    fn a_repeated_platform_is_only_locked_once() {
        let targets = targets(&args(&["macos-aarch64", "macos-aarch64"], false, false)).unwrap();
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn an_unknown_platform_lists_the_real_ones() {
        let error = targets(&args(&["solaris-sparc"], false, false)).unwrap_err();
        let expected = error.expectation().expect("the supported list");
        assert!(expected.contains("macos-aarch64"), "{expected}");
        assert!(expected.contains("linux-x86_64-musl"), "{expected}");
    }

    #[test]
    fn all_platforms_wins_over_an_explicit_list() {
        let targets = targets(&args(&["macos-aarch64"], true, false)).unwrap();
        assert_eq!(targets.len(), Platform::all_supported().len());
    }

    #[test]
    fn counts_read_naturally() {
        assert_eq!(pluralize(1, "platform"), "1 platform");
        assert_eq!(pluralize(6, "platform"), "6 platforms");
    }
}
