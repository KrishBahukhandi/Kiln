//! Commands whose behaviour belongs to a later phase.
//!
//! Each still does the work it *can* do first — finding and validating the
//! project — so that the error a user gets is the most useful one available. If
//! there is no `kiln.toml`, that is the real problem and Kiln says so; only a
//! project that is otherwise fine reaches the message about the missing phase.

use std::path::Path;

use kiln_core::error::{Error, Result};

use crate::commands::require_project;

/// `kiln clean`
pub fn clean(directory: &Path) -> Result<()> {
    require_project(directory)?;
    Err(
        Error::not_implemented("`kiln clean`", "Phase 5 (developer experience)")
            .hint("Kiln does not yet create any project-local state to remove")
            .command("kiln cache list"),
    )
}
