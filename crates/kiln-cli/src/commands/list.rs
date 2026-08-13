//! `kiln list` — what this project uses, and whether it is installed.
//!
//! Entirely offline. It shows the same view of the environment that `kiln run`
//! and `kiln shell` act on, so "list says it is installed" and "run works"
//! cannot disagree.

use std::path::Path;

use kiln_core::error::Result;
use kiln_core::ui::{Style, paint};
use kiln_core::{KilnPaths, Platform, Ui};
use kiln_resolver::Lockfile;
use kiln_runtime::Registry;
use serde_json::json;

use crate::cli::ListArgs;
use crate::commands::require_project;

pub fn run(args: &ListArgs, directory: &Path, ui: &Ui) -> Result<()> {
    let project = require_project(directory)?;
    let manifest = project.manifest();
    let platform = Platform::detect()?;
    let paths = KilnPaths::discover()?;
    let store = kiln_cache::ContentStore::new(paths.store());

    let environment = kiln_resolver::activate(
        manifest,
        &Registry::builtin(),
        &platform,
        &store,
        Lockfile::read(&project.lockfile_path())?.as_ref(),
    );

    if args.json {
        let value = json!({
            "project": manifest.project.name.as_str(),
            "platform": platform.key(),
            "ready": environment.is_ready(),
            "runtimes": environment.runtimes.iter().map(|runtime| json!({
                "id": runtime.id,
                "name": runtime.display_name,
                "requirement": runtime.requirement.to_string(),
                "version": runtime.version.as_ref().map(ToString::to_string),
                "installed": runtime.is_installed(),
                "state": runtime.state(),
                "bin_dirs": runtime.bin_dirs.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        ui.data(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
        return Ok(());
    }

    if environment.runtimes.is_empty() {
        ui.status("This project pins no runtimes.");
        return Ok(());
    }

    let rows: Vec<[String; 4]> = environment
        .runtimes
        .iter()
        .map(|runtime| {
            [
                runtime.display_name.clone(),
                runtime.requirement.to_string(),
                runtime
                    .version
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "-".to_string()),
                runtime.state().to_string(),
            ]
        })
        .collect();

    let headers = ["RUNTIME", "REQUIRED", "VERSION", "STATE"];
    let widths: Vec<usize> = (0..4)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].len())
                .chain(std::iter::once(headers[column].len()))
                .max()
                .unwrap_or(0)
        })
        .collect();

    // stdout, because this is data: `kiln list | grep node` should work.
    ui.data(format_row(&headers.map(String::from), &widths));
    for row in &rows {
        ui.data(format_row(row, &widths));
    }

    if !environment.is_ready() {
        ui.blank();
        ui.status(format!(
            "{} {}",
            paint("→", Style::Dim, ui.color()),
            paint(
                "run `kiln install` to install what is missing",
                Style::Dim,
                ui.color()
            )
        ));
    }

    Ok(())
}

fn format_row(cells: &[String; 4], widths: &[usize]) -> String {
    let mut line = String::new();
    for (index, cell) in cells.iter().enumerate() {
        if index == cells.len() - 1 {
            line.push_str(cell);
        } else {
            line.push_str(&format!("{cell:<width$}  ", width = widths[index]));
        }
    }
    line
}
