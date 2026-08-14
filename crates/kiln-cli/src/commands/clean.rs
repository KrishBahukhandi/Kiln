//! `kiln clean` — remove Kiln's scratch space and cached indexes.
//!
//! Deliberately *not* a way to delete installed runtimes. Two things here are
//! safe to remove because Kiln can rebuild both without asking anyone:
//!
//! - `staging/` — directories left by an install that was interrupted. A live
//!   install cleans up after itself, so anything here belongs to a run that is
//!   no longer happening.
//! - `state/http/` — cached release indexes, which are an optimisation and
//!   nothing more.
//!
//! Removing an installed runtime is `kiln cache clean`, and it is a different
//! question with a harder answer: which entries is some other project still
//! relying on?
//!
//! Reports by default and deletes only with `--force`, because a command whose
//! whole job is deleting things should make you say so.

use kiln_core::error::Result;
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Ui};

use crate::cli::CleanArgs;
use crate::commands::humanize_bytes;

pub fn run(args: &CleanArgs, ui: &Ui) -> Result<()> {
    let paths = KilnPaths::discover()?;

    let staging = paths.staging();
    let indexes = paths.state().join("http");

    let targets = [
        ("interrupted installs", staging.clone()),
        ("cached release indexes", indexes.clone()),
    ];

    let mut total = 0u64;
    let mut found = false;

    for (label, path) in &targets {
        let (entries, bytes) = measure(path);
        if entries == 0 {
            continue;
        }
        found = true;
        total += bytes;
        ui.status(format!(
            "  {:<24} {} in {}",
            label,
            paint(&humanize_bytes(bytes), Style::Bold, ui.color()),
            paint(&path.display().to_string(), Style::Dim, ui.color())
        ));
    }

    if !found {
        ui.status("Nothing to clean.");
        return Ok(());
    }

    if !args.force {
        ui.blank();
        ui.note(format!(
            "  {} would be freed. Nothing was removed.",
            humanize_bytes(total)
        ));
        ui.status(format!(
            "  {}",
            paint("kiln clean --force", Style::Cyan, ui.color())
        ));
        return Ok(());
    }

    let removed = kiln_resolver::clean_staging(&paths)?;
    if indexes.exists() {
        // A cache that cannot be cleared is not a reason to fail the command;
        // it will simply expire on its own.
        let _ = std::fs::remove_dir_all(&indexes);
    }

    ui.blank();
    ui.ok(format!(
        "Freed {} ({} interrupted install(s) removed)",
        humanize_bytes(total),
        removed
    ));
    ui.note("  Installed runtimes were not touched; see `kiln cache list`.");
    Ok(())
}

/// How many entries a directory holds, and how much they take up.
fn measure(path: &std::path::Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return (0, 0);
    };

    let mut count = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        count += 1;
        bytes += size_of(&entry.path());
    }
    (count, bytes)
}

fn size_of(path: &std::path::Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }

    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| size_of(&entry.path()))
                .sum::<u64>()
        })
        .unwrap_or(0)
}
