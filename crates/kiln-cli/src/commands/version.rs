//! `kiln version`

use kiln_core::error::Result;
use kiln_core::{KILN_VERSION, KilnPaths, Platform, Ui};
use serde_json::json;

use crate::cli::VersionArgs;

pub fn run(args: &VersionArgs, ui: &Ui) -> Result<()> {
    let platform = Platform::detect();
    let paths = KilnPaths::discover();

    if args.json {
        let value = json!({
            "version": KILN_VERSION,
            "platform": platform.as_ref().map(|p| p.key()).unwrap_or_else(|_| "unknown".into()),
            "platform_supported": platform.as_ref().map(Platform::is_supported).unwrap_or(false),
            "home": paths.as_ref().ok().map(|p| p.root().display().to_string()),
            "lockfile_version": kiln_resolver::LOCKFILE_VERSION,
        });
        ui.data(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()));
        return Ok(());
    }

    ui.data(format!("kiln {KILN_VERSION}"));

    if ui.verbosity() > 0 || ui.quiet() {
        return Ok(());
    }

    ui.blank();
    let width = 10;
    match &platform {
        Ok(platform) => {
            let suffix = if platform.is_supported() {
                String::new()
            } else {
                " (runtime installation not supported)".to_string()
            };
            ui.field("platform", &format!("{platform}{suffix}"), width);
        }
        Err(error) => ui.field("platform", &format!("unknown — {}", error.summary()), width),
    }
    match &paths {
        Ok(paths) => ui.field("home", &paths.root().display().to_string(), width),
        Err(error) => ui.field("home", &format!("unknown — {}", error.summary()), width),
    }
    ui.field(
        "lockfile",
        &kiln_resolver::LOCKFILE_VERSION.to_string(),
        width,
    );

    Ok(())
}
