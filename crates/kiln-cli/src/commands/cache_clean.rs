//! `kiln cache clean` — evict runtimes nothing has used recently.
//!
//! # Why by age, and not by reachability
//!
//! The question garbage collection wants to answer is "which entries does some
//! project still need?". Kiln cannot answer it. It keeps no registry of
//! projects — that is a deliberate design choice, not an oversight — so a
//! lockfile on a disconnected drive, a checkout under a colleague's home
//! directory, or a repository not cloned yet are all invisible.
//!
//! Rather than invent a registry and quietly become stateful, Kiln records when
//! each entry was last put to use and evicts by age. That answers a slightly
//! different and much more honest question: "has anything needed this lately?"
//!
//! Being wrong is cheap in one direction and only in one direction. Evicting
//! something still wanted costs a re-download; keeping something unwanted costs
//! disk. So the default is conservative, the command reports before it acts, and
//! deleting requires `--force`.

use kiln_core::error::Result;
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Ui};

use crate::commands::humanize_bytes;

/// Seconds in a day.
const DAY: u64 = 24 * 60 * 60;

pub fn run(older_than_days: u64, all: bool, force: bool, ui: &Ui) -> Result<()> {
    let paths = KilnPaths::discover()?;
    let store = kiln_cache::ContentStore::new(paths.store());

    let entries = store.entries()?;
    if entries.is_empty() {
        ui.status("The store is empty.");
        return Ok(());
    }

    let now = kiln_cache::unix_now();
    let cutoff = older_than_days.saturating_mul(DAY);

    let (condemned, kept): (Vec<_>, Vec<_>) = entries.into_iter().partition(|entry| {
        all || entry
            .idle_seconds(now)
            // An entry with no timestamp at all has no evidence of use, so it
            // is a candidate — but only because `last_touched` already falls
            // back to the install time, which every entry has.
            .is_some_and(|idle| idle >= cutoff)
    });

    if condemned.is_empty() {
        ui.status(format!(
            "{} Nothing to remove: {} in use within the last {}.",
            paint("✓", Style::Green, ui.color()),
            pluralize(kept.len(), "runtime"),
            pluralize(older_than_days as usize, "day"),
        ));
        return Ok(());
    }

    let width = condemned
        .iter()
        .map(|entry| describe(entry).0.len())
        .max()
        .unwrap_or(0);

    let mut freed = 0u64;
    for entry in &condemned {
        let (name, age) = describe(entry);
        ui.status(format!(
            "  {:<width$}  {}",
            name,
            paint(&age, Style::Dim, ui.color())
        ));

        if force {
            freed += store.remove(&entry.digest)?;
        }
    }

    ui.blank();
    if !force {
        let total: u64 = condemned
            .iter()
            .map(|entry| store.entry_size(&entry.digest))
            .sum();

        ui.note(format!(
            "  {} would be removed, freeing {}. Nothing was deleted.",
            pluralize(condemned.len(), "runtime"),
            humanize_bytes(total)
        ));
        ui.status(format!(
            "  {}",
            paint("kiln cache clean --force", Style::Cyan, ui.color())
        ));
        return Ok(());
    }

    ui.ok(format!(
        "Removed {}, freeing {}",
        pluralize(condemned.len(), "runtime"),
        humanize_bytes(freed)
    ));
    if !kept.is_empty() {
        ui.note(format!("  {} kept.", pluralize(kept.len(), "runtime")));
    }
    ui.note("  Anything removed by mistake comes back with `kiln install`.");
    Ok(())
}

/// A store entry as a name and an age, for display.
fn describe(entry: &kiln_cache::StoreEntry) -> (String, String) {
    let name = match &entry.meta {
        Some(meta) => format!("{} {}", meta.provider, meta.version),
        None => entry.digest.short(),
    };

    let age = match entry.idle_seconds(kiln_cache::unix_now()) {
        Some(idle) => {
            let days = idle / DAY;
            match days {
                0 => "used today".to_string(),
                1 => "unused for 1 day".to_string(),
                _ => format!("unused for {days} days"),
            }
        }
        None => "never used".to_string(),
    };
    (name, age)
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
        assert_eq!(pluralize(1, "runtime"), "1 runtime");
        assert_eq!(pluralize(0, "runtime"), "0 runtimes");
        assert_eq!(pluralize(30, "day"), "30 days");
    }
}
