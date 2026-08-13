//! `kiln doctor` — say what is wrong, precisely.
//!
//! Doctor reports what it can check *today* and names what it cannot check yet,
//! rather than printing a row of green ticks that only cover the easy half. A
//! diagnostic tool that overstates its coverage is worse than no diagnostic
//! tool, because it converts "something is broken" into "nothing is broken".

use std::path::Path;

use kiln_core::error::{Error, Result};
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Platform, Ui};
use kiln_runtime::Registry;
use serde_json::json;

use crate::cli::DoctorArgs;
use crate::commands::humanize_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

struct Check {
    name: &'static str,
    status: Status,
    detail: String,
}

/// Checks that need a later phase before they can mean anything.
const PENDING: &[(&str, &str)] = &[("cache integrity", "Phase 3")];

pub fn run(args: &DoctorArgs, directory: &Path, ui: &Ui) -> Result<()> {
    let checks = collect(directory);
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();

    if args.json {
        let value = json!({
            "ok": failed == 0,
            "checks": checks.iter().map(|check| json!({
                "name": check.name,
                "status": check.status.as_str(),
                "detail": check.detail,
            })).collect::<Vec<_>>(),
            "not_checked": PENDING.iter().map(|(name, phase)| json!({
                "name": name,
                "available_in": phase,
            })).collect::<Vec<_>>(),
        });
        ui.data(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
    } else {
        report(ui, &checks);
    }

    if failed > 0 {
        return Err(Error::config(format!(
            "{failed} {} did not pass",
            if failed == 1 { "check" } else { "checks" }
        ))
        .because("The report above lists what Kiln found.")
        .hint("fix the first failure and run `kiln doctor` again"));
    }
    Ok(())
}

fn collect(directory: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let registry = Registry::builtin();

    // --- The project ------------------------------------------------------
    let manifest_path = kiln_config::find_manifest(directory);
    match &manifest_path {
        Ok(path) => checks.push(Check {
            name: "kiln.toml",
            status: Status::Ok,
            detail: path.display().to_string(),
        }),
        Err(error) => checks.push(Check {
            name: "kiln.toml",
            status: Status::Fail,
            detail: error.summary().to_string(),
        }),
    }

    let manifest = manifest_path
        .as_ref()
        .ok()
        .map(|path| kiln_config::parse_file(path));

    match &manifest {
        Some(Ok(manifest)) => checks.push(Check {
            name: "configuration",
            status: Status::Ok,
            detail: format!(
                "{} pinned, {} command(s)",
                pluralize(manifest.requirement_count(), "runtime"),
                manifest.commands.len()
            ),
        }),
        Some(Err(error)) => checks.push(Check {
            name: "configuration",
            status: Status::Fail,
            detail: error
                .location()
                .map(|location| {
                    format!(
                        "{}: {}",
                        location.display_path(),
                        location.label.as_deref().unwrap_or("invalid")
                    )
                })
                .unwrap_or_else(|| error.to_string()),
        }),
        None => {}
    }

    // --- The machine ------------------------------------------------------
    let platform = Platform::detect();
    match &platform {
        Ok(platform) if platform.is_supported() => checks.push(Check {
            name: "platform",
            status: Status::Ok,
            detail: format!("{platform} ({})", platform.key()),
        }),
        Ok(platform) => checks.push(Check {
            name: "platform",
            status: Status::Fail,
            detail: format!("{platform} is not supported by this release"),
        }),
        Err(error) => checks.push(Check {
            name: "platform",
            status: Status::Fail,
            detail: error.summary().to_string(),
        }),
    }

    // --- Runtimes the project asks for ------------------------------------
    if let (Some(Ok(manifest)), Ok(platform)) = (&manifest, &platform) {
        match kiln_resolver::plan(manifest, &registry, platform) {
            Ok(planned) => {
                let names: Vec<String> = planned
                    .iter()
                    .map(|entry| format!("{} {}", entry.display_name, entry.requirement))
                    .collect();
                checks.push(Check {
                    name: "runtimes",
                    status: Status::Ok,
                    detail: if names.is_empty() {
                        "none pinned".to_string()
                    } else {
                        names.join(", ")
                    },
                });
            }
            Err(error) => checks.push(Check {
                name: "runtimes",
                status: Status::Fail,
                detail: error.summary().to_string(),
            }),
        }
    }

    // --- What is actually installed ---------------------------------------
    //
    // The same view `kiln run` and `kiln shell` act on, so doctor cannot
    // disagree with them about whether the environment is usable.
    if let (Some(Ok(manifest)), Ok(platform), Ok(paths)) =
        (&manifest, &platform, KilnPaths::discover())
    {
        let store = kiln_cache::ContentStore::new(paths.store());
        let lockfile = manifest_path
            .as_ref()
            .ok()
            .and_then(|path| path.parent().map(|p| p.join(kiln_config::LOCKFILE_FILE)))
            .and_then(|path| kiln_resolver::Lockfile::read(&path).ok().flatten());

        let environment =
            kiln_resolver::activate(manifest, &registry, platform, &store, lockfile.as_ref());

        checks.push(if environment.is_ready() {
            let installed: Vec<String> = environment
                .runtimes
                .iter()
                .map(|runtime| {
                    format!(
                        "{} {}",
                        runtime.id,
                        runtime
                            .version
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default()
                    )
                })
                .collect();
            Check {
                name: "installed",
                status: Status::Ok,
                detail: if installed.is_empty() {
                    "nothing to install".to_string()
                } else {
                    installed.join(", ")
                },
            }
        } else {
            let missing: Vec<String> = environment
                .missing()
                .iter()
                .map(|runtime| match &runtime.version {
                    Some(version) => format!("{} {version}", runtime.id),
                    None => format!("{} {} (not resolved)", runtime.id, runtime.requirement),
                })
                .collect();
            Check {
                name: "installed",
                // A project that has simply not been installed is not broken.
                status: Status::Warn,
                detail: format!("{} missing — run `kiln install`", missing.join(", ")),
            }
        });
    }

    // --- Kiln's own storage -----------------------------------------------
    match KilnPaths::discover() {
        Ok(paths) if paths.exists() => {
            checks.push(Check {
                name: "kiln home",
                status: Status::Ok,
                detail: paths.root().display().to_string(),
            });

            let store = kiln_cache::ContentStore::new(paths.store());
            match (store.entries(), store.size_on_disk()) {
                (Ok(entries), Ok(size)) => checks.push(Check {
                    name: "store",
                    status: Status::Ok,
                    detail: format!(
                        "{} using {}",
                        pluralize(entries.len(), "artifact"),
                        humanize_bytes(size)
                    ),
                }),
                (Err(error), _) | (_, Err(error)) => checks.push(Check {
                    name: "store",
                    status: Status::Fail,
                    detail: error.to_string(),
                }),
            }
        }
        Ok(paths) => checks.push(Check {
            name: "kiln home",
            // Not an error: nothing has needed it yet.
            status: Status::Warn,
            detail: format!("{} does not exist yet", paths.root().display()),
        }),
        Err(error) => checks.push(Check {
            name: "kiln home",
            status: Status::Fail,
            detail: error.summary().to_string(),
        }),
    }

    checks
}

fn report(ui: &Ui, checks: &[Check]) {
    ui.banner("doctor");
    ui.blank();

    // One column width across both tables, so the report reads as one report.
    let width = checks
        .iter()
        .map(|c| c.name.len())
        .chain(PENDING.iter().map(|(name, _)| name.len()))
        .max()
        .unwrap_or(0);

    for check in checks {
        let mark = match check.status {
            Status::Ok => paint("✓", Style::Green, ui.color()),
            Status::Warn => paint("⚠", Style::Yellow, ui.color()),
            Status::Fail => paint("✗", Style::Red, ui.color()),
        };
        ui.status(format!(
            "{mark} {:<width$}  {}",
            check.name,
            paint(&check.detail, Style::Dim, ui.color())
        ));
    }

    ui.blank();
    ui.section("Not checked by this release");
    for (name, phase) in PENDING {
        ui.field(name, phase, width);
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

    #[test]
    fn counts_read_naturally() {
        assert_eq!(pluralize(0, "runtime"), "0 runtimes");
        assert_eq!(pluralize(1, "runtime"), "1 runtime");
        assert_eq!(pluralize(2, "artifact"), "2 artifacts");
    }

    #[test]
    fn a_directory_with_no_project_fails_the_manifest_check() {
        let directory = std::env::temp_dir().join(format!("kiln-doctor-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();

        let checks = collect(&directory);
        let manifest_check = checks.iter().find(|c| c.name == "kiln.toml").unwrap();
        assert_eq!(manifest_check.status, Status::Fail);

        // The platform check runs regardless of whether there is a project.
        assert!(checks.iter().any(|c| c.name == "platform"));

        std::fs::remove_dir_all(&directory).ok();
    }
}
