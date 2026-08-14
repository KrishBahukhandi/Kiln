//! One module per command.

pub mod cache;
pub mod clean;
pub mod doctor;
pub mod init;
pub mod install;
pub mod list;
pub mod lock;
pub mod progress;
pub mod run;
pub mod shell;
pub mod version;

use kiln_core::error::Result;

/// Load the project a command applies to, searching upwards from `directory`.
pub fn require_project(directory: &std::path::Path) -> Result<kiln_config::Project> {
    kiln_config::Project::discover(directory)
}

/// Format a byte count for a human.
///
/// Powers of 1024 with SI-style suffixes, which is what every developer tool
/// does and what `du -h` will agree with.
pub fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counts_read_the_way_du_prints_them() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(1536), "1.5 KB");
        assert_eq!(humanize_bytes(45_678_901), "43.6 MB");
        assert_eq!(humanize_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
