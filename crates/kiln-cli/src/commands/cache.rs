//! `kiln cache` — look inside the content-addressed store.

use kiln_core::error::Result;
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Ui};
use serde_json::json;

use crate::cli::{CacheArgs, CacheCommand};
use crate::commands::humanize_bytes;

pub fn run(args: &CacheArgs, ui: &Ui) -> Result<()> {
    let paths = KilnPaths::discover()?;
    let store = kiln_cache::ContentStore::new(paths.store());

    match &args.command {
        CacheCommand::List { json: as_json } => list(&store, ui, *as_json),
        CacheCommand::Verify => store.verify(&placeholder()).map(|_| ()),
        CacheCommand::Clean { .. } => store.collect_garbage(&[]),
    }
}

fn list(store: &kiln_cache::ContentStore, ui: &Ui, as_json: bool) -> Result<()> {
    let entries = store.entries()?;

    if as_json {
        let value = json!({
            "store": store.root().display().to_string(),
            "artifacts": entries.iter().map(|entry| json!({
                "digest": entry.digest.to_string(),
                "path": entry.path.display().to_string(),
                "provider": entry.meta.as_ref().map(|m| m.provider.clone()),
                "version": entry.meta.as_ref().map(|m| m.version.clone()),
                "url": entry.meta.as_ref().map(|m| m.url.clone()),
                "artifact_bytes": entry.meta.as_ref().map(|m| m.artifact_bytes),
                "installed_unix": entry.meta.as_ref().map(|m| m.installed_unix),
            })).collect::<Vec<_>>(),
        });
        ui.data(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
        return Ok(());
    }

    if entries.is_empty() {
        ui.status(format!(
            "The store at {} is empty.",
            paint(&store.root().display().to_string(), Style::Dim, ui.color())
        ));
        ui.note("Runtimes appear here after `kiln install`.");
        return Ok(());
    }

    // Provenance rather than paths: a column of 90-character store paths is
    // technically complete and practically unreadable.
    let rows: Vec<(String, String, String)> = entries
        .iter()
        .map(|entry| {
            let (runtime, version) = match &entry.meta {
                Some(meta) => (meta.provider.clone(), meta.version.clone()),
                None => ("unknown".to_string(), "-".to_string()),
            };
            (runtime, version, entry.digest.short())
        })
        .collect();

    let runtime_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(7).max(7);
    let version_width = rows.iter().map(|r| r.1.len()).max().unwrap_or(7).max(7);

    ui.data(format!(
        "{:<runtime_width$}  {:<version_width$}  {}",
        "RUNTIME", "VERSION", "DIGEST"
    ));
    for (runtime, version, digest) in &rows {
        ui.data(format!(
            "{runtime:<runtime_width$}  {version:<version_width$}  {digest}"
        ));
    }

    ui.blank();
    ui.status(format!(
        "{} artifact(s), {} in {}",
        entries.len(),
        humanize_bytes(store.size_on_disk()?),
        store.root().display()
    ));
    Ok(())
}

/// `cache verify` will take a digest once there is anything to verify; until
/// then the call exists to route through the store and report Phase 3 honestly.
fn placeholder() -> kiln_core::Digest {
    kiln_core::Digest::of_bytes(kiln_core::HashAlgorithm::Sha256, b"")
}
