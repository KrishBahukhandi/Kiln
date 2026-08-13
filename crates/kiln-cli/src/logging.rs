//! Diagnostic logging, separate from user-facing output.
//!
//! `tracing` output is for people debugging Kiln. Everything a user is meant to
//! read goes through [`kiln_core::Ui`]. Both write to stderr, but only one of
//! them is a product surface.

use tracing_subscriber::EnvFilter;

/// Environment variable that turns logging on and sets its level.
const LOG_ENV: &str = "KILN_LOG";

/// Install the log subscriber, if this invocation wants logging at all.
///
/// A quiet run installs nothing. Simple commands are supposed to finish in
/// milliseconds, and there is no reason to pay for a subscriber that will never
/// be asked to print anything.
pub fn init(verbosity: u8) {
    let filter = match std::env::var(LOG_ENV) {
        Ok(value) if !value.trim().is_empty() => EnvFilter::new(value),
        _ => match verbosity {
            0 => return,
            1 => EnvFilter::new("kiln=info"),
            2 => EnvFilter::new("kiln=debug"),
            _ => EnvFilter::new("debug"),
        },
    };

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(verbosity > 2)
        .without_time()
        .try_init();
}
