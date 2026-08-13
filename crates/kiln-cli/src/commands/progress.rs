//! Showing an install happening.
//!
//! Progress bars go to stderr, like all narration, so `kiln install` can be
//! redirected without a stream of escape sequences ending up in a log file. When
//! stderr is not a terminal, or `--quiet` is set, the bars are replaced with
//! plain one-line-per-event output — a progress bar written to a CI log is
//! thousands of useless lines.

use indicatif::{ProgressBar, ProgressStyle};
use kiln_cache::StoreEntry;
use kiln_core::Ui;
use kiln_core::ui::{Style, paint};
use kiln_net::Progress;
use kiln_resolver::{Observer, ResolvedRuntime};

/// Draws install progress for a person at a terminal.
pub struct CliObserver {
    ui: Ui,
    /// Whether bars can be drawn at all.
    animated: bool,
}

impl CliObserver {
    /// Build an observer suited to where the output is going.
    pub fn new(ui: &Ui) -> Self {
        let animated = !ui.quiet() && std::io::IsTerminal::is_terminal(&std::io::stderr());
        CliObserver {
            ui: ui.clone(),
            animated,
        }
    }

    fn label(runtime: &ResolvedRuntime) -> String {
        format!("{} {}", runtime.display_name, runtime.version)
    }
}

impl Observer for CliObserver {
    fn reused(&mut self, runtime: &ResolvedRuntime) {
        self.ui.status(format!(
            "  {} {:<24} {}",
            paint("✓", Style::Green, self.ui.color()),
            Self::label(runtime),
            paint("already in the store", Style::Dim, self.ui.color())
        ));
    }

    fn downloading(&mut self, runtime: &ResolvedRuntime) -> Box<dyn Progress> {
        let label = Self::label(runtime);
        if !self.animated {
            self.ui.status(format!("  ↓ {label}"));
            return Box::new(LineProgress {
                ui: self.ui.clone(),
                label,
            });
        }

        let bar = ProgressBar::new_spinner();
        bar.set_message(label);
        Box::new(BarProgress { bar })
    }

    fn unpacking(&mut self, runtime: &ResolvedRuntime) {
        if !self.animated {
            self.ui
                .status(format!("  ⇱ unpacking {}", Self::label(runtime)));
        }
    }

    fn installed(&mut self, runtime: &ResolvedRuntime, entry: &StoreEntry) {
        let detail = if self.ui.verbosity() > 0 {
            entry.digest.to_string()
        } else {
            "installed".to_string()
        };
        self.ui.status(format!(
            "  {} {:<24} {}",
            paint("✓", Style::Green, self.ui.color()),
            Self::label(runtime),
            paint(&detail, Style::Dim, self.ui.color())
        ));
    }
}

/// A live progress bar.
struct BarProgress {
    bar: ProgressBar,
}

impl Progress for BarProgress {
    fn start(&mut self, total: Option<u64>) {
        match total {
            Some(total) => {
                self.bar.set_length(total);
                // `unwrap_or_else` rather than `unwrap`: a malformed template is
                // a cosmetic problem, and must never take down an install.
                let style = ProgressStyle::with_template(
                    "  {spinner:.cyan} {msg:<24} {bar:24.cyan/blue} {bytes:>10}/{total_bytes:<10} {eta:>5}",
                )
                .unwrap_or_else(|_| ProgressStyle::default_bar());
                self.bar.set_style(style.progress_chars("━━╾"));
            }
            None => {
                let style = ProgressStyle::with_template("  {spinner:.cyan} {msg:<24} {bytes:>10}")
                    .unwrap_or_else(|_| ProgressStyle::default_spinner());
                self.bar.set_style(style);
            }
        }
        self.bar
            .enable_steady_tick(std::time::Duration::from_millis(100));
    }

    fn advance(&mut self, bytes: u64) {
        self.bar.inc(bytes);
    }

    fn finish(&mut self) {
        // Cleared, not left on screen: the `✓` line that follows says the same
        // thing without the leftover bar.
        self.bar.finish_and_clear();
    }
}

/// Progress for somewhere a bar would be noise.
struct LineProgress {
    ui: Ui,
    label: String,
}

impl Progress for LineProgress {
    fn start(&mut self, total: Option<u64>) {
        if let Some(total) = total {
            self.ui.status(format!(
                "    {} ({})",
                self.label,
                crate::commands::humanize_bytes(total)
            ));
        }
    }
    fn advance(&mut self, _bytes: u64) {}
    fn finish(&mut self) {}
}
