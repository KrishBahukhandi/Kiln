//! Terminal output.
//!
//! Two rules govern every write Kiln makes:
//!
//! - **stdout carries data.** Command results, tables, JSON, and the output of
//!   any process Kiln runs on the user's behalf. `kiln list --json | jq` and
//!   `kiln run node -e '…' | wc -l` must both work.
//! - **stderr carries narration.** Progress, summaries, warnings and errors.
//!
//! Formatting is separated from printing: [`format_error`] is a pure function,
//! so the error presentation is unit-tested rather than eyeballed.

use std::io::{IsTerminal, Write};

use crate::error::{Error, Hint};
use crate::source::SourceLocation;

/// When to emit ANSI escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Colour when stderr is a terminal and the environment permits it.
    #[default]
    Auto,
    /// Always colour, even when redirected.
    Always,
    /// Never colour.
    Never,
}

impl ColorChoice {
    fn resolve(self) -> bool {
        match self {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            // https://no-color.org and the informal CLICOLOR_FORCE convention.
            ColorChoice::Auto => {
                if std::env::var_os("NO_COLOR").is_some() {
                    false
                } else if std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
                    true
                } else {
                    std::io::stderr().is_terminal()
                }
            }
        }
    }
}

/// ANSI styles Kiln uses. Kept small on purpose: a tool that speaks in six
/// colours is a tool nobody can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// De-emphasised text: labels, gutters, secondary detail.
    Dim,
    /// Emphasis for headings and key values.
    Bold,
    /// Failure.
    Red,
    /// Warning.
    Yellow,
    /// Success.
    Green,
    /// Structural accents.
    Cyan,
}

impl Style {
    const fn code(self) -> &'static str {
        match self {
            Style::Dim => "2",
            Style::Bold => "1",
            Style::Red => "31",
            Style::Yellow => "33",
            Style::Green => "32",
            Style::Cyan => "36",
        }
    }
}

/// Apply a style, or return the text unchanged when colour is off.
pub fn paint(text: &str, style: Style, color: bool) -> String {
    if color {
        format!("\u{1b}[{}m{}\u{1b}[0m", style.code(), text)
    } else {
        text.to_string()
    }
}

/// The output layer. Cheap to clone and pass around.
#[derive(Debug, Clone)]
pub struct Ui {
    color: bool,
    quiet: bool,
    verbosity: u8,
}

impl Ui {
    /// Build an output layer from the global CLI flags.
    pub fn new(choice: ColorChoice, quiet: bool, verbosity: u8) -> Self {
        Ui {
            color: choice.resolve(),
            quiet,
            verbosity,
        }
    }

    /// A non-colouring, non-narrating instance, for tests and JSON modes.
    pub fn silent() -> Self {
        Ui {
            color: false,
            quiet: true,
            verbosity: 0,
        }
    }

    /// Whether ANSI escapes are being emitted.
    pub fn color(&self) -> bool {
        self.color
    }

    /// Whether narration is suppressed. Errors are printed regardless.
    pub fn quiet(&self) -> bool {
        self.quiet
    }

    /// How many `-v` flags were given.
    pub fn verbosity(&self) -> u8 {
        self.verbosity
    }

    /// Whether Kiln may prompt: both the question and the answer need a terminal.
    pub fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    /// Write a line of *data* to stdout.
    pub fn data(&self, line: impl AsRef<str>) {
        let mut stdout = std::io::stdout().lock();
        // A closed pipe (`kiln list | head`) is not an error worth reporting.
        let _ = writeln!(stdout, "{}", line.as_ref());
    }

    /// Write a line of narration to stderr, unless quietened.
    pub fn status(&self, line: impl AsRef<str>) {
        if self.quiet {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{}", line.as_ref());
    }

    /// A blank narration line.
    pub fn blank(&self) {
        self.status("");
    }

    /// The command banner, e.g. `◆ Kiln`.
    pub fn banner(&self, subtitle: &str) {
        let mark = paint("◆", Style::Cyan, self.color);
        let name = paint("Kiln", Style::Bold, self.color);
        if subtitle.is_empty() {
            self.status(format!("{mark} {name}"));
        } else {
            self.status(format!(
                "{mark} {name} {}",
                paint(subtitle, Style::Dim, self.color)
            ));
        }
    }

    /// A section heading inside narration.
    pub fn section(&self, title: &str) {
        self.status(paint(title, Style::Bold, self.color));
    }

    /// An indented `label   value` pair, with the label padded to `width`.
    ///
    /// The label is padded *before* it is styled, so alignment counts visible
    /// characters rather than escape bytes.
    pub fn field(&self, label: &str, value: &str, width: usize) {
        let padded = format!("{label:<width$}");
        self.status(format!(
            "  {}  {}",
            paint(&padded, Style::Dim, self.color),
            value
        ));
    }

    /// A completed step.
    pub fn ok(&self, text: impl AsRef<str>) {
        self.status(format!(
            "{} {}",
            paint("✓", Style::Green, self.color),
            text.as_ref()
        ));
    }

    /// A step that needs attention but is not fatal.
    pub fn warn(&self, text: impl AsRef<str>) {
        self.status(format!(
            "{} {}",
            paint("⚠", Style::Yellow, self.color),
            text.as_ref()
        ));
    }

    /// A failed step.
    pub fn fail(&self, text: impl AsRef<str>) {
        self.status(format!(
            "{} {}",
            paint("✗", Style::Red, self.color),
            text.as_ref()
        ));
    }

    /// A de-emphasised aside.
    pub fn note(&self, text: impl AsRef<str>) {
        self.status(paint(text.as_ref(), Style::Dim, self.color));
    }

    /// Print a fully rendered error to stderr. Never suppressed by `--quiet`.
    pub fn report(&self, error: &Error) {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{}", format_error(error, self.color));

        if self.verbosity > 0 {
            let mut source = std::error::Error::source(error);
            while let Some(cause) = source {
                let _ = writeln!(
                    stderr,
                    "{}",
                    paint(&format!("  caused by: {cause}"), Style::Dim, self.color)
                );
                source = cause.source();
            }
        }
    }
}

/// Render an error in full. Pure, so the presentation can be tested.
///
/// The output answers, in order: what happened, where, why, what was expected,
/// and what to do next. Sections that carry no information are omitted rather
/// than printed empty.
pub fn format_error(error: &Error, color: bool) -> String {
    let mut out = String::new();

    out.push_str(&paint("error", Style::Red, color));
    out.push_str(&paint(":", Style::Dim, color));
    out.push(' ');
    out.push_str(&paint(error.summary(), Style::Bold, color));
    out.push('\n');

    if let Some(location) = error.location() {
        out.push('\n');
        out.push_str(&format_frame(location, color));
    }

    if let Some(reason) = error.reason() {
        out.push('\n');
        for line in reason.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }

    if let Some(expected) = error.expectation() {
        out.push('\n');
        out.push_str("  ");
        out.push_str(&paint("Expected:", Style::Bold, color));
        out.push('\n');
        for line in expected.lines() {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }

    if !error.hints().is_empty() {
        out.push('\n');
        out.push_str("  ");
        out.push_str(&paint("Try:", Style::Bold, color));
        out.push('\n');
        for hint in error.hints() {
            let bullet = paint("•", Style::Dim, color);
            let body = match hint {
                Hint::Command(command) => paint(command, Style::Cyan, color),
                Hint::Text(text) => text.clone(),
            };
            out.push_str(&format!("    {bullet} {body}\n"));
        }
    }

    out
}

/// Draw the code frame for a configuration error.
///
/// Every row shares one gutter width so the vertical bar forms a straight line
/// however many digits the line number has:
///
/// ```text
///     ┌─ kiln.toml:123:8
///     │
/// 123 │ node = "banana"
///     │        ^^^^^^^^ unsupported version requirement
/// ```
fn format_frame(location: &SourceLocation, color: bool) -> String {
    let line_number = location.line.to_string();
    let width = line_number.len();
    let blank = " ".repeat(width + 1);
    let numbered = format!("{:>width$} ", paint(&line_number, Style::Dim, color));
    let bar = paint("│", Style::Dim, color);
    let corner = paint("┌─", Style::Dim, color);

    let mut out = String::new();
    out.push_str(&format!(
        "{blank}{corner} {}\n",
        paint(&location.display_path(), Style::Dim, color)
    ));
    out.push_str(&format!("{blank}{bar}\n"));
    out.push_str(&format!("{numbered}{bar} {}\n", location.line_text));

    let pad = " ".repeat(location.column.saturating_sub(1));
    let caret = "^".repeat(location.highlight_len.max(1));
    let mut underline = format!("{blank}{bar} {pad}{}", paint(&caret, Style::Red, color));
    if let Some(label) = &location.label {
        underline.push(' ');
        underline.push_str(&paint(label, Style::Red, color));
    }
    out.push_str(&underline);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn plain_errors_render_without_escapes() {
        let error = Error::new(ErrorKind::Network, "Could not install Node.js 22.14.0")
            .because("The runtime artifact for macOS arm64 could not be found.")
            .command("kiln doctor")
            .hint("choose another supported version");

        let rendered = format_error(&error, false);
        assert!(!rendered.contains('\u{1b}'), "colour must be opt-in");
        assert!(rendered.starts_with("error: Could not install Node.js 22.14.0\n"));
        assert!(rendered.contains("  The runtime artifact for macOS arm64 could not be found."));
        assert!(rendered.contains("  Try:"));
        assert!(rendered.contains("    • kiln doctor"));
        assert!(rendered.contains("    • choose another supported version"));
    }

    #[test]
    fn empty_sections_are_omitted() {
        let rendered = format_error(&Error::internal("boom"), false);
        assert_eq!(rendered, "error: boom\n");
        assert!(!rendered.contains("Try:"));
        assert!(!rendered.contains("Expected:"));
    }

    #[test]
    fn expected_block_is_indented_under_a_heading() {
        let error = Error::config("unsupported version requirement `banana`")
            .expected("an exact version    22.14.0\na major pin         22");
        let rendered = format_error(&error, false);
        assert!(rendered.contains("  Expected:\n"));
        assert!(rendered.contains("    an exact version    22.14.0\n"));
        assert!(rendered.contains("    a major pin         22\n"));
    }

    #[test]
    fn code_frames_point_at_the_offending_token() {
        let text = "[runtime]\nnode = \"banana\"\n";
        let start = text.find("\"banana\"").unwrap();
        let location = SourceLocation::from_span(
            "/app/kiln.toml",
            text,
            start..start + 8,
            Some("unsupported version requirement".into()),
        );
        let error = Error::config("invalid kiln.toml").at(location);
        let rendered = format_error(&error, false);

        let expected_frame = "\
  ┌─ /app/kiln.toml:2:8
  │
2 │ node = \"banana\"
  │        ^^^^^^^^ unsupported version requirement
";
        assert!(
            rendered.contains(expected_frame),
            "unexpected frame:\n{rendered}"
        );
    }

    #[test]
    fn frame_gutter_stays_aligned_for_wide_line_numbers() {
        let mut text = "\n".repeat(122);
        text.push_str("node = \"banana\"\n");
        let start = text.find("\"banana\"").unwrap();
        let location =
            SourceLocation::from_span("kiln.toml", &text, start..start + 8, Some("bad".into()));
        let rendered = format_error(&Error::config("invalid kiln.toml").at(location), false);

        let expected_frame = "\
    ┌─ kiln.toml:123:8
    │
123 │ node = \"banana\"
    │        ^^^^^^^^ bad
";
        assert!(
            rendered.contains(expected_frame),
            "unexpected frame:\n{rendered}"
        );
    }

    #[test]
    fn colour_wraps_text_in_escapes() {
        let rendered = format_error(&Error::internal("boom"), true);
        assert!(rendered.contains('\u{1b}'));
        assert!(rendered.contains("boom"));
    }

    #[test]
    fn colour_choice_never_wins_over_environment() {
        assert!(!ColorChoice::Never.resolve());
        assert!(ColorChoice::Always.resolve());
    }

    #[test]
    fn paint_is_identity_without_colour() {
        assert_eq!(paint("kiln", Style::Green, false), "kiln");
        assert_eq!(paint("kiln", Style::Green, true), "\u{1b}[32mkiln\u{1b}[0m");
    }
}
