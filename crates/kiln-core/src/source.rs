//! Mapping byte offsets in a source file to a renderable location.
//!
//! Configuration errors point at the exact token that caused them. The parser
//! produces a byte span; this module turns that into line/column plus the text
//! of the offending line so the UI layer can draw a code frame:
//!
//! ```text
//!   ┌─ /home/dev/app/kiln.toml:6:8
//!   │
//! 6 │ node = "banana"
//!   │        ^^^^^^^^ unsupported version requirement
//! ```

use std::ops::Range;
use std::path::{Path, PathBuf};

/// A resolved position inside a configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    /// Path of the file the error came from.
    pub path: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number, counted in characters.
    pub column: usize,
    /// The full text of the offending line, without its terminator.
    pub line_text: String,
    /// How many characters to underline, starting at `column`. Always at least 1.
    pub highlight_len: usize,
    /// Short label rendered next to the underline.
    pub label: Option<String>,
}

impl SourceLocation {
    /// Resolve a byte `span` within `text` into a renderable location.
    ///
    /// Spans that fall outside `text` are clamped, so a parser that reports a
    /// stale offset degrades to a slightly wrong caret rather than a panic.
    pub fn from_span(
        path: impl Into<PathBuf>,
        text: &str,
        span: Range<usize>,
        label: Option<String>,
    ) -> Self {
        let start = clamp_to_char_boundary(text, span.start);
        let end = clamp_to_char_boundary(text, span.end.max(span.start));

        // Byte offset of the line containing `start`.
        let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[line_start..]
            .find('\n')
            .map_or(text.len(), |i| line_start + i);
        let line_text = text[line_start..line_end]
            .trim_end_matches('\r')
            .to_string();

        let line = text[..line_start].bytes().filter(|b| *b == b'\n').count() + 1;
        let column = text[line_start..start].chars().count() + 1;

        // Underline only the part of the span that lives on this line.
        let highlight_end = end.min(line_end);
        let highlight_len = text[start..highlight_end.max(start)].chars().count().max(1);

        SourceLocation {
            path: path.into(),
            line,
            column,
            line_text,
            highlight_len,
            label,
        }
    }

    /// `path:line:column`, the form editors and terminals can jump to.
    ///
    /// Shortened to a path relative to the working directory when the file is
    /// underneath it, the way compilers do. An absolute path in a deep temp
    /// directory can be longer than the terminal is wide, which pushes the code
    /// frame off screen and hides the thing the error is about.
    pub fn display_path(&self) -> String {
        let shown = std::env::current_dir()
            .ok()
            .and_then(|cwd| self.path.strip_prefix(&cwd).ok())
            .unwrap_or(&self.path);
        format!("{}:{}:{}", shown.display(), self.line, self.column)
    }
}

/// Move `offset` down to the nearest char boundary within `text`.
fn clamp_to_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Read a file for the sole purpose of rendering an error against it.
///
/// Returns `None` if the file cannot be read; a missing snippet must never turn
/// one error into two.
pub fn read_for_diagnostics(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[project]\nname = \"app\"\n\n[runtime]\nnode = \"banana\"\n";

    #[test]
    fn resolves_line_and_column() {
        let span_start = SAMPLE.find("\"banana\"").unwrap();
        let loc = SourceLocation::from_span(
            "/tmp/kiln.toml",
            SAMPLE,
            span_start..span_start + "\"banana\"".len(),
            Some("unsupported".into()),
        );
        assert_eq!(loc.line, 5);
        assert_eq!(loc.column, 8);
        assert_eq!(loc.line_text, "node = \"banana\"");
        assert_eq!(loc.highlight_len, 8);
        assert_eq!(loc.display_path(), "/tmp/kiln.toml:5:8");
    }

    #[test]
    fn first_line_is_line_one() {
        let loc = SourceLocation::from_span("k.toml", SAMPLE, 0..9, None);
        assert_eq!(loc.line, 1);
        assert_eq!(loc.column, 1);
        assert_eq!(loc.line_text, "[project]");
    }

    #[test]
    fn span_past_end_of_input_is_clamped() {
        let loc = SourceLocation::from_span("k.toml", SAMPLE, 9_999..10_000, None);
        assert!(loc.line >= 1);
        assert_eq!(loc.highlight_len, 1);
    }

    #[test]
    fn multibyte_lines_count_characters_not_bytes() {
        let text = "name = \"café\"\nnode = \"22\"\n";
        let start = text.find("\"22\"").unwrap();
        let loc = SourceLocation::from_span("k.toml", text, start..start + 4, None);
        assert_eq!(loc.line, 2);
        assert_eq!(loc.column, 8);
    }

    #[test]
    fn span_starting_mid_character_does_not_panic() {
        let text = "x = \"é\"\n";
        let mid = text.find('é').unwrap() + 1;
        let loc = SourceLocation::from_span("k.toml", text, mid..mid + 1, None);
        assert_eq!(loc.line, 1);
    }

    #[test]
    fn multiline_span_underlines_only_the_first_line() {
        let text = "a = 1\nb = 2\n";
        let loc = SourceLocation::from_span("k.toml", text, 0..text.len(), None);
        assert_eq!(loc.line, 1);
        assert_eq!(loc.highlight_len, "a = 1".chars().count());
    }

    #[test]
    fn carriage_returns_are_stripped_from_the_snippet() {
        let text = "a = 1\r\nb = 2\r\n";
        let start = text.find('b').unwrap();
        let loc = SourceLocation::from_span("k.toml", text, start..start + 1, None);
        assert_eq!(loc.line_text, "b = 2");
    }
}
