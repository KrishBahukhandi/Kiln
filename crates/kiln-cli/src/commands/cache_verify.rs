//! `kiln cache verify` — check stored runtimes against what was installed.
//!
//! # What is being compared against what
//!
//! A store entry is named by the digest of the archive it came from, which is
//! the right name but not one that can be recomputed later — the archive is
//! gone, and only the unpacked tree remains. So verification compares the tree
//! against a manifest Kiln wrote at install time, in the moment after the
//! archive's published digest checked out. See `kiln_cache::tree`.
//!
//! # Why damage is not an error
//!
//! Finding a corrupt entry is this command succeeding. The command reports every
//! entry it checked and only *then* exits non-zero, so a store with one bad
//! runtime still tells you about the other nine. The `Err` path is reserved for
//! being unable to look at all.

use kiln_cache::{ContentStore, StoreEntry, Verification};
use kiln_core::error::{Error, Result};
use kiln_core::ui::{Style, paint};
use kiln_core::{ErrorKind, KilnPaths, Ui};
use serde_json::json;

use crate::commands::humanize_bytes;

/// How many differing paths to print per entry before summarising.
///
/// A tree with a thousand differences means the whole entry is gone or was
/// replaced wholesale; printing all thousand buries that conclusion in scroll.
const MAX_SHOWN: usize = 12;

pub fn run(selectors: &[String], as_json: bool, ui: &Ui) -> Result<()> {
    let paths = KilnPaths::discover()?;
    let store = ContentStore::new(paths.store());

    let entries = select(&store, selectors)?;
    if entries.is_empty() {
        if as_json {
            ui.data(render_json(&[]));
        } else {
            ui.status("The store is empty.");
            ui.note("Runtimes appear here after `kiln install`.");
        }
        return Ok(());
    }

    if !as_json {
        ui.status(format!(
            "Checking {} against {} recorded at install.",
            pluralize(entries.len(), "runtime"),
            if entries.len() == 1 {
                "the manifest"
            } else {
                "the manifests"
            }
        ));
        ui.blank();
    }

    let width = entries.iter().map(|e| name_of(e).len()).max().unwrap_or(0);

    let mut results = Vec::with_capacity(entries.len());
    for entry in &entries {
        let verification = store.verify(&entry.digest)?;
        if !as_json {
            report_one(entry, &verification, width, ui);
        }
        results.push((entry, verification));
    }

    if as_json {
        ui.data(render_json(&results));
    } else {
        summarise(&results, ui);
    }

    // Reported first, then failed — so `kiln cache verify` in a script both
    // prints the detail and sets a status a script can branch on.
    if results.iter().any(|(_, v)| v.is_damaged()) {
        return Err(damaged_error(&results));
    }
    Ok(())
}

/// The entries a set of selectors names, or all of them when there are none.
///
/// A selector matches a digest by prefix or a runtime by name, because nobody
/// wants to type sixty-four hex characters and `node` is what people actually
/// mean.
fn select(store: &ContentStore, selectors: &[String]) -> Result<Vec<StoreEntry>> {
    let all = store.entries()?;
    if selectors.is_empty() {
        return Ok(all);
    }

    let mut chosen: Vec<StoreEntry> = Vec::new();
    for selector in selectors {
        let matches: Vec<&StoreEntry> = all
            .iter()
            .filter(|entry| matches_selector(entry, selector))
            .collect();

        if matches.is_empty() {
            return Err(
                Error::not_found(format!("Nothing in the store matches `{selector}`"))
                    .because("It is not a runtime name, nor the start of a stored digest.")
                    .command("kiln cache list"),
            );
        }
        for entry in matches {
            if !chosen.iter().any(|kept| kept.digest == entry.digest) {
                chosen.push(entry.clone());
            }
        }
    }
    Ok(chosen)
}

fn matches_selector(entry: &StoreEntry, selector: &str) -> bool {
    if entry.digest.hex().starts_with(selector) || entry.digest.to_string() == selector {
        return true;
    }
    entry.meta.as_ref().is_some_and(|meta| {
        meta.provider == selector || format!("{} {}", meta.provider, meta.version) == selector
    })
}

fn name_of(entry: &StoreEntry) -> String {
    match &entry.meta {
        Some(meta) => format!("{} {}", meta.provider, meta.version),
        None => entry.digest.short(),
    }
}

fn report_one(entry: &StoreEntry, verification: &Verification, width: usize, ui: &Ui) {
    let name = name_of(entry);
    let color = ui.color();

    match verification {
        Verification::Intact { files, bytes } => ui.status(format!(
            "  {} {:<width$}  {}",
            paint("✓", Style::Green, color),
            name,
            paint(
                &format!("{}, {}", pluralize(*files, "file"), humanize_bytes(*bytes)),
                Style::Dim,
                color
            ),
        )),
        Verification::Unverifiable { reason } => ui.status(format!(
            "  {} {:<width$}  {}",
            paint("?", Style::Yellow, color),
            name,
            paint(reason, Style::Dim, color),
        )),
        Verification::Damaged { differences } => {
            ui.status(format!(
                "  {} {:<width$}  {}",
                paint("✗", Style::Red, color),
                name,
                paint(
                    &format!(
                        "{} {}",
                        pluralize(differences.len(), "path"),
                        if differences.len() == 1 {
                            "differs"
                        } else {
                            "differ"
                        }
                    ),
                    Style::Red,
                    color
                ),
            ));
            let path_width = differences
                .iter()
                .take(MAX_SHOWN)
                .map(|d| d.path.len())
                .max()
                .unwrap_or(0);

            for difference in differences.iter().take(MAX_SHOWN) {
                ui.status(format!(
                    "      {:<path_width$}  {}",
                    difference.path,
                    paint(&difference.detail, Style::Dim, color)
                ));
            }
            if differences.len() > MAX_SHOWN {
                ui.status(format!(
                    "      {}",
                    paint(
                        &format!("… and {} more", differences.len() - MAX_SHOWN),
                        Style::Dim,
                        color
                    )
                ));
            }
        }
    }
}

fn summarise(results: &[(&StoreEntry, Verification)], ui: &Ui) {
    let intact = results.iter().filter(|(_, v)| v.is_intact()).count();
    let damaged = results.iter().filter(|(_, v)| v.is_damaged()).count();
    let unknown = results.len() - intact - damaged;

    ui.blank();
    if damaged == 0 && unknown == 0 {
        ui.ok(format!("{} intact", pluralize(intact, "runtime")));
        return;
    }

    if unknown > 0 && damaged == 0 {
        ui.warn(format!(
            "{} could not be checked",
            pluralize(unknown, "runtime")
        ));
        ui.note("  Entries installed before Kiln recorded manifests have nothing to compare");
        ui.note("  against. Reinstalling records one.");
    }
}

/// The failure returned once every entry has been reported.
fn damaged_error(results: &[(&StoreEntry, Verification)]) -> Error {
    let names: Vec<String> = results
        .iter()
        .filter(|(_, v)| v.is_damaged())
        .map(|(entry, _)| name_of(entry))
        .collect();

    let subject = if names.len() == 1 {
        format!("{} no longer matches", names[0])
    } else {
        format!("{} runtimes no longer match", names.len())
    };

    Error::new(
        ErrorKind::Verification,
        format!("{subject} what was installed"),
    )
    .because(format!(
        "The files on disk for {} differ from the manifest Kiln recorded when the \
         archive was unpacked and its digest verified.",
        names.join(", ")
    ))
    .expected("every file byte-for-byte as the publisher shipped it")
    .hint("remove the affected runtimes and install them again")
    .command(String::from(
        "kiln cache clean --all --force && kiln install",
    ))
}

fn render_json(results: &[(&StoreEntry, Verification)]) -> String {
    let value = json!({
        "entries": results.iter().map(|(entry, verification)| {
            let mut object = json!({
                "digest": entry.digest.to_string(),
                "runtime": entry.meta.as_ref().map(|m| m.provider.clone()),
                "version": entry.meta.as_ref().map(|m| m.version.clone()),
            });
            let map = object.as_object_mut().expect("just built as an object");
            match verification {
                Verification::Intact { files, bytes } => {
                    map.insert("status".into(), json!("intact"));
                    map.insert("files".into(), json!(files));
                    map.insert("bytes".into(), json!(bytes));
                }
                Verification::Unverifiable { reason } => {
                    map.insert("status".into(), json!("unverifiable"));
                    map.insert("reason".into(), json!(reason));
                }
                Verification::Damaged { differences } => {
                    map.insert("status".into(), json!("damaged"));
                    map.insert("differences".into(), json!(differences.iter().map(|d| json!({
                        "path": d.path,
                        "kind": d.kind.as_str(),
                        "detail": d.detail,
                    })).collect::<Vec<_>>()));
                }
            }
            object
        }).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
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
    use kiln_cache::EntryMeta;
    use kiln_core::{Digest, HashAlgorithm};

    fn entry(provider: &str, version: &str, seed: &[u8]) -> StoreEntry {
        let digest = Digest::of_bytes(HashAlgorithm::Sha256, seed);
        StoreEntry {
            meta: Some(EntryMeta::new(
                provider,
                version,
                "https://example.invalid/x.tar.gz",
                &digest,
                0,
            )),
            digest,
            path: std::path::PathBuf::from("/store/x"),
            last_used: None,
        }
    }

    #[test]
    fn a_selector_matches_a_runtime_by_name() {
        let node = entry("node", "22.14.0", b"a");
        assert!(matches_selector(&node, "node"));
        assert!(matches_selector(&node, "node 22.14.0"));
        assert!(!matches_selector(&node, "python"));
    }

    #[test]
    fn a_selector_matches_a_digest_by_prefix() {
        let node = entry("node", "22.14.0", b"a");
        let hex = node.digest.hex();

        // Nobody types sixty-four hex characters.
        assert!(matches_selector(&node, &hex[..8]));
        assert!(matches_selector(&node, &node.digest.to_string()));
        assert!(!matches_selector(&node, "ffffffff"));
    }

    #[test]
    fn a_damaged_entry_produces_a_verification_failure() {
        let node = entry("node", "22.14.0", b"a");
        let damaged = Verification::Damaged {
            differences: vec![kiln_cache::Difference {
                path: "bin/node".into(),
                kind: kiln_cache::DifferenceKind::ContentChanged,
                detail: "same size, different contents".into(),
            }],
        };

        let error = damaged_error(&[(&node, damaged)]);
        // Exit code 6, so CI can tell a corrupt store from a missing runtime.
        assert_eq!(error.kind(), ErrorKind::Verification);
        assert!(error.summary().contains("node 22.14.0"));
        assert!(error.summary().contains("no longer matches"));
    }

    #[test]
    fn several_damaged_entries_read_as_a_count() {
        let node = entry("node", "22.14.0", b"a");
        let python = entry("python", "3.13.15", b"b");
        let damaged = || Verification::Damaged {
            differences: vec![kiln_cache::Difference {
                path: "x".into(),
                kind: kiln_cache::DifferenceKind::Missing,
                detail: "gone".into(),
            }],
        };

        let error = damaged_error(&[(&node, damaged()), (&python, damaged())]);
        assert!(error.summary().contains("2 runtimes no longer match"));
    }

    #[test]
    fn json_reports_the_status_of_every_entry() {
        let node = entry("node", "22.14.0", b"a");
        let python = entry("python", "3.13.15", b"b");

        let rendered = render_json(&[
            (
                &node,
                Verification::Intact {
                    files: 12,
                    bytes: 34,
                },
            ),
            (
                &python,
                Verification::Unverifiable {
                    reason: "no manifest".into(),
                },
            ),
        ]);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        let entries = parsed["entries"].as_array().unwrap();

        assert_eq!(entries[0]["status"], "intact");
        assert_eq!(entries[0]["files"], 12);
        assert_eq!(entries[1]["status"], "unverifiable");
    }

    #[test]
    fn json_for_an_empty_store_is_still_valid_json() {
        // A script that always parses is worth more than one that special-cases
        // "nothing installed".
        let parsed: serde_json::Value = serde_json::from_str(&render_json(&[])).unwrap();
        assert!(parsed["entries"].as_array().unwrap().is_empty());
    }
}
