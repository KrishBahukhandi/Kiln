//! Asking the user a question.
//!
//! Kiln only ever asks yes-or-no questions with a safe default, and only when
//! both stdin and stderr are terminals. A tool that blocks on input in CI is a
//! tool that hangs a build at 3am.

use std::io::{BufRead, Write};

use kiln_core::Ui;
use kiln_core::error::{IoResultExt, Result};
use kiln_core::ui::{Style, paint};

/// Ask a yes/no question.
///
/// Returns `default` without asking when there is no terminal to ask through,
/// and on end-of-input.
pub fn confirm(ui: &Ui, question: &str, default: bool) -> Result<bool> {
    if !ui.is_interactive() {
        return Ok(default);
    }

    let choices = if default { "[Y/n]" } else { "[y/N]" };
    loop {
        {
            let mut stderr = std::io::stderr().lock();
            let _ = write!(
                stderr,
                "{question} {} ",
                paint(choices, Style::Dim, ui.color())
            );
            let _ = stderr.flush();
        }

        let mut answer = String::new();
        let read = std::io::stdin()
            .lock()
            .read_line(&mut answer)
            .io_context("Could not read your answer", std::path::Path::new("stdin"))?;
        if read == 0 {
            // End of input: take the default rather than looping forever.
            let _ = writeln!(std::io::stderr());
            return Ok(default);
        }

        match answer.trim().to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => ui.note("Please answer `y` or `n`."),
        }
    }
}
