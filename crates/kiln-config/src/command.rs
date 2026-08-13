//! Project commands, and the word splitting that turns them into an argv.
//!
//! Kiln does not run commands through a shell. `kiln run dev` executes the
//! program directly with the arguments below it, so nothing in `kiln.toml` can
//! become shell syntax. Cloning a repository and entering its environment stays
//! a read-only act right up until the developer asks for a command by name.
//!
//! The consequence is that `&&`, `|`, `>` and `$` have no meaning here, and a
//! manifest that uses them is rejected at parse time rather than silently doing
//! something different from what it looks like it does. Commands that genuinely
//! need shell features belong in a script the manifest can call.

use std::fmt;
use std::str::FromStr;

use kiln_core::error::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Characters that mean something to a shell and nothing to Kiln.
const SHELL_OPERATORS: &[(char, &str)] = &[
    ('|', "pipelines"),
    ('&', "background jobs and `&&`"),
    (';', "command sequences"),
    ('<', "input redirection"),
    ('>', "output redirection"),
    ('`', "command substitution"),
    ('$', "variable and command substitution"),
    ('\n', "line breaks"),
    ('\r', "line breaks"),
];

/// A command declared under `[commands]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    raw: String,
    argv: Vec<String>,
}

impl CommandSpec {
    /// Parse and validate a command string.
    pub fn parse(raw: &str) -> Result<Self> {
        let argv = split_words(raw)?;
        if argv.is_empty() {
            return Err(Error::config("the command is empty")
                .because("a command must name a program to run")
                .hint("for example: dev = \"npm run dev\""));
        }
        Ok(CommandSpec {
            raw: raw.to_string(),
            argv,
        })
    }

    /// The command exactly as written in the manifest, for display.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The program and its arguments, ready to hand to `Command::new`.
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// The program to execute.
    pub fn program(&self) -> &str {
        &self.argv[0]
    }

    /// The arguments after the program.
    pub fn args(&self) -> &[String] {
        &self.argv[1..]
    }
}

impl fmt::Display for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for CommandSpec {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        CommandSpec::parse(s)
    }
}

impl Serialize for CommandSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for CommandSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        CommandSpec::parse(&raw)
            .map_err(|e| serde::de::Error::custom(crate::parse::serde_message(&e)))
    }
}

/// Split a command line into words.
///
/// Supports the two quoting styles people expect — `'literal'` and `"escaped"` —
/// and rejects shell operators outside quotes. Inside quotes those characters
/// are ordinary data, because no shell will ever see them.
pub fn split_words(input: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut has_word = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() && c != '\n' && c != '\r' => {
                if has_word {
                    words.push(std::mem::take(&mut current));
                    has_word = false;
                }
            }
            '\'' => {
                has_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(unterminated_quote('\'')),
                    }
                }
            }
            '"' => {
                has_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            // Only the escapes a double-quoted shell word honours.
                            Some(escaped @ ('"' | '\\')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err(unterminated_quote('"')),
                        },
                        Some(c) => current.push(c),
                        None => return Err(unterminated_quote('"')),
                    }
                }
            }
            '\\' => match chars.next() {
                Some(escaped) => {
                    has_word = true;
                    current.push(escaped);
                }
                None => {
                    return Err(Error::config("the command ends with a trailing backslash")
                        .because("a backslash escapes the character after it, and there is none"));
                }
            },
            c => {
                if let Some((_, meaning)) = SHELL_OPERATORS.iter().find(|(op, _)| *op == c) {
                    return Err(shell_operator_error(input, c, meaning));
                }
                has_word = true;
                current.push(c);
            }
        }
    }

    if has_word {
        words.push(current);
    }
    Ok(words)
}

fn unterminated_quote(quote: char) -> Error {
    Error::config(format!("the command has an unterminated {quote} quote"))
        .because("every quote must be closed before the command ends")
}

fn shell_operator_error(input: &str, operator: char, meaning: &str) -> Error {
    let error = Error::config(format!("`{operator}` is not supported in a command"))
        .because(format!(
            "Kiln runs commands directly instead of through a shell, so {meaning} \
             would not do what the command says."
        ))
        .hint("quote it if you meant it literally")
        .hint("move shell logic into a script and call that instead");

    // `&&` is the overwhelmingly common case; name the fix precisely.
    if input.contains("&&") {
        error.hint("split the two halves into separate commands, e.g. `build` and `dev`")
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(input: &str) -> Vec<String> {
        CommandSpec::parse(input)
            .unwrap_or_else(|e| panic!("`{input}` should parse: {e}"))
            .argv()
            .to_vec()
    }

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(argv("npm run dev"), ["npm", "run", "dev"]);
        assert_eq!(argv("  node   --version  "), ["node", "--version"]);
    }

    #[test]
    fn keeps_quoted_words_together() {
        assert_eq!(
            argv("node -e 'console.log(1)'"),
            ["node", "-e", "console.log(1)"]
        );
        assert_eq!(argv("echo \"hello world\""), ["echo", "hello world"]);
    }

    #[test]
    fn honours_escapes_inside_double_quotes() {
        assert_eq!(argv(r#"echo "say \"hi\"""#), ["echo", r#"say "hi""#]);
        assert_eq!(argv(r#"echo "back\\slash""#), ["echo", r"back\slash"]);
    }

    #[test]
    fn single_quotes_are_literal() {
        assert_eq!(argv(r"echo 'a\b'"), ["echo", r"a\b"]);
        assert_eq!(argv("echo '$HOME'"), ["echo", "$HOME"]);
    }

    #[test]
    fn adjacent_quotes_join_into_one_word() {
        assert_eq!(argv("cmd 'a'\"b\"c"), ["cmd", "abc"]);
    }

    #[test]
    fn empty_quoted_arguments_survive() {
        assert_eq!(argv("cmd ''"), ["cmd", ""]);
        assert_eq!(argv("cmd \"\" x"), ["cmd", "", "x"]);
    }

    #[test]
    fn globs_pass_through_literally() {
        // Kiln does not expand them; the program receives the pattern.
        assert_eq!(argv("eslint src/**/*.ts"), ["eslint", "src/**/*.ts"]);
    }

    #[test]
    fn shell_operators_are_rejected() {
        for input in [
            "npm run build && npm run dev",
            "cat a | wc -l",
            "node app.js > out.log",
            "node app.js < in.txt",
            "echo `whoami`",
            "echo $HOME",
            "a; b",
            "server &",
        ] {
            assert!(
                CommandSpec::parse(input).is_err(),
                "`{input}` should be rejected"
            );
        }
    }

    #[test]
    fn the_and_and_case_suggests_splitting_the_command() {
        let err = CommandSpec::parse("npm run build && npm run dev").unwrap_err();
        assert!(err.summary().contains('&'));
        assert!(
            err.hints()
                .iter()
                .any(|h| h.text().contains("separate commands"))
        );
    }

    #[test]
    fn operators_inside_quotes_are_data() {
        assert_eq!(
            argv(r#"node -e "console.log(1|2)""#),
            ["node", "-e", "console.log(1|2)"]
        );
        assert_eq!(
            argv("git log --format='%h %s'"),
            ["git", "log", "--format=%h %s"]
        );
    }

    #[test]
    fn empty_commands_are_rejected() {
        assert!(CommandSpec::parse("").is_err());
        assert!(CommandSpec::parse("   ").is_err());
    }

    #[test]
    fn unterminated_quotes_are_rejected() {
        assert!(CommandSpec::parse("echo 'unclosed").is_err());
        assert!(CommandSpec::parse("echo \"unclosed").is_err());
        assert!(CommandSpec::parse("echo trailing\\").is_err());
    }

    #[test]
    fn program_and_args_split_correctly() {
        let spec = CommandSpec::parse("npm run dev").unwrap();
        assert_eq!(spec.program(), "npm");
        assert_eq!(spec.args(), ["run", "dev"]);
        assert_eq!(spec.raw(), "npm run dev");
    }
}
